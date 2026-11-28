//! Browse-mode painting from the hosted file's layout, theme and record page.
//! Authoring handles and part labels belong to Layout mode, not this surface.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use egui::{
    pos2, vec2, Align, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Ui, UiBuilder,
};
use om_data::calc::Value;
use om_data::choice::{self, ChoiceError, ChoiceTarget, Choices, MenuCommand};
use om_data::host::layout::{ObjectState, TextSubstitution};
use om_data::host::NO_RECORD;
use om_data::{EditRefusal, Field, FieldKind, RecordCapabilities, RenderLayout};
use om_layout::{Color, Layout, LayoutObject, ObjectKind, ResolvedStyle, TextRun};

use crate::{
    app::{steps, Action, PortalClick, ViewMode},
    calc::{Evaluator, ObjectCalcState, PortalRow, SegmentName, Visibility},
    icons::{self, Icon},
    session::{PageKey, Session},
    theme,
};

mod calendar;
mod container;
pub(crate) mod design;
pub(crate) use container::is_media;
pub use container::PdfViewer;
pub(crate) mod data_format;
mod drop_down;
mod layout_cache;
mod line;
mod options;
mod popup;
pub(crate) mod portals;
mod rich_text;
mod scaled_text;
pub(crate) mod scroll;
mod slide;
pub(crate) mod value_dialogs;
pub(crate) mod zoom;

pub(crate) use portals::enclosing as enclosing_portal;

/// The field kept active while an alert covers its editor
/// (`DocumentState::hold_focus`): its name, the object showing it, and the
/// editor (layout, record, object) that takes the focus back, when Browse
/// paints one.
struct HeldFocus {
    field: String,
    object: Option<u64>,
    editor: Option<(u64, i64, u64)>,
}

/// An Edit menu command on the field the user is typing in
/// (`DocumentState::edit_field`). Cut, Copy and Paste go to the field's
/// editor through the platform's clipboard instead
/// (`egui::ViewportCommand::RequestCopy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldEdit {
    Undo,
    Redo,
    /// Delete the selection, as Delete does, without the clipboard.
    Clear,
    SelectAll,
}

/// A field editor's selection as last drawn (`DocumentState::selection`).
struct EditorSelection {
    field: String,
    editor: egui::Id,
    range: (usize, usize),
}

/// A step's selection waiting for its field's editor
/// (`DocumentState::pending_selection`): the layout, record and field, and
/// the selection (1-based start and length; `None` after its text).
type PendingSelection = (u64, i64, String, Option<(u64, u64)>);

/// The related records portals showed in a frame, by layout, portal and
/// parent record (`DocumentState::portal_records`).
type ShownPortals = BTreeMap<(u64, u64, u64), std::sync::Arc<[u64]>>;

#[derive(Default)]
pub(crate) struct DocumentState {
    pub zoom: zoom::Zoom,
    pub scroll: scroll::Scroll,
    editor_scope: std::cell::OnceCell<egui::Id>,
    layout_cache: layout_cache::LayoutCache,
    /// Container fields' decoded pictures.
    pictures: container::Pictures,
    /// Selected panels are view state; switching records must not reset them.
    /// Reports print the panel shown here, by (layout, tab control).
    pub(crate) panels: BTreeMap<(u64, u64), u64>,
    /// The popover each layout's popover button holds open, by layout id.
    /// View state too: it shows on whichever record is current.
    popovers: BTreeMap<u64, u64>,
    /// Close Popover returns keyboard focus to its associated button in
    /// this document's next paint, in its own viewport.
    popover_button_focus: Option<(u64, u64)>,
    popover_paint: PopoverPaint,
    /// The open popover's surface, as of the last frame.
    popover_surface: Option<egui::Id>,
    pub capabilities: RecordCapabilities,
    /// The window has no calculations for this layout (no schema, or no
    /// table occurrence): hide conditions cannot be decided.
    pub calculations_unavailable: bool,
    pub pending: bool,
    pub focus: Option<String>,
    /// New Record asked for the focus in the first field: the frame that
    /// paints the new record collects its candidates (`first_candidates`),
    /// and the next one gives the focus to the winner (`resolve_first`).
    pub focus_first: bool,
    /// The enterable field objects of the current record a frame painted
    /// while `focus_first` waited: (place in the tab order, 0 outside it;
    /// paint order; layout, record, object).
    first_candidates: Vec<(u32, usize, (u64, i64, u64))>,
    /// The enterable field objects of the current record this frame
    /// painted, as `first_candidates` (place, paint order, target): the
    /// stops Tab and Shift-Tab move through (#848).
    tab_stops: Vec<TabStop>,
    /// `tab_stops` as the last frame painted them.
    painted_stops: Vec<TabStop>,
    /// A Tab or Shift-Tab taken in the last frame (`resolve_tab`): its
    /// direction, the stop that had the focus (none: no widget had it),
    /// and the input events that came after it in that frame, which go
    /// to the field it moves to.
    tab_held: Option<(bool, Option<egui::Id>, Vec<egui::Event>)>,
    /// The stop a Tab gave the focus to this frame: its focus is dropped
    /// at the frame's end if no editor took it (the field went).
    tab_target: Option<(u64, i64, u64)>,
    /// The field a Tab gives the focus to puts its insertion point after
    /// its text, selecting nothing (FileMaker Pro 26.0.3, #848), or selects
    /// a drop-down calendar's text (#964; `tab_entry_range`).
    tab_entry: bool,
    /// The related field's editor a Tab gives the focus to, which places
    /// its selection as `tab_entry` says when it draws (#859).
    related_entry: Option<egui::Id>,
    /// The field of a portal row (or a related field) a Tab goes to, by
    /// what it shows, not by its widget: the frame that draws it, from a
    /// fresh read of its portal, gives it the focus and, in the same frame,
    /// the keys typed since, which wait in `tab_goal_keys` (#859). A click,
    /// an alert, another layout or record, a cancelled focus or
    /// `TAB_GOAL_FRAMES` frames end the wait: the keys then go where the
    /// focus is.
    tab_goal: Option<(RelatedField, u64)>,
    tab_goal_keys: Vec<egui::Event>,
    /// Frames the keys have waited, counted whether or not the document
    /// is drawn (`begin_frame`).
    tab_goal_frames: u32,
    /// The widget with the keyboard focus as the last frame drew the
    /// document. egui gives a Tab pressed with no widget focused to the
    /// next widget that wants it before the document is drawn, so this
    /// tells that Tab from one pressed in another widget.
    focus_before: Option<egui::Id>,
    /// The keyboard is not the document's this frame: the window has no
    /// input, something covers it, a script runs or its dialog is up.
    pub keys_held: bool,
    pub select_all: bool,
    /// A clicked field can outlive an asynchronous save and page reload.
    /// Names alone cannot distinguish duplicate objects or List rows.
    pending_focus: Option<(u64, i64, u64)>,
    /// A click that was cancelled: its editor gives up the focus when the
    /// window next draws (in its own viewport; another window's editor
    /// keeps its focus), and in that frame.
    cancelled_focus: Option<(u64, i64, u64)>,
    surrender_focus: Option<(u64, i64, u64)>,
    /// Character position from the preview that received the first click.
    pending_cursor: Option<egui::text::CCursor>,
    pub table_current: Option<(u32, u64)>,
    list_current: Option<(u32, u64)>,
    buffers: BTreeMap<(u64, i64, String), FieldBuffer>,
    /// UI text remains a candidate until the owning operation acknowledges its
    /// accepted-page baseline. The editor/caret/IME buffer is never replaced.
    edit_preparations: BTreeMap<(u64, i64, String), CandidatePreparation>,
    next_edit_preparation: u64,
    held_edit_actions: Vec<Action>,
    held_edit_focus: Option<(Option<Focused>, Option<Focused>, Vec<Focused>)>,

    /// The field auto-complete last completed the typing in (#870): the
    /// field shows the typed letters in their own case, and when its editor
    /// is left keeps the item's own text (`FieldBuffer::accept_completion`).
    /// A record's buffers can be replaced meanwhile (a script's drafts), so
    /// it is kept here.
    completed: Option<Completion<(u64, i64, String)>>,
    /// `completed` for a related field (a portal row's).
    related_completed: Option<Completion<RelatedField>>,
    /// The user's edits of related records' fields (portal rows, related
    /// fields), by the record of the layout they were made through. They
    /// belong to that record and are committed with it.
    related: BTreeMap<RelatedField, RelatedBuffer>,
    /// The editor of a related field a click entered: it takes the click's
    /// cursor (`pending_cursor`) as it gets the focus.
    related_click: Option<egui::Id>,
    /// The portal row the user selected (clicked outside its fields):
    /// Delete Record deletes its record. A field's focus, a click on the
    /// layout or another record or layout ends it.
    pub selected_row: Option<SelectedRow>,
    /// The related records the current record deletes with its next commit
    /// (Delete Record on a portal row, Delete Portal Row), by base table and
    /// record: portals leave them out.
    pub deleted_related: BTreeSet<(u64, u64)>,
    /// The id the record a creation row begins next gets (`RelatedRow::New`).
    next_new: u64,
    /// A field of a portal row a script put the focus in: its editor takes
    /// the keyboard when the row paints.
    pub portal_focus: Option<PortalFocus>,
    /// The related field editor whose text a script's Go to Field [Select]
    /// selects as it gets the focus.
    related_select: Option<egui::Id>,
    /// The row without a record whose objects are painting (a creation
    /// row, a begun record's row): its buttons' clicks name it.
    painting_unsaved: Option<crate::app::UnsavedRow>,
    /// The portal row painting is one shown before its portal's records
    /// were read again (`portals::PortalPaintRow::stale`): a click waiting
    /// for its row waits for the rows as they are now.
    painting_stale: bool,
    /// Form canvas click left unclaimed by its interactive controls.
    empty_field_click: Option<egui::Pos2>,
    /// The related records each portal showed, by layout, portal and
    /// parent record: in this frame and in the one before. While a
    /// portal's records are read again it shows those (#536); a portal not
    /// painted for a frame is forgotten.
    shown_portals: [ShownPortals; 2],
    /// What the current record keeps uncommitted of its related records,
    /// as the window last had it: stored records a step changed, `(table,
    /// record, cells)`, and records begun through creation rows by
    /// occurrence, `(id, table, cells)`. Calculations see them, and begun
    /// records show as rows.
    pub related_drafts: RelatedDrafts,
    /// The current record's later repetitions a script set and no commit
    /// has kept: its base table, id and those cells (`related_drafts` shows
    /// them too, under the user's typing). Their editors open with them.
    pub scripted: Option<(u64, u64, crate::scripting::Cells)>,
    pub begun_rows: BegunRows,
    /// The field of the current record that has the focus, as of the last
    /// frame, and what this frame saw (object triggers).
    pub active: Option<String>,
    pub active_object: Option<u64>,
    seen_active: Option<String>,
    seen_object: Option<u64>,
    /// When `active` is a related field object's: which, and its row.
    pub active_related: Option<RelatedFocus>,
    seen_related: Option<RelatedFocus>,
    /// The related field object the focus last left (`end_frame`).
    pub left_related: Option<RelatedFocus>,
    /// The related field (and its name) a trigger of the user's edit
    /// keeps active while its portal's rows are read again and its editor
    /// is not drawn (`hold_related_focus`).
    held_related: Option<HeldRelated>,
    /// The portals whose records this frame read rather than showed as
    /// last read, by layout, portal and parent record.
    read_portals: BTreeSet<(u64, u64, u64)>,
    /// The field kept active while an alert covers it (`hold_focus`).
    held_focus: Option<HeldFocus>,
    /// The pop-up menu's Other… dialog, while it is open (#869).
    other_dialog: Option<value_dialogs::OtherDialog>,
    /// A command a menu refused this frame, alerted next frame.
    choice_refusal: Option<ChoiceError>,
    /// The value list a menu's Edit… opens, next frame (#869).
    edit_list: Option<u64>,
    /// The object the last frame painted with the focus. Unlike
    /// `active_object`, which a script's window state also sets, only
    /// painting sets it: moving between two objects of one field is a
    /// focus change of its own.
    painted_object: Option<u64>,
    /// The editor of the field that had the focus as last drawn, and its
    /// selection then: the characters from `range.0` to `range.1`, either
    /// way round (`selection`, `place_cursor`).
    editor_selection: Option<EditorSelection>,
    /// Why the field with the focus as last drawn refuses typing, by its
    /// editor (`guarded_editor`): Edit's Clear, Undo and Redo are refused
    /// there as its keys are (`edit_field`).
    entered_refusal: Option<(egui::Id, EditRefusal)>,
    /// A step's selection for a field whose editor does not have the focus
    /// yet (Set Selection into another field): its 1-based start and
    /// length, or `None` for after its text. The editor takes it as it
    /// gets the focus (`saw`).
    pending_selection: Option<PendingSelection>,
    /// Fields (and the objects showing them) the user changed this frame.
    changed: Vec<Focused>,
    /// A checkbox or radio button set change waiting for the set to be
    /// entered.
    option_wait: Option<OptionWait>,
    /// The container field the user selected. Container fields take no
    /// typing, so no editor holds the focus for them; the selection is
    /// kept here instead, and is the window's active field for scripts
    /// and the menus (which egui focus would not survive). A text field's
    /// focus, a click elsewhere on the layout, another record, layout or
    /// mode, or a script's focus elsewhere ends it.
    container: Option<ContainerSelection>,
    /// The web viewers Browse painted this frame, and what of the layout
    /// floats above them (open popovers), in window points.
    pub web_viewers: Vec<crate::web_viewer::Placement>,
    /// The pages Browse paints itself, with their newest pictures
    /// (`web_viewer::composited`), set before the window is drawn.
    pub web_pages: Vec<crate::web_viewer::PageTexture>,
    pub overlays: Vec<Rect>,
    /// Why web viewers show no page in this window, painted in their place.
    pub web_note: Option<&'static str>,
    /// Why interactive containers' audio and video do not play in the
    /// window (`web_viewer::media_note`).
    pub media_note: Option<&'static str>,
    /// The record of the List row being painted: its web viewers are views
    /// of their own (`web_viewer::Placement::record`).
    web_row: Option<i64>,
    /// A click on a button in a portal row that waited for the focused
    /// field's exit triggers, the record's save or a trigger script.
    portal_click: Option<PortalClickWait>,
    /// A click on a field in a portal row shown while its portal's records
    /// were read again: it waits to give that field the focus in the row of
    /// the record it was clicked on, once the records are in (#1099).
    field_click: Option<FieldClickWait>,
    /// Scripts run, a prompt shows or the record page loads: a waiting
    /// portal click or option set change neither goes on nor goes yet.
    pub clicks_held: bool,
    /// The button bar whose segments are painting, and its active segment.
    segment_bar: Option<(u64, ActiveSegment)>,
    /// The menu of the drop-down list whose editor has the focus, when
    /// this frame showed it open: the keyboard stays with that editor, so
    /// its keys still go to its keystroke triggers (keystrokes.rs).
    editor_list: Option<egui::Id>,
    /// Find mode's request while the layout paints as one (`show_find`).
    find: Option<FindEntry>,
    /// The Find mode controls whose lists the last frame asked for
    /// (`FindEntry::wanted`).
    find_wanted: BTreeSet<u64>,
}

/// The find request the layout shows in Find mode: its fields take the
/// request's criteria rather than a record's values.
struct FindEntry {
    request: om_data::found::FindRequest,
    /// Each field object's criterion name (`find_names`); objects left out
    /// take no criterion.
    names: BTreeMap<u64, String>,
    /// The field object to give the keyboard focus this frame.
    focus: Option<u64>,
    /// The field that had the focus this frame.
    focused: Option<String>,
    /// Return or Enter in a criterion: Perform Find.
    perform: bool,
    /// The criterion fields of the tab order drawn this frame: (place,
    /// object).
    drawn: Vec<(u32, u64)>,
    /// The enterable criteria drawn this frame, in paint order: (place,
    /// object, the widget that takes the focus), Tab's stops (#858).
    stops: Vec<(u32, u64, egui::Id)>,
    /// The value-list items read for this frame, with no record: each set's,
    /// each pop-up menu's that shows their labels, and those `wanted` the
    /// frame before.
    choices: BTreeMap<u64, std::sync::Arc<om_data::choice::Choices>>,
    /// Whether the window has calculations to read lists with.
    lists: bool,
    /// The pop-up menus and drop-down lists open this frame, whose lists
    /// are read for the next.
    wanted: BTreeSet<u64>,
    /// The file's date format and today, for drop-down calendars; `None`
    /// without calculations.
    dates: Option<(om_data::calc::Locale, om_data::calc::Date)>,
}

/// Stored related records a step changed: `(table, record, cells)`.
pub(crate) type RelatedDrafts = Vec<(u64, u64, crate::scripting::Cells)>;
/// Records begun through creation rows, by occurrence: `(id, table, cells)`.
pub(crate) type BegunRows = BTreeMap<u64, Vec<(u64, u64, crate::scripting::Cells)>>;

/// A selected container field object: its layout, record and object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContainerSelection {
    pub layout: u64,
    pub record: i64,
    pub object: u64,
    pub field: String,
    /// A related container's portal row (`field` is then
    /// `Occurrence::Field`): the Insert menu fills that row's record.
    pub row: Option<ContainerRow>,
}

impl ContainerSelection {
    /// Whether its field object lets the user change the field in Browse
    /// mode: "Field entry behavior" Edit. View Only and Select Only
    /// prevent edits through the object (Claris Help, "Allowing or
    /// preventing entry into fields"), so the Insert menu does not fill it.
    pub(crate) fn allows_entry(&self, session: &Session) -> bool {
        let crate::session::RenderState::Loaded(render) = &session.render else {
            return false;
        };
        render
            .layout
            .objects
            .get(self.object)
            .and_then(|object| object.field_obj.as_ref())
            .is_some_and(|field| field.field_entry(false) == om_layout::FieldEntry::Edit)
    }

    pub(crate) fn is_interactive(&self, session: &Session) -> bool {
        let crate::session::RenderState::Loaded(render) = &session.render else {
            return false;
        };
        self.row.is_none()
            && render
                .layout
                .objects
                .get(self.object)
                .is_some_and(container::is_interactive)
    }
}

/// The portal row of a selected related container: of portal `portal`
/// (of `occurrence`), row `number`, showing `record`, or no record
/// (`NO_RECORD`) for the row `unsaved` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ContainerRow {
    pub portal: u64,
    pub occurrence: u64,
    pub record: u64,
    pub number: usize,
    pub unsaved: Option<crate::app::UnsavedRow>,
}

/// A related field a trigger of the user's edit keeps active
/// (`DocumentState::hold_related_focus`).
#[derive(Clone, Debug)]
struct HeldRelated {
    name: String,
    focus: RelatedFocus,
    /// The script ended: the editor showing the field's record takes the
    /// focus back, wherever its row is now.
    released: bool,
    /// An editor showing it was drawn this frame.
    seen: bool,
}

/// The related field object whose editor has the user's focus on the
/// current record (`related_field`): its object triggers run as the
/// layout's own fields' do, with its portal row active (#1005).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RelatedFocus {
    pub object: u64,
    pub field: RelatedField,
    /// The portal it is in, its occurrence, and the row (from 1); `None`
    /// for a related field outside portals.
    pub portal: Option<u64>,
    pub occurrence: u64,
    pub row: Option<u64>,
    pub repetition: u32,
}

/// A button bar's active segment as Browse shows it (layout-model.md 5.5):
/// in the theme's active state, and selected for accessibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveSegment {
    None,
    Segment(u64),
    /// OpenMaker cannot tell which segment FileMaker makes active, and
    /// why: none shows active, and the bar says so rather than guessing.
    Undetermined(&'static str),
}

/// The active segment of button bar `bar` on its record. `name` is what
/// its calculation gave there (`None` while its data is fetched, or
/// without calculations: `unavailable`). A calculated bar in a portal row
/// is not evaluated.
fn active_segment(
    layout: &Layout,
    bar: &LayoutObject,
    name: Option<&SegmentName>,
    in_portal: bool,
    unavailable: bool,
) -> ActiveSegment {
    use om_data::host::layout_objects::{named_segment, segment_choice, SegmentChoice};
    match segment_choice(bar) {
        SegmentChoice::Fixed(key) => key.map_or(ActiveSegment::None, ActiveSegment::Segment),
        SegmentChoice::Undetermined(why) => ActiveSegment::Undetermined(why),
        SegmentChoice::Calculated(_) if in_portal => ActiveSegment::Undetermined(
            "its active segment is calculated, which OpenMaker does not evaluate in portal rows",
        ),
        SegmentChoice::Calculated(_) => match name {
            Some(SegmentName::Name(name)) => {
                named_segment(layout, bar, name).map_or(ActiveSegment::None, ActiveSegment::Segment)
            }
            Some(SegmentName::Unresolved) => ActiveSegment::Undetermined(
                "its active segment calculation needs data, account details or functions \
                 this window does not have",
            ),
            None if unavailable => {
                ActiveSegment::Undetermined("This window has no calculations for the layout")
            }
            // Its data is on the way: no segment is active until it comes.
            None => ActiveSegment::None,
        },
    }
}

/// A bar whose active segment is undetermined: a dashed outline round it
/// and the reason on hover, under its segments, which keep their clicks.
fn undetermined_segment(ui: &Ui, rect: Rect, id: egui::Id, why: &str) {
    let outline = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    ui.painter().extend(egui::Shape::dashed_line(
        &outline,
        Stroke::new(1.0, theme::DISABLED),
        4.0,
        3.0,
    ));
    let note = format!("OpenMaker cannot tell which segment of this button bar is active: {why}.");
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Group, true, &note));
    response.on_hover_text(note);
}

/// A held portal row click goes on when its button paints again, shown,
/// in the row of the same related record of the same portal on the same
/// record and layout, at that row's number then: never by the row's old
/// position. Rows, a hide condition or a page still loading keep it
/// waiting; a frame that paints none of that (the row or its button gone,
/// hidden or undecidable, another layout, record or mode) drops it.
#[derive(Clone, Copy, Debug)]
struct PortalClickWait {
    object: u64,
    click: PortalClick,
    /// The click made its row's record current first (List view): it goes
    /// unless that record is current.
    moved: bool,
    /// This frame painted its button's row or could not tell yet.
    seen: bool,
}

/// The field the user clicked in a row shown while its portal's records
/// were read again: field `field` (its record's, by identity, not its
/// row's number) shown by object `object`.
#[derive(Clone, Copy, Debug)]
struct FieldClickWait {
    field: RelatedField,
    object: u64,
    /// This frame painted its field's row or could not tell yet.
    seen: bool,
}

/// A field with the focus or edited, and the layout object showing it
/// (`None` in the generic views, which show no layout).
pub(crate) type Focused = (String, Option<u64>);

/// A change the user made to a checkbox or radio button set
/// (`options.rs`) that waits for the set to be entered: the exit triggers
/// of the field the focus left (which can refuse it), the record made
/// current and loaded, the set's own OnObjectEnter. It is made only on the
/// layout, record and object it was made on, while the set is shown, still
/// editable and has the focus; a veto or context change
/// ([`DocumentState::cancel_pending_focus`]), or a frame that paints none
/// of it while nothing holds it, drops it.
#[derive(Clone, Debug)]
struct OptionWait {
    layout: u64,
    record: i64,
    object: u64,
    change: OptionChange,
    /// Made this frame: the record it goes to may become current only
    /// when the frame's actions are applied.
    fresh: bool,
    seen: bool,
}

#[derive(Clone, Debug)]
enum OptionChange {
    /// Pick the item storing this value, Shift held or not.
    Pick { value: String, shift: bool },
    /// A key the set's keystroke trigger let through.
    Key(String),
    /// Open a pop-up menu's menu.
    Open,
    /// Enter the text Other… was given (`choice::other_value`).
    Other(String),
}

/// An unsaved edit of one field: the field's name, the value the edit
/// started from, and the edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldDraft {
    pub name: String,
    pub original: String,
    pub value: String,
}

/// A field of a related record the user edits in Browse, reached from
/// record `parent` of `layout`: field `field` of row `row`, a record of
/// base table `table`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RelatedField {
    pub layout: u64,
    pub parent: i64,
    pub table: u64,
    pub row: RelatedRow,
    pub field: u64,
}

/// Field object `field` in row `row` (from 1) of its portal on record
/// `parent` of `layout`, which a step put the focus in; `select`: its text
/// selected (Go to Field [Select/perform]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PortalFocus {
    pub layout: u64,
    pub parent: i64,
    pub row: u64,
    pub field: u64,
    pub select: bool,
}

/// A portal row the user selected: of portal `portal` (of `occurrence`,
/// base table `table`) on record `parent` of `layout`, showing `record` as
/// row `number`. `deletes`: the portal allows deleting its records
/// (`kAllowDelete`, layout-model.md 5.3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SelectedRow {
    pub layout: u64,
    pub parent: i64,
    pub portal: u64,
    pub occurrence: u64,
    pub table: u64,
    pub record: u64,
    pub number: usize,
    pub deletes: bool,
}

/// "Allow deletion of portal records" (`PortalLayoutObject.portalFlags`).
const PORTAL_ALLOWS_DELETE: i32 = 0x4;

/// The related record a related edit is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RelatedRow {
    /// A stored record.
    Record(u64),
    /// A record the user began in the creation row of a portal of
    /// `occurrence`, created when the parent is committed. `id` is the
    /// window's for it (`RelatedDraft::begun`); begun rows show in its
    /// order.
    New { occurrence: u64, id: u64 },
}

/// A related field's edit: the occurrence the record was reached through,
/// the record's modification count as the window showed it when the
/// edit began, and the edit.
struct RelatedBuffer {
    occurrence: u64,
    version: Option<u64>,
    buffer: FieldBuffer,
    /// What a script left in the cell, uncommitted, as the edit began or
    /// last took it (`DocumentState::scripted_value`). The editor opens
    /// with it, and the user's edit is a change against it, not against
    /// the stored value: clearing the box back to the stored value
    /// replaces the script's too.
    scripted: Option<String>,
}

impl RelatedBuffer {
    fn new(occurrence: u64, version: Option<u64>, stored: &str, scripted: Option<String>) -> Self {
        let mut buffer = FieldBuffer::new(stored);
        if let Some(value) = &scripted {
            buffer.value.clone_from(value);
        }
        Self {
            occurrence,
            version,
            buffer,
            scripted,
        }
    }

    /// Whether the user changed a stored record's value: from the script's,
    /// if one is left, else from the stored one.
    fn changed(&self) -> bool {
        self.buffer.value != *self.scripted.as_ref().unwrap_or(&self.buffer.original)
    }

    /// The record was read again: an unchanged edit takes its value (the
    /// script's, if one is left) and version; a changed one keeps the
    /// version it began from.
    fn refresh(&mut self, target: &crate::calc::RelatedRecord, scripted: Option<String>) {
        if self.changed() {
            // An override of a scripted draft can equal the stored text.
            // Keep its edit and the original text/version together.
            if self.scripted.is_none() {
                self.buffer.refresh(&target.stored);
                if self.buffer.value == self.buffer.original {
                    self.version = target.version;
                }
            }
        } else {
            self.buffer.value.clone_from(&self.buffer.original);
            self.buffer.refresh(&target.stored);
            if let Some(value) = &scripted {
                self.buffer.value.clone_from(value);
            }
            self.scripted = scripted;
            self.version = target.version;
        }
    }
}

/// A related field's unsaved value ([`DocumentState::related_edits`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelatedEdit {
    pub field: RelatedField,
    pub occurrence: u64,
    pub version: Option<u64>,
    pub value: String,
    /// The field's stored text when the edit began (a reconnect compares
    /// the host's with it).
    pub original: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidatePreparation {
    Pending(u64),
    Accepted,
    Failed,
}

struct FieldBuffer {
    original: String,
    value: String,
    /// Real UI input changed this buffer; worker-supplied drafts do not
    /// acquire a UI edit baseline until the user changes them.
    ui_edited: bool,
    /// This baseline is newer than the retained record page.
    refreshed: bool,
    retained_page: Option<PageKey>,
    accepted_version: Option<u64>,
}
impl FieldBuffer {
    fn new(value: &str) -> Self {
        Self {
            original: value.into(),
            value: value.into(),
            ui_edited: false,
            refreshed: false,
            retained_page: None,
            accepted_version: None,
        }
    }

    /// Its editor was left after `completion`: a value still exactly the
    /// completed text becomes the item's own, as FileMaker Pro 26.0.3
    /// stores "Acme" for a completed "acme" (#870's reading). A value
    /// changed since (typing on, Backspace, a script's draft) is kept.
    fn accept_completion<K>(&mut self, completion: &Completion<K>) {
        if self.value == completion.text && self.value != completion.item {
            self.value.clone_from(&completion.item);
            self.ui_edited = true;
        }
    }
    fn remember_version(&mut self, session: &Session, index: u64) {
        // Called before widgets process this frame's input. A dirty candidate
        // keeps the page version that supplied its original, across new pages.
        if !self.ui_edited && self.value == self.original {
            self.accepted_version = session.record_version_at(index);
        }
    }
    fn refresh(&mut self, server: &str) {
        self.refreshed = false;
        self.retained_page = None;
        if self.value == self.original || self.value == server {
            self.original = server.into();
            self.value = server.into();
            self.ui_edited = false;
        }
    }
    fn refresh_page(&mut self, server: &str, page: Option<PageKey>) {
        if self.refreshed {
            // Duplicate objects share this buffer. Only an accepted newer
            // record page may supersede a targeted/worker original.
            if self.retained_page.is_none() {
                self.retained_page = page;
            }
            if self.retained_page == page {
                return;
            }
        }
        let clean = self.value == self.original;
        self.refresh(server);
        if clean {
            self.retained_page = page;
        }
    }
    fn refresh_targeted(&mut self, server: &str, page: Option<PageKey>) {
        // An evaluation overlays real drafts; that overlay must never become
        // their stored original merely because its value matches the draft.
        if self.value == self.original {
            self.refresh(server);
            self.refreshed = true;
            self.retained_page = page;
        }
    }
}

/// An auto-complete in progress (#870): the field, the editor it happened
/// in, the text the completion left there, and the item it completed to.
#[derive(Debug)]
struct Completion<K> {
    key: K,
    editor: egui::Id,
    text: String,
    item: String,
}

/// Where the open popover paints (layout-model.md 5.3).
#[derive(Default)]
enum PopoverPaint {
    /// In stacking order: List view rows, and the raised paint itself.
    #[default]
    InPlace,
    /// Form view: the traversal notes where it reaches the popover through
    /// visible containers, to paint it above the layout in that context.
    Defer(Option<RaisedPopover>),
}

#[derive(Clone, Copy)]
struct RaisedPopover {
    id: u64,
    origin: egui::Pos2,
    growth: egui::Vec2,
    record_index: u64,
    portal: Option<PortalRow>,
}

impl DocumentState {
    pub(crate) fn design_surface<'a>(
        &'a mut self,
        session: &'a Session,
        schema: Option<&'a om_data::host::CalcSchema>,
    ) -> (Option<design::Surface<'a>>, &'a mut scroll::Scroll) {
        (
            design::Surface::new(session, &self.layout_cache, schema),
            &mut self.scroll,
        )
    }

    /// Capture the accepted page identity while text is only an input candidate.
    pub(crate) fn prepare_candidates(
        &mut self,
        ordinary_existing: bool,
    ) -> Vec<(u64, om_data::RecordEditRequest)> {
        let mut requests = Vec::new();
        for ((layout, record, name), buffer) in &self.buffers {
            if !buffer.ui_edited || buffer.value == buffer.original {
                continue;
            }
            let key = (*layout, *record, name.clone());
            if self.edit_preparations.contains_key(&key) {
                continue;
            }
            if !ordinary_existing
                || !self.capabilities.prepare_edit
                || self.capabilities.globals.contains(name)
            {
                self.edit_preparations
                    .insert(key, CandidatePreparation::Accepted);
                continue;
            }
            let Ok(layout) = u32::try_from(*layout) else {
                continue;
            };
            // The widget retained this version before its first changed input.
            let version = buffer.accepted_version;
            self.next_edit_preparation = self.next_edit_preparation.wrapping_add(1);
            let token = self.next_edit_preparation;
            self.edit_preparations
                .insert(key, CandidatePreparation::Pending(token));
            requests.push((
                token,
                om_data::RecordEditRequest {
                    baseline: om_data::ContainerEditBaseline {
                        layout,
                        record: *record,
                        version,
                    },
                    cells: Default::default(),
                    fields: [name.clone()].into_iter().collect(),
                },
            ));
        }
        requests
    }

    pub(crate) fn accept_edit_preparation(
        &mut self,
        token: u64,
        request: &om_data::RecordEditRequest,
        success: bool,
    ) -> bool {
        let mut accepted = false;
        for name in &request.fields {
            let key = (
                u64::from(request.baseline.layout),
                request.baseline.record,
                name.clone(),
            );
            if let Some(prepared) = self.edit_preparations.get_mut(&key) {
                if *prepared != CandidatePreparation::Pending(token) {
                    continue;
                }
                accepted = true;
                *prepared = if success {
                    CandidatePreparation::Accepted
                } else {
                    CandidatePreparation::Failed
                };
            }
        }
        if accepted && !success {
            self.held_edit_actions.clear();
        }
        accepted
    }

    /// The window released `record` of `layout` (it keeps no edit of it):
    /// the lock a preparation took goes with it, so a reply to one still
    /// on its way is not this window's any more, and an entry begun again
    /// prepares again. Refused input stays to be discarded.
    pub(crate) fn forget_edit_preparations(&mut self, layout: u32, record: i64) {
        self.edit_preparations.retain(|(l, r, _), p| {
            *l != u64::from(layout) || *r != record || *p == CandidatePreparation::Failed
        });
    }

    /// Retention starts with real input, even while preparation has not
    /// accepted its value. This pins the page the user actually edited.
    pub(crate) fn entry_candidate_records(&self) -> BTreeSet<(u32, i64)> {
        self.buffers
            .iter()
            .filter_map(|((layout, record, _), buffer)| {
                if !buffer.ui_edited || buffer.value == buffer.original {
                    return None;
                }
                Some((u32::try_from(*layout).ok()?, *record))
            })
            .collect()
    }

    pub(crate) fn failed_edit(&self) -> bool {
        self.edit_preparations
            .values()
            .any(|p| *p == CandidatePreparation::Failed)
    }

    /// Explicitly discard only input the provider refused to accept. Other
    /// accepted edits and provider-owned attachments remain untouched.
    pub(crate) fn discard_failed_edits(&mut self) {
        self.buffers.retain(|key, _| {
            self.edit_preparations.get(key) != Some(&CandidatePreparation::Failed)
        });
        self.edit_preparations
            .retain(|_, p| *p != CandidatePreparation::Failed);
        self.held_edit_actions.clear();
        self.held_edit_focus = None;
        self.cancel_pending_focus();
    }

    pub(crate) fn preparing_edit(&self) -> bool {
        self.edit_preparations
            .values()
            .any(|p| matches!(p, CandidatePreparation::Pending(_)))
    }

    fn accepted_buffer(&self, key: &(u64, i64, String), buffer: &FieldBuffer) -> bool {
        !buffer.ui_edited
            || match self.edit_preparations.get(key) {
                Some(CandidatePreparation::Accepted) => true,
                Some(CandidatePreparation::Pending(_) | CandidatePreparation::Failed) => false,
                None => !self.capabilities.prepare_edit,
            }
    }

    /// Keep field-editing commands in the field. Revert also takes the
    /// active field so its script step can run the discard triggers, and
    /// Relookup Field Contents as its target (Claris's help: select the
    /// match field, then choose the command). Other menu commands
    /// surrender focus and wait for the exit triggers.
    pub(crate) fn menu_leaves_field(&mut self, ui: &Ui, chosen: &mut Vec<Action>) {
        // A catalog read is background work, even when opening a menu
        // requested it. It does not own a field exit or wait for one.
        if chosen.iter().all(|action| *action == Action::ReadScripts) {
            return;
        }
        let preparing = self.preparing_edit() || self.failed_edit();
        if self
            .held_edit_actions
            .iter()
            .any(|action| matches!(action, Action::MenuFieldExit(_)))
        {
            // The first command owns the pending exit. Close remains
            // available to cancel the pending edit and close the file.
            chosen.retain(|action| {
                *action == Action::ReadScripts || (preparing && *action == Action::CloseFile)
            });
            return;
        }
        let Some(action) = chosen.iter_mut().find(|action| match action {
            #[cfg(target_os = "macos")]
            Action::NativeEditField(..) => false,
            Action::EditField(_) | Action::ReadScripts => false,
            Action::Step(step, _) | Action::FieldMenuStep(step, _, _) => !matches!(
                *step,
                steps::INSERT_CURRENT_DATE
                    | steps::INSERT_CURRENT_TIME
                    | steps::INSERT_CURRENT_USER_NAME
                    | steps::INSERT_FROM_INDEX
                    | steps::INSERT_FROM_LAST_VISITED
                    | om_data::scripts::revert_record::STEP_ID
                    | om_data::scripts::relookup::STEP_ID
            ),
            Action::CloseFile if preparing => false,
            _ => true,
        }) else {
            return;
        };
        if self.active.is_none() {
            return;
        }
        ui.memory_mut(|memory| {
            if let Some(editor) = memory.focused() {
                memory.surrender_focus(editor);
            }
        });
        *action = Action::MenuFieldExit(Box::new(action.clone()));
    }

    pub(crate) fn hold_candidate_actions(
        &mut self,
        left: &mut Option<Focused>,
        entered: &mut Option<Focused>,
        modified: &mut Vec<Focused>,
        actions: &mut Vec<Action>,
    ) {
        let waiting = self.edit_preparations.iter().any(|(key, state)| {
            *state == CandidatePreparation::Failed
                || (*state != CandidatePreparation::Accepted
                    && self.buffers.get(key).is_some_and(|b| b.value != b.original))
        });
        if waiting {
            let mut retained = Vec::new();
            let mut cancellation = Vec::new();
            for action in actions.drain(..) {
                if matches!(
                    action,
                    Action::ReadScripts
                        | Action::RevertRecord
                        | Action::CloseFile
                        | Action::DismissAlert
                        | Action::DismissHostMessage
                        | Action::CloseDisconnectedFile(_)
                        | Action::DiscardEntryCandidates
                ) {
                    cancellation.push(action);
                } else if matches!(
                    action,
                    Action::Step(om_data::scripts::relookup::STEP_ID, _)
                        | Action::FieldMenuStep(om_data::scripts::relookup::STEP_ID, _, _)
                ) {
                    // Relookup acts on the field entered when it was
                    // chosen, and the record had something to commit: not
                    // held for later, when the focus may have moved.
                } else {
                    retained.push(action);
                }
            }
            *actions = cancellation;
            self.held_edit_actions.append(&mut retained);
            let held = self
                .held_edit_focus
                .get_or_insert_with(|| (left.clone(), entered.clone(), Vec::new()));
            if held.0.is_none() {
                held.0 = left.clone();
            }
            if entered.is_some() {
                held.1 = entered.clone();
            }
            for field in modified.drain(..) {
                if !held.2.contains(&field) {
                    held.2.push(field);
                }
            }
            *left = None;
            *entered = None;
        } else {
            if let Some((l, e, mut m)) = self.held_edit_focus.take() {
                if left.is_none() {
                    *left = l;
                }
                if entered.is_none() {
                    *entered = e;
                }
                m.append(modified);
                *modified = m;
            }
            let mut held = std::mem::take(&mut self.held_edit_actions);
            held.append(actions);
            *actions = held;
        }
    }

    /// The document of window `window`: its record editors' ids are its
    /// own, as a card shows the same records as its parent in one
    /// viewport.
    pub(crate) fn for_window(window: u32) -> Self {
        let state = Self::default();
        let _ = state
            .editor_scope
            .set(egui::Id::unique(("record_editors", window)));
        state
    }

    pub fn popovers(&self) -> &BTreeMap<u64, u64> {
        &self.popovers
    }

    pub fn cancel_popover_focus(&mut self) {
        self.popover_button_focus = None;
    }

    #[cfg(test)]
    pub(crate) fn hold_popover(&mut self, layout: u64, popover: u64) {
        self.popovers.insert(layout, popover);
    }

    /// Shared dismissal for the button toggle and the script step. Drafts
    /// belong to the record and survive closing its popover.
    pub fn close_popover(&mut self, layout: u64, popover: u64, button: Option<u64>) -> bool {
        if self.popovers.get(&layout) != Some(&popover) {
            return false;
        }
        self.popovers.remove(&layout);
        // Dismissing the field's surface ends container focus just as it
        // ends text-editor focus. Menus alone still retain the selection.
        self.container = None;
        self.popover_surface = None;
        self.popover_paint = PopoverPaint::default();
        self.cancel_pending_focus();
        self.focus = None;
        self.focus_first = false;
        self.select_all = false;
        self.popover_button_focus = button.map(|button| (layout, button));
        true
    }

    pub fn clear_layout_cache(&self) {
        self.layout_cache.clear();
    }

    /// An enterable field object of the current record that this frame
    /// painted while New Record's focus waits (`focus_first`).
    fn note_first_candidate(
        &mut self,
        object: &LayoutObject,
        target: (u64, i64, u64),
        widget: egui::Id,
    ) {
        let enters = object
            .field_obj
            .as_ref()
            .is_some_and(|field| field.field_entry(false) == om_layout::FieldEntry::Edit);
        if enters {
            self.note_tab_stop(object.tab_order, target, widget, true);
        }
        if self.focus_first && enters {
            let order = self.first_candidates.len();
            self.first_candidates
                .push((object.tab_order, order, target));
        }
    }

    /// A related field object of the current record, on `parent`, that
    /// this frame painted (#859): in portal row `row`, or on the layout.
    fn note_related_stop(
        &mut self,
        (layout, object): (&Layout, &LayoutObject),
        field: RelatedField,
        row: Option<u64>,
        widget: egui::Id,
    ) {
        let enters = object
            .field_obj
            .as_ref()
            .is_some_and(|field| field.field_entry(false) == om_layout::FieldEntry::Edit);
        // Also while the portal's rows are read again, so that the row with
        // the keyboard is one; a Tab enters a row only once it is fresh
        // (`tab_goal`).
        if enters {
            let order = self.tab_stops.len();
            self.tab_stops.push(TabStop {
                place: object.tab_order,
                order,
                target: (layout.id, field.parent, object.id),
                widget,
                row: portals::enclosing(layout, object.id).zip(row),
                key: Some((field, object.id)),
                object: false,
                departure_only: false,
            });
        }
    }

    /// Find mode's criterion of `column` in the list Table view and an
    /// undrawn layout show (`content::find`) is a Tab stop, in column
    /// order; whether Tab has just entered it, its insertion point to go
    /// after its text.
    pub(crate) fn find_cell_tab(&mut self, layout: u64, column: usize) -> bool {
        let target = (layout, FIND_REQUEST, TABLE_CELL | column as u64);
        self.tab_target == Some(target) && std::mem::take(&mut self.tab_entry)
    }

    /// [`Self::find_cell_tab`]'s criterion was drawn with `widget`.
    pub(crate) fn note_find_cell(&mut self, layout: u64, column: usize, widget: egui::Id) {
        let target = (layout, FIND_REQUEST, TABLE_CELL | column as u64);
        self.note_tab_stop(0, target, widget, false);
    }

    /// A field Tab can move the focus to this frame, at `place` in the
    /// layout's tab order (0 outside it). `object`: a field object of the
    /// layout, whose editor takes the focus through `pending_focus`; a Find
    /// criterion or a Table view cell takes it directly.
    fn note_tab_stop(
        &mut self,
        place: u32,
        target: (u64, i64, u64),
        widget: egui::Id,
        object: bool,
    ) {
        let order = self.tab_stops.len();
        self.tab_stops.push(TabStop {
            place,
            order,
            target,
            widget,
            object,
            row: None,
            key: None,
            departure_only: false,
        });
    }

    /// New Record's focus (calc-functions.md 6.1, s33: Title on the
    /// Contacts starter's Contact Details): of the field objects the last
    /// frame painted for the new record, the first of the layout's stored
    /// tab order, as a script's New Record gives it
    /// (`layout_objects::first_in_tab_order`); with none in the tab order,
    /// the first painted. The focus goes to that object, not to its field's
    /// name, so a field placed twice, or hidden, cannot take it instead.
    fn resolve_first(&mut self) {
        let candidates = std::mem::take(&mut self.first_candidates);
        if !self.focus_first {
            return;
        }
        if let Some(&(_, _, target)) = candidates
            .iter()
            .min_by_key(|(place, order, _)| (*place == 0, *place, *order))
        {
            self.pending_focus = Some(target);
            self.focus_first = false;
        }
    }

    /// Tab or Shift-Tab in Browse (#848): the focus goes to the next, or
    /// the previous, field object of the current record in the layout's
    /// stored tab order, its insertion point after its text; in Find mode,
    /// to the request's criteria in the same order (`find_field`, #858).
    /// Stops go by place, then paint order ([`Self::resolve_first`]'s rule); objects
    /// outside the order (place 0) are no stops while any object is in it,
    /// and on a layout with none in it the paint order serves. Past the
    /// last stop Tab goes to the first, and Shift-Tab before the first to
    /// the last; in List view it stays on the current record (FileMaker Pro
    /// 26.0.3 on the Contacts starter, #848, which also stops on buttons,
    /// tab controls, web viewers and containers; these are passed over
    /// here). With a field's list open, Tab does nothing (observed for a
    /// pop-up menu). Inferred: from a field outside the order, or with no
    /// widget focused, Tab goes to the first and Shift-Tab to the last.
    ///
    /// It takes two frames, so that each key reaches one field. In the
    /// frame of the Tab the field that has the focus keeps the keys typed
    /// before it, and those after it wait. At the start of the next frame,
    /// before any field is drawn, the field the Tab goes to (found among
    /// the stops that frame painted, on the current layout and record)
    /// takes the focus, its content is selected, and the keys that waited
    /// come to it. The field left keeps its typing in the record's draft,
    /// as when a click moves the focus. A Tab is the document's only when
    /// it has the keyboard (`keys_held`) and the widget with the focus is
    /// one of its stops, or no widget has it.
    /// `find`: this frame draws Find mode, whose stops are the request's
    /// criteria; a held Tab only moves to a stop of the frame's mode.
    pub(crate) fn resolve_tab(&mut self, ui: &Ui, session: &Session, find: bool) {
        let mut moved = false;
        // A portal row's field a Tab aims at took the focus in the last
        // frame: the keys that waited come to it now. Until it does, the
        // keys typed wait too; if it does not come (its row left the
        // portal, an alert came up), they go where the focus is.
        if let Some((field, _)) = self.tab_goal {
            let ended = self.keys_held
                || self.tab_goal_frames > TAB_GOAL_FRAMES
                || ui.input(|input| input.pointer.any_pressed())
                || session.layout.map(u64::from) != Some(field.layout)
                || session.current_record().map(|(_, record)| record.id) != Some(field.parent);
            if ended {
                self.tab_goal = None;
            } else {
                let typed = take_typing(ui);
                self.tab_goal_keys.extend(typed);
                ui.ctx().request_repaint();
                return;
            }
        }
        if !self.tab_goal_keys.is_empty() {
            replay(ui, std::mem::take(&mut self.tab_goal_keys));
        }
        // The Tab held from the last frame moves the focus, unless the
        // keyboard stopped being the document's since (an alert came up)
        // or the user clicked: then its keys go back as they came, to
        // whoever has them now.
        if let Some((backward, from, held)) = self.tab_held.take() {
            let clicked = ui.input(|input| input.pointer.any_pressed());
            let to = if !self.keys_held && !self.pending && !clicked {
                self.finish_tab(ui, session, backward, from, find)
            } else {
                TabMove::Stays
            };
            match to {
                TabMove::Moved => {
                    moved = true;
                    replay(ui, held);
                }
                // The field takes the focus as it draws: this frame's keys
                // wait with the held ones.
                TabMove::Aimed => {
                    let typed = take_typing(ui);
                    self.tab_goal_keys = held;
                    self.tab_goal_keys.extend(typed);
                    ui.ctx().request_repaint();
                    return;
                }
                TabMove::Stays => replay(ui, held),
            }
        }
        let now = ui.memory(|memory| memory.focused());
        let before = std::mem::replace(&mut self.focus_before, now);
        // A Tab among the keys that came back starts from the field the
        // held one just moved to, not from where the last frame left it.
        let before = if moved { now } else { before };
        if self.keys_held || self.pending {
            return;
        }
        let Some((at, backward)) = tab_pressed(ui) else {
            return;
        };
        let stop = |id: egui::Id| self.painted_stops.iter().any(|stop| stop.widget == id);
        let from = match (before, now) {
            (Some(id), _) if stop(id) => Some(id),
            // Another widget's Tab: egui's own traversal.
            (Some(_), _) => return,
            (None, Some(id)) if stop(id) => Some(id),
            // No widget had the focus: the first field takes it (from the
            // widget egui gave it to this frame).
            (None, _) => None,
        };
        // A field's open list keeps the focus and its item.
        if from.is_some_and(|id| egui::Popup::is_id_open(ui.ctx(), popup::menu_id(id))) {
            return;
        }
        let held = ui.ctx().input_mut(|input| input.events.split_off(at + 1));
        self.tab_held = Some((backward, from, held));
        ui.ctx().request_repaint();
    }

    pub(crate) fn tab_in_progress(&self) -> bool {
        self.tab_held.is_some() || self.tab_goal.is_some()
    }

    /// The second frame of [`Self::resolve_tab`]: `from`, the stop that had
    /// the focus when Tab was pressed, gives it to the next stop of those
    /// the last frame painted.
    fn finish_tab(
        &mut self,
        ui: &Ui,
        session: &Session,
        backward: bool,
        from: Option<egui::Id>,
        find: bool,
    ) -> TabMove {
        // A stop Tab only leaves from is not among them: from it, Tab goes
        // to the first (Shift-Tab: the last).
        let mut stops: Vec<&TabStop> = self
            .painted_stops
            .iter()
            .filter(|stop| !stop.departure_only)
            .collect();
        if stops.iter().any(|stop| stop.place > 0) {
            stops.retain(|stop| stop.place > 0);
        }
        // A portal's rows go together, at the first place (then the first
        // paint order) its fields hold, row by row, each row's fields by
        // place: as FileMaker Pro 26.0.3 goes through the Tasks starter's
        // attachment portal (#859).
        let group = |stop: &TabStop| match stop.row {
            None => (stop.place, stop.order),
            Some((portal, _)) => stops
                .iter()
                .filter(|other| other.row.is_some_and(|(p, _)| p == portal))
                .map(|other| (other.place, other.order))
                .min()
                .unwrap_or((stop.place, stop.order)),
        };
        let mut keyed: Vec<_> = stops
            .iter()
            .map(|stop| {
                let row = stop.row.map_or(0, |(_, row)| row);
                ((group(stop), row, stop.place, stop.order), *stop)
            })
            .collect();
        keyed.sort_by_key(|(key, _)| *key);
        let stops: Vec<&TabStop> = keyed.into_iter().map(|(_, stop)| stop).collect();
        let at = from.and_then(|from| stops.iter().position(|stop| stop.widget == from));
        let next = match at {
            Some(at) if backward => (at + stops.len() - 1) % stops.len(),
            Some(at) => (at + 1) % stops.len(),
            None if backward => stops.len().wrapping_sub(1),
            None => 0,
        };
        // A stop of the current layout: of its current record, or in Find
        // mode a criterion of the request, which belongs to no record (#858).
        let layout = session.layout.map(u64::from);
        let record = session.current_record().map(|(_, record)| record.id);
        // The mode changed since the Tab (Find and Browse): no stop of the
        // last frame is this one's.
        let Some(stop) = stops.get(next).filter(|stop| {
            Some(stop.widget) != from
                && layout == Some(stop.target.0)
                && if find {
                    stop.target.1 == FIND_REQUEST
                } else {
                    record == Some(stop.target.1)
                }
        }) else {
            return TabMove::Stays;
        };
        if let Some(key) = stop.key {
            self.tab_goal = Some(key);
            self.tab_goal_frames = 0;
            return TabMove::Aimed;
        }
        let (target, widget, object) = (stop.target, stop.widget, stop.object);
        ui.memory_mut(|memory| memory.request_focus(widget));
        if object {
            self.pending_focus = Some(target);
        }
        self.tab_entry = true;
        self.tab_target = Some(target);
        TabMove::Moved
    }

    /// The field of a portal row (or related field) `key`, drawn now, takes
    /// the focus a Tab aims at it (`tab_goal`), and the keys typed since,
    /// in this frame before its editor draws: not from a row painted while
    /// its portal is read again, which may have left it, unless it already
    /// has the keyboard. A further Tab among those keys, and what follows
    /// it, waits for the next frame (`tab_held`), from this field.
    fn reach_tab_goal(&mut self, ui: &Ui, key: (RelatedField, u64), id: egui::Id) -> bool {
        let fresh = !self.painting_stale || ui.memory(|memory| memory.has_focus(id));
        if self.tab_goal != Some(key) || !fresh {
            return false;
        }
        self.tab_goal = None;
        ui.memory_mut(|memory| memory.request_focus(id));
        // The next Tab starts from here, also in the next frame.
        self.focus_before = Some(id);
        let mut keys = std::mem::take(&mut self.tab_goal_keys);
        if let Some(at) = keys.iter().position(|event| tab_key(event).is_some()) {
            let mut rest = keys.split_off(at);
            let backward = tab_key(&rest.remove(0)).unwrap_or(false);
            self.tab_held = Some((backward, Some(id), rest));
            ui.ctx().request_repaint();
        }
        replay(ui, keys);
        true
    }

    /// A veto, failed save, or context change cancels the originating click.
    /// Its window may be another than the one drawn now: the clicked
    /// editor gives up the focus when its window next draws.
    pub fn cancel_pending_focus(&mut self) {
        // A Tab's field waiting to draw: its keys go where the focus is.
        self.tab_goal = None;
        self.pending_selection = None;
        self.portal_click = None;
        self.field_click = None;
        self.option_wait = None;
        self.pending_cursor = None;
        if let Some(target) = self.pending_focus.take() {
            self.cancelled_focus = Some(target);
        }
    }

    /// Button `object`'s click in a portal row waits to find its row again
    /// (its record's row as the window paints it next), on its record made
    /// current when `moved`; a later click replaces it.
    pub fn wait_portal_click(&mut self, object: u64, click: PortalClick, moved: bool) {
        self.portal_click = Some(PortalClickWait {
            object,
            click: PortalClick {
                current: false,
                ..click
            },
            moved,
            seen: true,
        });
    }

    /// The window is not drawn, or the user did something else: a waiting
    /// portal click, on a button or a field, goes.
    pub fn drop_portal_click(&mut self) {
        self.portal_click = None;
        self.field_click = None;
    }

    /// [`Self::cancel_pending_focus`], for a script that ended with the
    /// focus in the field the user entered: a waiting option set change
    /// of it stays (its own checks still apply when it is made).
    pub fn cancel_pending_focus_keeping_option(&mut self) {
        let wait = self.option_wait.take();
        self.cancel_pending_focus();
        self.option_wait = wait;
    }

    /// An alert covers the active field, whose editor egui leaves without
    /// the focus meanwhile: the field stays active, so no exit or enter
    /// trigger runs, and its editor takes the focus back when the alert
    /// goes (`release_focus`), as FileMaker Pro keeps the field entered.
    /// `record` is the window's layout and current record.
    pub(crate) fn hold_focus(&mut self, record: Option<(u32, i64)>) {
        let Some(field) = self.active.clone() else {
            return;
        };
        let object = self.active_object;
        let editor = record
            .zip(object)
            .map(|((layout, record), object)| (u64::from(layout), record, object));
        self.held_focus = Some(HeldFocus {
            field,
            object,
            editor,
        });
    }

    /// The Other… dialog is open over the document, with its keyboard
    /// (#869).
    pub(crate) fn other_dialog_open(&self) -> bool {
        self.other_dialog.is_some()
    }

    /// The alert over the active field went (`hold_focus`).
    pub(crate) fn release_focus(&mut self) {
        match self.held_focus.take() {
            Some(HeldFocus {
                editor: Some(editor),
                ..
            }) => self.pending_focus = Some(editor),
            Some(held) => self.focus = Some(held.field),
            None => {}
        }
    }

    /// A trigger of the user's edit of a related field starts: the field
    /// stays active, with no exit or enter trigger, while the script runs
    /// and its portal's rows are read again (a script's changes, its
    /// variables' too, read them again), until its editor has the focus
    /// again (`release_related_focus`).
    pub(crate) fn hold_related_focus(&mut self) {
        if let (Some(name), Some(focus)) = (&self.active, self.active_related) {
            self.held_related = Some(HeldRelated {
                name: name.clone(),
                focus,
                released: false,
                seen: false,
            });
        }
    }

    /// The script ended: the editor showing the held field's record takes
    /// the focus back, in whichever row shows it now (`related_field`). One
    /// outside portals is let go.
    pub(crate) fn release_related_focus(&mut self) {
        match &mut self.held_related {
            Some(held) if held.focus.portal.is_some() => held.released = true,
            Some(_) => self.held_related = None,
            None => {}
        }
    }

    /// A trigger refused the exit from portal row field `focus` (named
    /// `name`): the editor showing its record takes the focus back, in
    /// whichever row shows it now, as a held field does once its script
    /// ended (#1050). It is let go when no frame paints that portal.
    pub(crate) fn refocus_related(&mut self, name: String, focus: RelatedFocus) {
        self.held_related = Some(HeldRelated {
            name,
            focus,
            released: true,
            seen: false,
        });
    }

    /// The script moved the focus, the layout or the record: the held
    /// related field is not given the focus back.
    pub(crate) fn drop_related_hold(&mut self) {
        self.held_related = None;
    }

    /// The related field object the user's focus is held in, if any.
    pub(crate) fn held_related_object(&self) -> Option<u64> {
        self.held_related.as_ref().map(|held| held.focus.object)
    }

    /// Restore the exact object's editor after its exit trigger refuses
    /// a focus move. The caller has checked the original window context.
    pub(crate) fn restore_object_focus(&mut self, layout: u64, record: i64, object: u64) {
        self.pending_focus = Some((layout, record, object));
    }

    /// Whether the only popup open is the list of the drop-down list whose
    /// editor has the focus (`editor_list`).
    pub(crate) fn editor_list_open(&self, ctx: &egui::Context) -> bool {
        self.editor_list
            .is_some_and(|menu| egui::Popup::is_id_open(ctx, menu))
    }

    /// The window is not drawn: a waiting option set change goes.
    pub fn drop_option_change(&mut self) {
        self.option_wait = None;
    }

    /// Option set `object` on `record` of `layout` takes `key`, which its
    /// keystroke trigger let through, once it is shown with the focus
    /// (Space enters the picked item, a letter picks one).
    pub fn queue_option_key(&mut self, layout: u64, record: i64, object: u64, key: String) {
        self.option_wait = Some(OptionWait {
            layout,
            record,
            object,
            change: OptionChange::Key(key),
            fresh: false,
            seen: true,
        });
    }

    /// The records portal `key` (layout, portal, parent record) shows: those
    /// `read` now, else, while they are read again, those it showed last.
    fn portal_records(
        &mut self,
        key: (u64, u64, u64),
        read: Option<std::sync::Arc<[u64]>>,
    ) -> Option<std::sync::Arc<[u64]>> {
        if read.is_some() {
            self.read_portals.insert(key);
        }
        let records =
            read.or_else(|| self.shown_portals.iter().find_map(|f| f.get(&key).cloned()))?;
        self.shown_portals[0].insert(key, std::sync::Arc::clone(&records));
        Some(records)
    }

    /// What this frame paints cannot tell yet whether some of it shows (a
    /// hide condition's data, a portal's rows on their way): a waiting
    /// click's row and button, and a held related field's editor, may
    /// still show, so they keep waiting.
    fn visibility_undecided(&mut self) {
        if let Some(wait) = &mut self.portal_click {
            wait.seen = true;
        }
        if let Some(wait) = &mut self.field_click {
            wait.seen = true;
        }
        if let Some(held) = &mut self.held_related {
            held.seen = true;
        }
    }

    /// The waiting click of button `object` in `row` of `portal` on
    /// `parent` (the current record or not), at the row's number now,
    /// when nothing holds it.
    fn take_portal_click(
        &mut self,
        layout: u64,
        portal: u64,
        (parent, current): (u64, bool),
        object: u64,
        row: PortalRow,
    ) -> Option<PortalClick> {
        let wait = self.portal_click.as_mut()?;
        let click = wait.click;
        if wait.object != object
            || click.layout != layout
            || click.portal != portal
            || click.parent != parent
            || click.occurrence != row.occurrence
            || click.record != row.record
            || click.unsaved != self.painting_unsaved
        {
            return None;
        }
        wait.seen = true;
        if self.clicks_held || self.pending || self.painting_stale {
            return None;
        }
        let moved = wait.moved;
        self.portal_click = None;
        if moved && !current {
            // Its record did not become current (a trigger refused).
            return None;
        }
        Some(PortalClick {
            row: row.number as u64,
            current: true,
            ..click
        })
    }

    /// A click on object `object` of `record` on `layout` that waits for
    /// the window's save.
    #[cfg(test)]
    pub(crate) fn click_during_save(&mut self, layout: u64, record: i64, object: u64) {
        self.pending = true;
        self.pending_focus = Some((layout, record, object));
    }

    #[cfg(test)]
    pub(crate) fn pending_click(&self) -> Option<(u64, i64, u64)> {
        self.pending_focus
    }

    /// Whether a click on a portal row's button or field waits for its
    /// row: until it runs, or the field takes the focus, the window has not
    /// settled (`OpenMakerApp::is_settled`, #1113). The records portals
    /// read are waited for as calc data (`Calculations::awaiting_data`),
    /// which leaves out reads that failed or wait for an unreachable host.
    pub fn awaiting_portal(&self) -> bool {
        self.portal_click.is_some() || self.field_click.is_some()
    }

    /// A frame starts: focus and edits are seen again.
    pub fn begin_frame(&mut self) {
        self.edit_preparations
            .retain(|key, _| self.buffers.contains_key(key));

        self.shown_portals[1] = std::mem::take(&mut self.shown_portals[0]);
        if let Some(wait) = &mut self.portal_click {
            wait.seen = false;
        }
        if let Some(wait) = &mut self.field_click {
            wait.seen = false;
        }
        if let Some(wait) = &mut self.option_wait {
            wait.seen = false;
        }
        self.seen_active = None;
        self.seen_object = None;
        self.seen_related = None;
        self.painted_stops = std::mem::take(&mut self.tab_stops);
        // Keys that waited for a Tab's field far longer than Browse would
        // let them (it is not drawn) are dropped rather than typed later.
        if self.tab_goal.is_some() || !self.tab_goal_keys.is_empty() {
            self.tab_goal_frames = self.tab_goal_frames.saturating_add(1);
            if self.tab_goal_frames > 2 * TAB_GOAL_FRAMES {
                self.tab_goal = None;
                self.tab_goal_keys.clear();
            }
        }
        self.editor_list = None;
        self.changed.clear();
        self.web_viewers.clear();
        self.overlays.clear();
        self.surrender_focus = self.cancelled_focus.take();
    }

    /// A frame ended: the field the focus left and the one it entered (when
    /// it moved), and the fields edited, each with the object showing it
    /// when the frame painted one.
    #[allow(clippy::type_complexity)]
    pub fn end_frame(&mut self) -> (Option<Focused>, Option<Focused>, Vec<Focused>) {
        // The field a Tab moved to was not drawn: its focus goes.
        if let Some(target) = self.tab_target.take() {
            if self.pending_focus == Some(target) {
                self.pending_focus = None;
            }
            self.tab_entry = false;
        }
        self.related_entry = None;
        if !self.clicks_held && self.portal_click.is_some_and(|wait| !wait.seen) {
            self.portal_click = None;
        }
        // A field click whose record's row no longer shows goes.
        if !self.clicks_held && self.field_click.is_some_and(|wait| !wait.seen) {
            self.field_click = None;
        }
        if !self.clicks_held
            && !self.pending
            && self.option_wait.as_ref().is_some_and(|wait| !wait.seen)
        {
            self.option_wait = None;
        }
        if let (None, Some(held)) = (&self.seen_active, &self.held_focus) {
            self.seen_active = Some(held.field.clone());
            self.seen_object = held.object;
        }
        // A held field whose record its portal, read again, no longer
        // shows is left (its exit triggers run).
        if let Some(held) = &mut self.held_related {
            let seen = std::mem::take(&mut held.seen);
            let portal = held.focus.portal.and_then(|portal| {
                let parent = u64::try_from(held.focus.field.parent).ok()?;
                Some((held.focus.field.layout, portal, parent))
            });
            let read = portal.is_some_and(|portal| self.read_portals.contains(&portal));
            // Nor is one whose portal a frame that decided what it shows
            // did not paint: hidden, or on another layout or record, it has
            // no editor to take the focus back (#1050). A frame still
            // deciding keeps it (`visibility_undecided`).
            let painted = portal.is_some_and(|portal| self.shown_portals[0].contains_key(&portal));
            if held.released && !seen && (read || !painted) {
                self.held_related = None;
            }
        }
        self.read_portals.clear();
        if let (None, Some(held)) = (&self.seen_active, &self.held_related) {
            self.seen_active = Some(held.name.clone());
            self.seen_object = Some(held.focus.object);
            self.seen_related = Some(held.focus);
        }
        if self.seen_active.is_some() {
            // A field's editor has the focus.
            self.container = None;
            self.portal_focus = None;
            self.selected_row = None;
        } else if let Some(selection) = self.container.as_ref().filter(|s| s.row.is_none()) {
            // A related container in a portal row is no active field of
            // the window's record.
            self.seen_active = Some(selection.field.clone());
            self.seen_object = Some(selection.object);
        }
        let changed = std::mem::take(&mut self.changed);
        let painted = std::mem::replace(&mut self.painted_object, self.seen_object);
        self.active_object = self.seen_object;
        let was_related = std::mem::replace(&mut self.active_related, self.seen_related);
        // The focus went from one object of a field to another of it, or
        // from one related record's row to another's in one portal.
        let moved = self.seen_active.is_some()
            && (matches!((painted, self.seen_object), (Some(a), Some(b)) if a != b)
                || matches!((was_related, self.seen_related), (Some(a), Some(b)) if a.field != b.field));
        if self.seen_active == self.active && !moved {
            return (None, None, changed);
        }
        let left = self.active.take().map(|field| (field, painted));
        self.left_related = was_related.filter(|_| left.is_some());
        self.active = self.seen_active.clone();
        let entered = self.active.clone().map(|field| (field, self.seen_object));
        // A field the user (a click, Tab) or a step put the keyboard in
        // since: a field click still waiting for its row goes, rather than
        // take the focus from it later.
        if entered.is_some() {
            self.field_click = None;
        }
        (left, entered, changed)
    }

    /// The container field selected on the session's current record.
    pub fn selected_container(&self, session: &Session) -> Option<&ContainerSelection> {
        let record = session.current_record()?.1.id;
        self.container.as_ref().filter(|selection| {
            Some(selection.layout) == session.layout.map(u64::from) && selection.record == record
        })
    }

    /// Before Browse paints: a selection of another layout or record (the
    /// window went elsewhere) or outside Browse mode ends. While the
    /// current record loads it stays.
    pub fn keep_container(&mut self, session: Option<&Session>, browse: bool) {
        let Some(selection) = &self.container else {
            return;
        };
        let kept = browse
            && session.is_some_and(|session| {
                Some(selection.layout) == session.layout.map(u64::from)
                    && session
                        .current_record()
                        .is_none_or(|(_, record)| record.id == selection.record)
            });
        if !kept {
            self.container = None;
        }
    }

    /// A script left the focus in `field` (or none): a selected container
    /// stays selected only if it is that field.
    pub fn script_focus(&mut self, field: Option<&str>) {
        // A selection waiting for a field the focus no longer goes to.
        if self
            .pending_selection
            .as_ref()
            .is_some_and(|(_, _, pending, _)| Some(pending.as_str()) != field)
        {
            self.pending_selection = None;
        }
        if self
            .container
            .as_ref()
            .is_some_and(|selection| Some(selection.field.as_str()) != field)
        {
            self.container = None;
        }
    }

    /// The editor an auto-complete happened in no longer has the focus:
    /// its field keeps the item's text if it still holds the completed
    /// text (#870). Checked by the editor, not by the field's other
    /// objects, which may show it unfocused while it is still typed in.
    pub(crate) fn settle_completions(&mut self, ui: &Ui) {
        // An alert holding the field entered, or a focus still to be
        // given (after the alert, a click waiting for a save), is not
        // leaving it yet.
        if self.held_focus.is_some() || self.pending_focus.is_some() || self.focus.is_some() {
            return;
        }
        let left = |editor: egui::Id| !ui.memory(|memory| memory.has_focus(editor));
        if self.completed.as_ref().is_some_and(|c| left(c.editor)) {
            if let Some(completion) = self.completed.take() {
                if let Some(buffer) = self.buffers.get_mut(&completion.key) {
                    buffer.accept_completion(&completion);
                }
            }
        }
        if self
            .related_completed
            .as_ref()
            .is_some_and(|c| left(c.editor))
        {
            if let Some(completion) = self.related_completed.take() {
                if let Some(edit) = self.related.get_mut(&completion.key) {
                    edit.buffer.accept_completion(&completion);
                }
            }
        }
    }

    /// What a script left uncommitted in `field`, a later repetition of
    /// the current record (`scripted`): "" for one it cleared.
    fn scripted_value(&self, field: &RelatedField) -> Option<String> {
        let (table, record, cells) = self.scripted.as_ref()?;
        if field.table != *table || field.row != RelatedRow::Record(*record) {
            return None;
        }
        cells
            .get(&field.field)
            .map(|value| value.clone().unwrap_or_default())
    }

    /// Whether `field` of any record has an unsaved draft.
    pub fn edited(&self, field: &str) -> bool {
        self.buffers
            .iter()
            .any(|((_, _, name), b)| name == field && b.value != b.original)
    }

    /// Apply an auto-entry to a typed draft, keeping its record version.
    pub(crate) fn apply_typed_auto_entry(
        &mut self,
        layout: u32,
        record: i64,
        field: &str,
        transform: impl FnOnce(&str) -> Option<String>,
    ) -> bool {
        let Some(buffer) = self
            .buffers
            .get_mut(&(u64::from(layout), record, field.to_owned()))
        else {
            return false;
        };
        if !buffer.ui_edited {
            return false;
        }
        let Some(value) = transform(&buffer.value) else {
            return false;
        };
        buffer.value = value;
        true
    }

    /// What the editor of related field `field` holds, once it was drawn.
    pub fn related_text(&self, field: &RelatedField) -> Option<&str> {
        self.related
            .get(field)
            .map(|edit| edit.buffer.value.as_str())
    }

    /// Whether the user changed the related field `focus` shows: a stored
    /// record's since it was read, a begun one's at all.
    pub fn related_field_edited(&self, focus: &RelatedFocus) -> bool {
        self.related
            .get(&focus.field)
            .is_some_and(|edit| match focus.field.row {
                RelatedRow::Record(_) => edit.changed(),
                RelatedRow::New { .. } => edit.buffer.ui_edited,
            })
    }

    /// A draft a script left: `value` over the stored `original`.
    pub fn put_draft(
        &mut self,
        layout: u32,
        record: i64,
        field: &str,
        original: String,
        value: String,
    ) {
        self.edit_preparations
            .remove(&(u64::from(layout), record, field.to_owned()));
        self.buffers.insert(
            (u64::from(layout), record, field.to_owned()),
            FieldBuffer {
                original,
                value,
                ui_edited: false,
                // The worker supplied this original; the retained page may
                // still be loading or predate a targeted refresh.
                refreshed: true,
                retained_page: None,
                accepted_version: None,
            },
        );
    }

    fn saw(&mut self, response: &egui::Response, field: &str, current: bool, object: Option<u64>) {
        if response.has_focus() && current {
            self.seen_active = Some(field.to_owned());
            self.seen_object = object;
            self.scroll.focused(field, response.id);
            let state = egui::TextEdit::load_state(&response.ctx, response.id);
            // A control that is not a text editor (a set's button, a pop-up
            // menu) has no selection: none is read from an editor left.
            self.editor_selection = state.as_ref().map(|state| {
                let range = state.cursor.char_range();
                EditorSelection {
                    field: field.to_owned(),
                    editor: response.id,
                    range: range.map_or((0, 0), |range| {
                        (range.primary.index.0, range.secondary.index.0)
                    }),
                }
            });
            if let Some((layout, record, _, selection)) = self
                .pending_selection
                .take_if(|(_, _, pending, _)| pending == field && state.is_some())
            {
                self.place_cursor(&response.ctx, (layout, record), field, selection, None);
            }
        }
        if response.changed() {
            self.changed.push((field.to_owned(), object));
        }
    }

    /// The active field `field`'s selection, 1-based start and length, as
    /// a script reads it (`Get ( ActiveSelectionStart )`; the Insert steps
    /// insert there).
    pub(crate) fn selection(&self, field: &str) -> Option<(u64, u64)> {
        let selection = self.editor_selection.as_ref()?;
        if selection.field != field {
            return None;
        }
        let (a, b) = selection.range;
        Some((a.min(b) as u64 + 1, a.abs_diff(b) as u64))
    }

    /// The editor and buffer of the field the user is typing in on record
    /// `record` of layout `layout` (one of the record's own fields).
    fn entered_buffer(&self, layout: u64, record: i64) -> Option<(egui::Id, &FieldBuffer)> {
        let selection = self.editor_selection.as_ref()?;
        if self.active.as_deref() != Some(selection.field.as_str()) || self.active_related.is_some()
        {
            return None;
        }
        let buffer = self
            .buffers
            .get(&(layout, record, selection.field.clone()))?;
        Some((selection.editor, buffer))
    }

    /// The editor of the field entered on this record, for menus shown
    /// outside egui (the macOS system menu bar, OpenMaker Go's).
    pub(crate) fn entered_editor(&self, layout: u64, record: i64) -> Option<egui::Id> {
        self.entered_buffer(layout, record).map(|(id, _)| id)
    }

    /// Whether Edit > Undo and Redo have a step to take back or again in
    /// the field the user is typing in (its editor's own history).
    pub(crate) fn field_undo(&self, ctx: &egui::Context, layout: u64, record: i64) -> (bool, bool) {
        let Some((editor, buffer)) = self.entered_buffer(layout, record) else {
            return (false, false);
        };
        let Some(state) = egui::TextEdit::load_state(ctx, editor) else {
            return (false, false);
        };
        let Some(range) = state.cursor.char_range() else {
            return (false, false);
        };
        let current = (range, buffer.value.clone());
        let undoer = state.undoer();
        (undoer.has_undo(&current), undoer.has_redo(&current))
    }

    /// Edit > `edit` in the field the user is typing in, as its editor's
    /// own keys do (Undo and Redo through its history). The record's draft
    /// takes the change; the field stays entered. A field that refuses
    /// typing (`entered_refusal`) still selects, and refuses a change, as
    /// its keys are refused: the refusal is returned and nothing changes.
    pub(crate) fn edit_field(
        &mut self,
        ctx: &egui::Context,
        layout: u64,
        record: i64,
        edit: FieldEdit,
    ) -> Option<EditRefusal> {
        let (editor, _) = self.entered_buffer(layout, record)?;
        if edit != FieldEdit::SelectAll {
            if let Some((_, refusal)) = self.entered_refusal.filter(|(id, _)| *id == editor) {
                return Some(refusal);
            }
        }
        let field = self.active.clone()?;
        let buffer = self.buffers.get_mut(&(layout, record, field))?;
        let mut state = egui::TextEdit::load_state(ctx, editor).unwrap_or_default();
        let len = buffer.value.chars().count();
        let range = state
            .cursor
            .char_range()
            .unwrap_or_else(|| egui::text::CCursorRange::one(egui::text::CCursor::new(len)));
        match edit {
            FieldEdit::SelectAll => {
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(len),
                    )))
            }
            FieldEdit::Clear => {
                let [start, end] = range.sorted_cursors();
                let (start, end) = (start.index.0.min(len), end.index.0.min(len));
                if start < end {
                    let mut undoer = state.undoer();
                    undoer.add_undo(&(range, buffer.value.clone()));
                    state.set_undoer(undoer);
                    buffer.value = buffer
                        .value
                        .chars()
                        .take(start)
                        .chain(buffer.value.chars().skip(end))
                        .collect();
                    buffer.ui_edited = true;
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(
                            egui::text::CCursor::new(start),
                        )));
                }
            }
            FieldEdit::Undo | FieldEdit::Redo => {
                let current = (range, buffer.value.clone());
                let mut undoer = state.undoer();
                let restored = if edit == FieldEdit::Undo {
                    undoer.undo(&current).cloned()
                } else {
                    undoer.redo(&current).cloned()
                };
                if let Some((range, text)) = restored {
                    buffer.value = text;
                    buffer.ui_edited = true;
                    state.cursor.set_char_range(Some(range));
                }
                state.set_undoer(undoer);
            }
        }
        state.store(ctx, editor);
        None
    }

    /// A step left `field`'s selection at `selection` (1-based start and
    /// length; `None` after its `len` characters): its editor shows it, as
    /// after an Insert step FileMaker Pro puts the insertion point after
    /// what it inserted.
    pub(crate) fn place_cursor(
        &mut self,
        ctx: &egui::Context,
        (layout, record): (u64, i64),
        field: &str,
        selection: Option<(u64, u64)>,
        len: Option<usize>,
    ) {
        let Some(editor) = self
            .editor_selection
            .as_mut()
            .filter(|selection| selection.field == field)
        else {
            // Its editor takes it as it gets the focus.
            self.pending_selection = Some((layout, record, field.to_owned(), selection));
            return;
        };
        // The text as the step left it, else as the editor holds it.
        let len = len
            .or_else(|| {
                self.buffers
                    .get(&(layout, record, field.to_owned()))
                    .map(|buffer| buffer.value.chars().count())
            })
            .unwrap_or(0);
        let (start, size) = selection.map_or((len, 0), |(start, size)| {
            let start = usize::try_from(start.saturating_sub(1))
                .unwrap_or(len)
                .min(len);
            (start, usize::try_from(size).unwrap_or(0).min(len - start))
        });
        let mut state = egui::TextEdit::load_state(ctx, editor.editor).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(start),
                egui::text::CCursor::new(start + size),
            )));
        state.store(ctx, editor.editor);
        editor.range = (start, start + size);
    }

    /// The scroll offset that shows the current record's row, at `top`,
    /// once the current record changes.
    pub fn list_scroll(&mut self, session: &Session, top: f32, ui: &Ui) -> Option<f32> {
        let key = (session.layout.unwrap_or_default(), session.current);
        let changed = self.list_current != Some(key);
        if !ui.is_sizing_pass() {
            self.list_current = Some(key);
        }
        changed.then_some(top)
    }
    /// A draft of `name` on the current record, as if the user typed it.
    #[cfg(test)]
    pub fn set_field(
        &mut self,
        session: &Session,
        name: &str,
        value: String,
    ) -> Result<(), String> {
        let (fields, record) = session
            .current_record()
            .ok_or("The current record has not loaded.")?;
        let index = fields
            .iter()
            .position(|field| field.name == name)
            .ok_or_else(|| format!("The current table has no field named {name:?}."))?;
        if !self.capabilities.can_edit(&fields[index]) {
            return Err(format!(
                "The field {name:?} cannot be edited on this connection."
            ));
        }
        let server = record
            .values
            .get(index)
            .and_then(Option::as_deref)
            .unwrap_or("");
        self.edit_preparations.insert(
            (
                u64::from(session.layout.unwrap_or_default()),
                record.id,
                name.into(),
            ),
            CandidatePreparation::Accepted,
        );
        let buffer = self
            .buffers
            .entry((
                u64::from(session.layout.unwrap_or_default()),
                record.id,
                name.into(),
            ))
            .or_insert_with(|| FieldBuffer::new(server));
        buffer.remember_version(session, session.current);
        buffer.value = value;
        buffer.ui_edited = true;
        Ok(())
    }
    /// The current record's field `name` as the window shows it: its
    /// draft, else its stored text.
    pub fn value(&self, session: &Session, name: &str) -> Option<String> {
        let (fields, record) = session.current_record()?;
        let layout = u64::from(session.layout.unwrap_or_default());
        if let Some(buffer) = self.buffers.get(&(layout, record.id, name.to_owned())) {
            return Some(buffer.value.clone());
        }
        let index = fields.iter().position(|field| field.name == name)?;
        Some(record.values.get(index)?.clone().unwrap_or_default())
    }

    pub fn changes(&self, session: &Session) -> om_data::FieldChanges {
        let Some((fields, record)) = session.current_record() else {
            return Default::default();
        };
        let layout = u64::from(session.layout.unwrap_or_default());
        self.buffers
            .iter()
            .filter_map(|((id, row, name), buffer)| {
                if *id != layout || *row != record.id {
                    return None;
                }
                if buffer.original == buffer.value
                    || !self.accepted_buffer(&(*id, *row, name.clone()), buffer)
                {
                    return None;
                }
                let value = &buffer.value;
                let index = fields.iter().position(|field| field.name == *name)?;
                if !buffer.refreshed
                    && record
                        .values
                        .get(index)
                        .and_then(Option::as_deref)
                        .unwrap_or("")
                        == value
                {
                    return None;
                }
                Some((name.clone(), (!value.is_empty()).then(|| value.clone())))
            })
            .collect()
    }
    /// How many fields hold an edit not saved to the host, on any record.
    pub fn unsaved(&self) -> usize {
        self.buffers
            .values()
            .filter(|buffer| buffer.value != buffer.original)
            .count()
    }

    /// The records with unsaved edits, by layout and record.
    pub fn drafted_records(&self) -> std::collections::BTreeSet<(u32, i64)> {
        self.buffers
            .iter()
            .filter(|(key, buffer)| {
                buffer.value != buffer.original && self.accepted_buffer(key, buffer)
            })
            .filter_map(|((layout, record, _), _)| Some((u32::try_from(*layout).ok()?, *record)))
            .collect()
    }

    /// Records with genuine user edits, distinct from worker-only drafts.
    pub fn ui_drafted_records(&self) -> std::collections::BTreeSet<(u32, i64)> {
        self.buffers
            .iter()
            .filter(|(key, buffer)| {
                buffer.ui_edited
                    && buffer.value != buffer.original
                    && self.accepted_buffer(key, buffer)
            })
            .filter_map(|((layout, record, _), _)| Some((u32::try_from(*layout).ok()?, *record)))
            .collect()
    }

    /// The unsaved edits of every record, by layout and record.
    pub fn drafts(&self) -> BTreeMap<(u32, i64), Vec<FieldDraft>> {
        let mut drafts: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for ((layout, record, name), buffer) in &self.buffers {
            let Ok(layout) = u32::try_from(*layout) else {
                continue;
            };
            if buffer.value != buffer.original
                && self.accepted_buffer(&(u64::from(layout), *record, name.clone()), buffer)
            {
                drafts
                    .entry((layout, *record))
                    .or_default()
                    .push(FieldDraft {
                        name: name.clone(),
                        original: buffer.original.clone(),
                        value: buffer.value.clone(),
                    });
            }
        }
        drafts
    }

    /// The fields of record `record` of `layout` whose draft changed
    /// their value (a field only entered is not one).
    pub fn edited_fields(&self, layout: u32, record: i64) -> Vec<String> {
        self.buffers
            .iter()
            .filter(|((id, row, _name), buffer)| {
                *id == u64::from(layout)
                    && *row == record
                    && buffer.value != buffer.original
                    && self.accepted_buffer(&(*id, *row, _name.clone()), buffer)
            })
            .map(|((_, _, name), _)| name.clone())
            .collect()
    }

    /// Record `old` is record `new` now: a new record a reconnect held again
    /// under another id (#152). Its drafts, its related records' and the
    /// focus on it follow it; what was only shown of it is drawn again.
    pub fn renumber(&mut self, old: i64, new: i64) {
        let moved = |record: &mut i64| {
            if *record == old {
                *record = new;
            }
        };
        self.buffers = std::mem::take(&mut self.buffers)
            .into_iter()
            .map(|((layout, mut record, name), buffer)| {
                moved(&mut record);
                ((layout, record, name), buffer)
            })
            .collect();
        self.related = std::mem::take(&mut self.related)
            .into_iter()
            .map(|(mut field, buffer)| {
                moved(&mut field.parent);
                (field, buffer)
            })
            .collect();
        for focus in [
            &mut self.tab_target,
            &mut self.pending_focus,
            &mut self.cancelled_focus,
            &mut self.surrender_focus,
        ]
        .into_iter()
        .flatten()
        {
            moved(&mut focus.1);
        }
        for (_, _, focus) in &mut self.first_candidates {
            moved(&mut focus.1);
        }
        if let Some(focus) = &mut self.portal_focus {
            moved(&mut focus.parent);
        }
        self.completed = None;
        self.related_completed = None;
        self.selected_row = None;
        self.shown_portals = Default::default();
    }

    pub fn clear_record(&mut self, layout: u32, record: i64) {
        self.held_edit_actions.clear();
        self.held_edit_focus = None;
        self.buffers
            .retain(|(id, row, _), _| *id != u64::from(layout) || *row != record);
        self.edit_preparations
            .retain(|(id, row, _), _| *id != u64::from(layout) || *row != record);
    }
    pub fn revert(&mut self, session: &Session) {
        // A reverted record's auto-complete is no longer to keep (#870).
        self.completed = None;
        self.related_completed = None;
        if let Some((_, record)) = session.current_record() {
            let layout = session.layout.unwrap_or_default();
            self.clear_record(layout, record.id);
            let (layout, parent) = (u64::from(layout), record.id);
            self.retain_related(|field| field.layout != layout || field.parent != parent);
        }
    }

    /// The unsaved edits of related records made through record `parent`
    /// of `layout`: changed fields of stored records, and every field the
    /// user typed into of a record begun in a creation row (also one typed
    /// back to empty: the record stays begun).
    pub fn related_edits(&self, layout: u32, parent: i64) -> Vec<RelatedEdit> {
        let layout = u64::from(layout);
        self.related
            .iter()
            .filter(|(key, edit)| {
                key.layout == layout
                    && key.parent == parent
                    && match key.row {
                        RelatedRow::Record(_) => edit.changed(),
                        RelatedRow::New { .. } => edit.buffer.ui_edited,
                    }
            })
            .map(|(key, edit)| RelatedEdit {
                field: *key,
                occurrence: edit.occurrence,
                version: edit.version,
                value: edit.buffer.value.clone(),
                original: edit.buffer.original.clone(),
            })
            .collect()
    }

    /// The edits of stored related records made through any record of any
    /// layout, still unsaved: what a reconnect reads back (#152).
    pub fn all_related_edits(&self) -> Vec<RelatedEdit> {
        self.related
            .iter()
            .filter(|(key, edit)| matches!(key.row, RelatedRow::Record(_)) && edit.changed())
            .map(|(key, edit)| RelatedEdit {
                field: *key,
                occurrence: edit.occurrence,
                version: edit.version,
                value: edit.buffer.value.clone(),
                original: edit.buffer.original.clone(),
            })
            .collect()
    }

    /// A user's edit of related field `field`: `value` over the stored
    /// `original`, the record shown at `version`.
    #[cfg(test)]
    pub fn put_related_edit(
        &mut self,
        field: RelatedField,
        occurrence: u64,
        version: Option<u64>,
        original: &str,
        value: &str,
    ) {
        let edit = self
            .related
            .entry(field)
            .or_insert_with(|| RelatedBuffer::new(occurrence, version, original, None));
        edit.buffer.value = value.into();
        edit.buffer.ui_edited = true;
    }

    /// Whether a record of `layout` has unsaved edits of related records
    /// made through it.
    pub fn related_edited_on(&self, layout: u32) -> bool {
        let layout = u64::from(layout);
        self.related.iter().any(|(key, edit)| {
            key.layout == layout
                && match key.row {
                    RelatedRow::Record(_) => edit.changed(),
                    RelatedRow::New { .. } => edit.buffer.ui_edited,
                }
        })
    }

    /// Whether the current record has unsaved edits of related records.
    pub fn related_edited(&self, session: &Session) -> bool {
        session.current_record().is_some_and(|(_, record)| {
            !self
                .related_edits(session.layout.unwrap_or_default(), record.id)
                .is_empty()
        })
    }

    /// The records the user began in the creation row of a portal of
    /// `occurrence` through record `parent` of `layout`, in the order they
    /// were begun: they show as rows after the related records, before the
    /// next creation row.
    pub fn new_rows(&self, layout: u64, parent: i64, occurrence: u64) -> Vec<u64> {
        let kept = self
            .begun_rows
            .get(&occurrence)
            .into_iter()
            .flatten()
            .map(|(id, _, _)| *id);
        let ids: BTreeSet<u64> = self
            .related
            .iter()
            .filter_map(|(key, edit)| match key.row {
                RelatedRow::New { occurrence: of, id }
                    if key.layout == layout
                        && key.parent == parent
                        && of == occurrence
                        && edit.buffer.ui_edited =>
                {
                    Some(id)
                }
                _ => None,
            })
            .chain(kept)
            .collect();
        ids.into_iter().collect()
    }

    /// The id the next record begun in a creation row gets: also a record
    /// a step created through one (adopted, so it shows as its row).
    pub fn adopt_begun(&mut self) -> u64 {
        let id = self.next_new;
        self.next_new += 1;
        id
    }

    /// The related edits after the worker's answer: `keep` gives the
    /// value each one keeps (`Some(None)`: as it is), or drops it (`None`).
    pub fn settle_related(
        &mut self,
        mut keep: impl FnMut(&RelatedField) -> Option<Option<String>>,
    ) {
        self.related.retain(|field, edit| match keep(field) {
            Some(value) => {
                if let Some(value) = value {
                    edit.buffer.value = value;
                }
                true
            }
            None => false,
        });
    }

    /// Keep only the related edits `keep` keeps (a commit or a revert
    /// ended the others, or a script replaced them).
    pub fn retain_related(&mut self, mut keep: impl FnMut(&RelatedField) -> bool) {
        self.related.retain(|field, _| keep(field));
    }

    /// A field editor shared by generic Form, List and Table views. Its draft
    /// is keyed by record ID, so scrolling or changing views never applies an
    /// edit to another row.
    /// Keep just the current editor alive when virtualized rows scroll
    /// past it. It retains draft/cursor input and can bring itself back
    /// into view when typing resumes; no offscreen records are fetched.
    /// `complete_from`: by field name, the items a flagged drop-down list
    /// of the field lends its cell (`text_edit`), so a focused cell
    /// scrolled away still completes (#870).
    #[allow(clippy::type_complexity)]
    pub fn offscreen_editor(
        &mut self,
        ui: &mut Ui,
        session: &Session,
        origin: egui::Pos2,
        row_height: f32,
        complete_from: Option<&dyn Fn(&str) -> Option<std::sync::Arc<Choices>>>,
        actions: &mut Vec<Action>,
    ) {
        let Some(name) = self.focus.clone().or_else(|| self.active.clone()) else {
            return;
        };
        let Some((fields, _)) = session.current_record() else {
            return;
        };
        let Some(field) = fields.iter().find(|field| field.name == name) else {
            return;
        };
        let rect = Rect::from_min_size(origin, vec2(170.0, row_height));
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
        // Read every frame it is kept, as a shown cell reads them.
        let items = complete_from.and_then(|read| read(&field.name));
        let read = items.map(|items| move || items.clone());
        self.text_edit(
            &mut child,
            session,
            session.current,
            field,
            170.0,
            read.as_ref()
                .map(|read| read as &dyn Fn() -> std::sync::Arc<Choices>),
            actions,
        );
    }

    /// `complete_from`: the items of the drop-down list the layout shows
    /// the field with, when it has "Auto-complete using value list": the
    /// cell completes typing from them as that list does (#870), read
    /// while the current record's cell is entered.
    #[allow(clippy::too_many_arguments)]
    pub fn text_edit(
        &mut self,
        ui: &mut Ui,
        session: &Session,
        record_index: u64,
        field: &Field,
        width: f32,
        complete_from: Option<&dyn Fn() -> std::sync::Arc<Choices>>,
        actions: &mut Vec<Action>,
    ) -> Option<egui::Response> {
        // A field the account may not see takes no editor: it shows
        // `<No Access>`. One it may see but not change, and one of a record
        // it may not see, take a guarded one (`browse_refusal`).
        let refusal = browse_refusal(session, &self.capabilities, field, record_index);
        let hidden = session.no_access(record_index, &field.name);
        // A layout whose records the set gives no access offers no editor,
        // not even a guarded one, which would show the stored value (#707).
        if session.layout_masked()
            || refusal.is_none() && (hidden || !self.capabilities.can_edit(field))
        {
            return None;
        }
        let (fields, record) = session.record_at(record_index)?;
        let value = fields
            .iter()
            .position(|candidate| candidate.name == field.name)
            .and_then(|index| record.values.get(index))
            .and_then(Option::as_deref)
            .unwrap_or("");
        let value = if hidden { NO_ACCESS } else { value };
        let layout = u64::from(session.layout.unwrap_or_default());
        let edit = self
            .buffers
            .entry((layout, record.id, field.name.clone()))
            .or_insert_with(|| FieldBuffer::new(value));
        edit.refresh(value);
        edit.remember_version(session, record_index);
        let id = self
            .editor_scope
            .get_or_init(|| egui::Id::unique("record_editors"))
            .with((layout, record.id, &field.name));
        // A cell of the current record is a Tab stop, in column order: Tab
        // and Shift-Tab stay on the record and wrap, as FileMaker Pro's
        // Table view does (#848's readings, #858).
        let current = record_index == session.current;
        let column = fields
            .iter()
            .position(|candidate| candidate.name == field.name)
            .unwrap_or_default() as u64;
        let target = (layout, record.id, TABLE_CELL | column);
        // Entered by Tab: the insertion point goes after the text before
        // the editor takes the keys the Tab's frame held for it.
        if current && self.tab_target == Some(target) && std::mem::take(&mut self.tab_entry) {
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(edit.value.chars().count()),
                )));
            state.store(ui.ctx(), id);
        }
        let pending = self.pending;
        // Zoomed, the cell's text is rasterized at the zoom, as the layout's
        // own text is; at 100% it is egui's single-line text as it lays it
        // out itself.
        let font = egui::TextStyle::Body.resolve(ui.style());
        let line_height =
            ui.fonts_mut(|fonts| fonts.row_height(&font)) + ui.spacing().extra_text_line_spacing;
        let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
            let mut job = egui::text::LayoutJob::simple_singleline(
                text.as_str().to_owned(),
                font.clone(),
                theme::TEXT,
            );
            job.keep_trailing_whitespace = true;
            for section in &mut job.sections {
                section.format.line_height = Some(line_height);
            }
            layer_galley(ui, job)
        };
        let zoomed = layer_scale(ui) != 1.0;
        let (response, changed) =
            guarded_editor(ui, id, &mut edit.value, refusal, actions, |ui, buffer| {
                let editor = egui::TextEdit::singleline(buffer)
                    .id(id)
                    .interactive(!pending)
                    .text_color(theme::TEXT);
                ui.add_sized(
                    vec2(width, ui.spacing().interact_size.y),
                    if zoomed {
                        editor.font(font.clone()).layouter(&mut layouter)
                    } else {
                        editor
                    },
                )
            });
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::TextInput, !self.pending, &field.name)
        });
        if record_index == session.current
            && !session.is_loading_records()
            && (self.focus.as_deref() == Some(&field.name) || self.focus_first)
        {
            response.request_focus();
            self.scroll.resolve_focus(&field.name, response.id);
            if self.select_all {
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::select_all_str(
                            &edit.value,
                        )));
                    state.store(ui.ctx(), response.id);
                }
            }
            self.focus = None;
            self.focus_first = false;
            self.select_all = false;
        }
        if response.gained_focus() && record_index != session.current {
            actions.push(Action::GoToRecord(record_index));
        }
        if changed {
            edit.ui_edited = true;
        }
        // Auto-complete in a Table view cell, as in the field's drop-down
        // list (#870's Table view reading). Its items are read every frame
        // the cell is entered, as the list's editor reads them, so a
        // hosted list stays shown and is not asked for again.
        if let Some(read) = complete_from.filter(|_| current && response.has_focus()) {
            let choices = read();
            if changed {
                if let Some(item) = drop_down::complete(ui, id, &mut edit.value, &choices) {
                    edit.ui_edited = true;
                    self.completed = Some(Completion {
                        key: (layout, record.id, field.name.clone()),
                        editor: id,
                        text: edit.value.clone(),
                        item,
                    });
                }
            }
        }
        if response.lost_focus()
            && self.held_focus.is_none()
            && self.pending_focus.is_none()
            && self.completed.as_ref().is_some_and(|c| c.editor == id)
        {
            if let Some(completion) = self.completed.take() {
                edit.accept_completion(&completion);
            }
        }
        // A guarded editor (a field the account may see but not change)
        // is a stop too, so that its Tab stays on the record.
        if current {
            self.note_tab_stop(0, target, response.id, false);
            // Tab is the document's (`resolve_tab`), not egui's.
            if response.has_focus() {
                ui.memory_mut(|memory| {
                    memory.set_focus_lock_filter(response.id, drop_down::browse_keys(false))
                });
            }
        }
        if current {
            self.scroll
                .field_with_id(ui, &field.name, response.rect, response.id);
        }
        self.saw(&response, &field.name, current, None);
        Some(response)
    }
}

/// Find mode on the layout itself (Claris "Finding records"): the layout
/// as Form view draws it, with no record. Its field objects take the
/// current request's criteria, a portal shows its first row for criteria
/// on its fields, merge fields show nothing, and no record calculation
/// (hide condition, conditional formatting, calculated label) applies.
/// Return or Enter in a criterion performs the find.
pub(crate) fn show_find(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    find: &mut crate::app::FindState,
    (schema, calc): (Option<&om_data::host::CalcSchema>, Option<&Evaluator<'_>>),
    actions: &mut Vec<Action>,
) {
    let names = find_names(document, schema);
    // A value-list control offers its items as criteria; with no record,
    // a list of related values offers none (client-ui.md, "Find mode
    // content"). A set shows its items, and a pop-up menu showing their
    // labels needs them closed; a pop-up menu or drop-down list reads its
    // list once it opens (`FindEntry::wanted`, read after the frame).
    let wanted = std::mem::take(&mut state.find_wanted);
    // The controls whose lists this frame tried to read, read or not.
    let mut tried = BTreeSet::new();
    let choices = calc
        .map(|calc| {
            document
                .layout
                .objects
                .objects
                .iter()
                .filter(|object| names.contains_key(&object.id))
                .filter(|object| {
                    let display = object.field_obj.as_ref().map_or(0, |f| f.display_type);
                    match display {
                        3 | 4 => true,
                        2 => calc.shows_labels(object) || wanted.contains(&object.id),
                        1 => wanted.contains(&object.id),
                        _ => false,
                    }
                })
                .filter_map(|object| {
                    tried.insert(object.id);
                    Some((object.id, calc.option_values(object, NO_RECORD, None).ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    let dates = calc.map(|calc| {
        let cx = calc.context();
        (cx.locale.clone(), cx.today)
    });
    // Entering Find mode, or a new request, puts the focus in the first
    // field of the tab order (layout_objects::first_in_tab_order), here
    // the first that takes a criterion.
    let first = |find: &mut crate::app::FindState| {
        find.take_focus_first()
            .then(|| {
                document
                    .layout
                    .objects
                    .objects
                    .iter()
                    .filter(|object| object.tab_order > 0 && names.contains_key(&object.id))
                    .filter(|object| {
                        object.field_obj.as_ref().is_none_or(|field| {
                            field.field_entry(true) == om_layout::FieldEntry::Edit
                        })
                    })
                    .min_by_key(|object| object.tab_order)
            })
            .flatten()
            .map(|object| object.id)
    };
    let focus = find.take_focus_object().or_else(|| first(find));
    state.find = Some(FindEntry {
        request: find.current().clone(),
        names,
        focus,
        focused: None,
        perform: false,
        drawn: Vec::new(),
        stops: Vec::new(),
        choices,
        lists: calc.is_some(),
        wanted: BTreeSet::new(),
        dates,
    });
    show(ui, session, document, state, ViewMode::Form, None, actions);
    let Some(entry) = state.find.take() else {
        return;
    };
    let layout = document.layout.id;
    for &(place, object, widget) in &entry.stops {
        state.note_tab_stop(place, (layout, FIND_REQUEST, object), widget, false);
    }
    // A list a control opened this frame is read for the next. One being
    // read repaints when its data arrives; one that failed shows none.
    if entry.wanted.iter().any(|id| !tried.contains(id)) {
        ui.ctx().request_repaint();
    }
    state.find_wanted = entry.wanted;
    *find.current_mut() = entry.request;
    // The field chosen was not drawn (another tab panel, a closed popover):
    // the first of the tab order that was takes the focus next frame.
    if entry.focus.is_some() {
        if let Some(&(_, object)) = entry.drawn.iter().min() {
            find.focus_object(object);
        }
    }
    if let Some(field) = &entry.focused {
        find.set_focused_field(field);
    }
    if entry.perform {
        actions.push(Action::PerformFind);
    }
}

/// The criterion each field object of `document` takes in Find mode: the
/// field's name in the layout's occurrence, `Occurrence::Field` in a
/// related one, as Perform Find reads them. Container, summary and global
/// fields take none (Claris "Making a find request"), nor does an object
/// showing a later repetition or a field of an occurrence the layout's
/// cannot reach. Without the schema, the layout's own fields take criteria.
fn find_names(
    document: &RenderLayout,
    schema: Option<&om_data::host::CalcSchema>,
) -> BTreeMap<u64, String> {
    use om_data::host::FieldClass;
    let own = document
        .layout
        .def
        .as_ref()
        .and_then(|def| om_layout::occurrence_id(def.table_alias_key));
    let mut names = BTreeMap::new();
    for object in &document.layout.objects.objects {
        let Some(spec) = object.field_spec().filter(|spec| spec.repetition <= 1) else {
            continue;
        };
        let occurrence = om_layout::occurrence_id(spec.table_key);
        let definition = schema.zip(occurrence).and_then(|(schema, occurrence)| {
            Some((
                schema.occurrence(occurrence)?,
                schema.field(occurrence, spec.field_id()?)?,
            ))
        });
        if definition.is_some_and(|(_, field)| {
            field.class == FieldClass::Summary
                || field.global
                || field.value_type == om_data::calc::ValueType::Container
        }) {
            continue;
        }
        if let Some(name) = document.field_bindings.get(&object.id) {
            names.insert(object.id, name.clone());
            continue;
        }
        let reachable =
            schema
                .zip(own.zip(occurrence))
                .is_some_and(|(schema, (own, occurrence))| {
                    own != occurrence && schema.path(own, occurrence).is_some()
                });
        if let Some((occurrence, field)) = definition.filter(|_| reachable) {
            names.insert(object.id, format!("{}::{}", occurrence.name, field.name));
        }
    }
    names
}

/// A field object in Find mode: an editor of the current request's
/// criterion for its field, in the field's own style, with the find badge
/// (a magnifier) at its right, left of a drop-down arrow. Find mode field entry (layout-model.md 5.4) other than
/// Edit takes no typing; the placeholder shows unless the object hides it
/// in Find mode (field flag `0x10000`, 5.4.1).
fn find_field(
    ui: &mut Ui,
    state: &mut DocumentState,
    (layout, object, rect): (&Layout, &LayoutObject, Rect),
) {
    let tab_entered = state.tab_target == Some((layout.id, FIND_REQUEST, object.id))
        && std::mem::take(&mut state.tab_entry);
    let Some(find) = state.find.as_mut() else {
        return;
    };
    let Some(name) = find.names.get(&object.id).cloned() else {
        return;
    };
    let field = object.field_obj.as_ref();
    let enterable =
        field.is_none_or(|field| field.field_entry(true) == om_layout::FieldEntry::Edit);
    let placeholder = object
        .placeholder
        .as_deref()
        .filter(|_| field.is_none_or(|field| field.flags & 0x1_0000 == 0));
    let id = ui.scope_id().with(("find", layout.id, object.id));
    let mut criterion = find.request.get(&name).unwrap_or_default().to_owned();
    let display = field.map_or(0, |field| field.display_type);
    // As this frame began: Return in an open menu chooses an item.
    let menu_open = egui::Popup::is_id_open(ui.ctx(), popup::menu_id(id));
    let read = find.choices.get(&object.id).cloned();
    // An open menu or list asks for its items (`show_find`).
    let mut want = |open: bool| {
        if open && find.lists {
            find.wanted.insert(object.id);
        }
    };
    let choices = match display {
        3 | 4 => read.clone(),
        2 if find.lists => {
            want(menu_open);
            Some(read.clone().unwrap_or_default())
        }
        _ => None,
    };
    // A pop-up menu, checkbox or radio button set takes the criterion its
    // items make, as Browse's control takes a value: choosing a menu item
    // or a radio button sets it, a checkbox adds or removes its line.
    if let Some(choices) = choices.as_ref() {
        let radio = display == 4;
        let single = field.is_some_and(om_layout::FieldObject::single_unlabeled_checkbox);
        let shown = if display == choice::POP_UP_MENU {
            let empty = criterion.is_empty().then_some(placeholder).flatten();
            let text = field_text_box(
                object,
                layout,
                state.layout_cache.text_style(
                    layout,
                    object,
                    &[],
                    if empty.is_some() { 18 } else { 1 },
                ),
            );
            let arrow = field.is_some_and(om_layout::FieldObject::has_icon);
            // Find mode offers no Other… or Edit…: what Pro offers there is
            // unmeasured (#869).
            popup::show(
                ui,
                id,
                rect,
                &criterion,
                empty,
                (choices, &[]),
                &text,
                arrow,
                enterable,
                true,
                &name,
            )
        } else {
            let styles = options::styles(&state.layout_cache, layout, object, &[], false);
            let single_items = single.then(|| options::single_item(choices));
            options::show(
                ui,
                id,
                rect,
                &criterion,
                single_items.as_deref().unwrap_or(&choices.items),
                radio,
                &styles,
                enterable,
                true,
                &name,
                single,
            )
        };
        let picked = match shown.input {
            Some(options::Input::Open) => {
                egui::Popup::open_id(ui.ctx(), popup::menu_id(id));
                None
            }
            // A single checkbox's criterion is its first value or none.
            Some(options::Input::Click { .. } | options::Input::Space(_)) if single => {
                choice::single_checkbox_criterion(choices, &criterion)
            }
            Some(options::Input::Click { index, shift }) => {
                choices.items.get(index).and_then(|item| {
                    if display == choice::POP_UP_MENU {
                        Some(item.value.clone()).filter(|value| *value != criterion)
                    } else {
                        options::picked(&criterion, &item.value, radio, shift)
                    }
                })
            }
            Some(options::Input::Space(index)) => choices
                .items
                .get(index)
                .and_then(|item| options::picked(&criterion, &item.value, radio, false)),
            Some(options::Input::Command(_)) | None => None,
        };
        if let Some(value) = picked {
            find.request.set(&name, value);
        }
        // A pick makes this the field Insert > Operators goes into.
        if shown.input.is_some() {
            find.focused = Some(name.clone());
        }
        if let Some(set) = shown.set {
            if enterable {
                find.stops.push((object.tab_order, object.id, set.id));
            }
            // Tab is the document's, as in Browse's sets (`paint_option_set`):
            // the set keeps it from egui, with its arrow keys; a pop-up
            // menu keeps no arrow keys, and Tab with its list open does
            // nothing.
            if set.has_focus() {
                let pop_up = display == choice::POP_UP_MENU;
                let keys = egui::EventFilter {
                    tab: true,
                    horizontal_arrows: !pop_up,
                    vertical_arrows: !pop_up,
                    ..Default::default()
                };
                ui.memory_mut(|memory| memory.set_focus_lock_filter(set.id, keys));
            }
            if enterable && object.tab_order > 0 {
                find.drawn.push((object.tab_order, object.id));
            }
            if enterable && find.focus == Some(object.id) {
                find.focus = None;
                set.request_focus();
            }
            if set.has_focus() {
                // Return performs the find, as in a criterion's editor.
                if !menu_open && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    find.perform = true;
                }
                find.focused = Some(name);
            }
        }
        return;
    }
    let list_control = display == choice::DROP_DOWN_LIST && find.lists;
    let calendar = calendar::is_calendar(object);
    let open = (list_control || calendar) && drop_down::is_open(ui, id);
    // A criterion completes from its list as in Browse (#870's reading):
    // entered, its items are asked for as an open list asks for them.
    let completes =
        list_control && !calendar && field.is_some_and(om_layout::FieldObject::auto_completes);
    let entered = ui.memory(|memory| memory.has_focus(id));
    want(list_control && (open || (completes && entered)));
    let list = read.clone().filter(|_| list_control);
    // Entered by Tab: the insertion point goes after the criterion, as
    // in Browse, before the editor takes the keys the Tab's frame held.
    if tab_entered {
        let mut edit = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        edit.cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(criterion.chars().count()),
            )));
        edit.store(ui.ctx(), id);
    }
    // Tab is the document's (`DocumentState::resolve_tab`), as in Browse.
    let response = field_editor(
        ui,
        &state.layout_cache,
        (layout, object, rect),
        id,
        &mut criterion,
        placeholder,
        drop_down::browse_keys(open),
        enterable,
        false,
    );
    if response.changed() && completes {
        if let Some(list) = &list {
            drop_down::complete(ui, id, &mut criterion, list);
        }
    }
    // A drop-down list's items and a calendar's days, chosen, replace the
    // criterion, as typing does.
    let with_arrow = has_drop_down_arrow(object);
    let input = if list_control {
        // Offered, as in Browse, only with items; closed, the list is not
        // read, and its arrow opens it.
        let offered = list.as_ref().is_none_or(|list| !list.items.is_empty());
        drop_down::show(
            ui,
            id,
            rect,
            with_arrow,
            &criterion,
            &list.clone().unwrap_or_default(),
            offered,
            enterable,
            None,
        )
    } else if let (true, Some((locale, today))) = (calendar, &find.dates) {
        let cx = om_data::calc::Ctx::new(locale, *today);
        calendar::show(
            ui, id, rect, with_arrow, &criterion, &cx, true, enterable, None,
        )
    } else {
        None
    };
    // Without its arrow or icon, the list or calendar opens when the field
    // is entered, as in Browse (`drop_down`, `calendar`).
    // Entered, a list without its arrow opens, and reads its items: it
    // offers nothing once they turn out to be none.
    let entered = !with_arrow && enterable && response.gained_focus();
    want(list_control && entered);
    let offered = list_control || (calendar && find.dates.is_some());
    if offered && entered {
        egui::Popup::open_id(ui.ctx(), popup::menu_id(id));
    }
    // Using the arrow or the list makes this the field Insert > Operators
    // goes into; a choice leaves the caret in it.
    if input.is_some() {
        find.focused = Some(name.clone());
    }
    match input {
        Some(drop_down::Input::Open) => egui::Popup::open_id(ui.ctx(), popup::menu_id(id)),
        Some(drop_down::Input::Close) => egui::Popup::close_id(ui.ctx(), popup::menu_id(id)),
        Some(drop_down::Input::Choose(index)) => {
            if let Some(item) = list.as_ref().and_then(|list| list.items.get(index)) {
                criterion.clone_from(&item.value);
                find.request.set(&name, criterion.clone());
                egui::Popup::close_id(ui.ctx(), popup::menu_id(id));
                response.request_focus();
            }
        }
        Some(drop_down::Input::Set(text)) => {
            criterion.clone_from(&text);
            find.request.set(&name, text);
            egui::Popup::close_id(ui.ctx(), popup::menu_id(id));
            response.request_focus();
        }
        None => {}
    }
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::text_edit(enterable, "", &criterion, "");
        info.label = Some(name.clone());
        info
    });
    if response.changed() {
        find.request.set(&name, criterion);
    }
    if enterable {
        find.stops.push((object.tab_order, object.id, response.id));
    }
    if enterable && object.tab_order > 0 {
        find.drawn.push((object.tab_order, object.id));
    }
    if enterable && find.focus == Some(object.id) {
        find.focus = None;
        response.request_focus();
    }
    if response.has_focus() {
        find.focused = Some(name);
    } else if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
        find.perform = true;
    }
    // Left of a drop-down arrow; only where a criterion can be typed, and
    // never on a pop-up menu, checkbox or radio button set (Claris "Making
    // a find request").
    let right = rect.right()
        - if has_drop_down_arrow(object) {
            31.0
        } else {
            11.0
        };
    let badge = field.is_none_or(|field| !matches!(field.display_type, 2..=4));
    if enterable && badge && right - rect.left() > 20.0 {
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(right, rect.center().y), vec2(11.0, 11.0)),
            Icon::Search,
            theme::DISABLED,
            Color32::TRANSPARENT,
        );
    }
}

pub(crate) fn show(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    view: ViewMode,
    calc: Option<&Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    show_document(ui, session, document, state, view, calc, actions);
    // A field painted earlier in this frame took the focus from a completed
    // one (#870): its text is the item's before the frame's triggers and
    // saves read the draft.
    state.settle_completions(ui);
}

fn show_document(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    view: ViewMode,
    calc: Option<&Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    state.resolve_first();
    state.resolve_tab(ui, session, state.find.is_some());
    state.settle_completions(ui);
    take_requests(ui, session, state, actions);
    if view == ViewMode::List {
        // Zoomed as Form view is: its parts and records in the document's
        // zoom layer, in the layout's own coordinates.
        let factor = state.zoom.factor();
        zoom::show(ui, ui.available_size() / factor, factor, |ui| {
            // The canvas behind the rows, in their layer (the window's own
            // canvas is under it): a click there that no row or field
            // takes commits the record, as in Form view.
            let background = ui.interact(
                ui.max_rect(),
                ui.scope_id().with("list_canvas"),
                Sense::click(),
            );
            show_list(ui, session, document, state, calc, actions);
            if background.clicked() && !state.pending && !state.changes(session).is_empty() {
                actions.push(Action::CommitRecord);
            }
        });
        return;
    }
    let layout = &document.layout;
    let width = finite_size(layout.width());
    let height = finite_size(layout.height());
    let background = state
        .layout_cache
        .background_style(layout, 1, 1)
        .background
        .map(color)
        .unwrap_or(Color32::WHITE);
    ui.painter()
        .rect_filled(ui.max_rect(), CornerRadius::ZERO, background);
    let frame = state.scroll.begin(ui, ("document", layout.id), None);
    let mut scroll = egui::ScrollArea::both();
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let scroll = scroll.stick_to_bottom(frame.at_end());
    let output = scroll
        .id_salt(("document", layout.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            state.scroll.origin(ui, ui.max_rect().min);
            let factor = state.zoom.factor();
            let size = vec2(
                width.max(ui.available_width() / factor),
                height.max(ui.available_height() / factor),
            );
            zoom::show(ui, size, factor, |ui| {
                let area = ui.max_rect();
                let background =
                    ui.interact(area, ui.scope_id().with("document_canvas"), Sense::click());
                // An empty field has no editor. Check the canvas click so
                // buttons and other existing hit targets keep their clicks.
                state.empty_field_click = if background.clicked_by(egui::PointerButton::Primary)
                    && session.empty_field_alert().is_some()
                {
                    background.interact_pointer_pos()
                } else {
                    None
                };
                let origin = area.min;
                let growth = vec2(
                    (area.width() - width).max(0.0),
                    (area.height() - height).max(0.0),
                );
                for part in &layout.parts {
                    let height = finite_size(part.height)
                        + if part.kind() == om_layout::PartKind::Body {
                            growth.y
                        } else {
                            0.0
                        };
                    let below_body = layout.parts.iter().any(|body| {
                        body.kind() == om_layout::PartKind::Body && part.top >= body.bottom()
                    });
                    let top = part.top as f32 + if below_body { growth.y } else { 0.0 };
                    let rect =
                        Rect::from_min_size(origin + vec2(0.0, top), vec2(area.width(), height));
                    paint_box(
                        ui,
                        rect,
                        &state.layout_cache.part_style(layout, part, 1, 1),
                        ObjectKind::Rectangle,
                    );
                }
                // An open popover floats above the layout's other objects.
                // It paints last, and only where the traversal reaches it
                // through visible containers. The wheel over it is its own.
                let wheel = hold_popover_wheel(ui, state.popover_surface.take());
                state.popover_paint = PopoverPaint::Defer(None);
                let mut visited = BTreeSet::new();
                for &id in &layout.objects.root {
                    paint_object(
                        ui,
                        session,
                        document,
                        state,
                        calc,
                        origin,
                        growth,
                        id,
                        session.current,
                        None,
                        actions,
                        &mut visited,
                    );
                }
                for object in &layout.objects.objects {
                    if object.parent.is_none() {
                        paint_object(
                            ui,
                            session,
                            document,
                            state,
                            calc,
                            origin,
                            growth,
                            object.id,
                            session.current,
                            None,
                            actions,
                            &mut visited,
                        );
                    }
                }
                release_popover_wheel(ui, wheel);
                if let PopoverPaint::Defer(Some(raised)) = std::mem::take(&mut state.popover_paint)
                {
                    visited.remove(&raised.id);
                    paint_object(
                        ui,
                        session,
                        document,
                        state,
                        calc,
                        raised.origin,
                        raised.growth,
                        raised.id,
                        raised.record_index,
                        raised.portal,
                        actions,
                        &mut visited,
                    );
                }
                state.empty_field_click = None;
                if background.clicked() {
                    state.selected_row = None;
                }
                if background.clicked()
                    && state.find.is_none()
                    && !state.pending
                    && (!state.changes(session).is_empty() || state.related_edited(session))
                {
                    actions.push(Action::CommitRecord);
                }
            });
            frame.hold(ui);
        });
    state.scroll.finish(frame, &output);
}

/// What the layout's fields asked for in the last frame: a refused choice,
/// Edit… on a value list, and Other…'s dialog, which shows here.
fn take_requests(ui: &Ui, session: &Session, state: &mut DocumentState, actions: &mut Vec<Action>) {
    if let Some(error) = state.choice_refusal.take() {
        actions.push(Action::RefuseChoice(error));
    }
    // Edit… saves the record first, as FileMaker Pro 26.0.3 does (#869):
    // the window commits a changed record before the action goes on.
    if let Some(list) = state.edit_list.take() {
        actions.push(Action::EditValueList(list));
    }
    show_other_dialog(ui, session, state);
}

/// An object whose hide condition OpenMaker cannot decide
/// ([`Visibility::Unresolved`]): a dashed outline where it is, without its
/// content, children or action. Not a FileMaker presentation: it keeps the
/// object from silently disappearing without guessing that FileMaker shows
/// it. It takes the click, so nothing under it acts in its place, but not
/// the keyboard focus.
fn unresolved(ui: &Ui, rect: Rect, id: egui::Id, object: &LayoutObject) {
    let kind = om_data::host::layout_objects::object_type(object);
    let label = match object.name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => format!("Undetermined {kind} “{name}”"),
        None => format!("Undetermined {kind}"),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), theme::SEARCH_FILL);
    let outline = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(
        &outline,
        Stroke::new(1.0, theme::DISABLED),
        4.0,
        3.0,
    ));
    let role = if object.script_step().is_some() || object.popover_button.is_some() {
        egui::Role::Button
    } else {
        egui::Role::Label
    };
    let response = ui.interact(rect, id, Sense::CLICK);
    response.widget_info(|| egui::WidgetInfo::labeled(role, false, &label));
    response.on_hover_text(
        "OpenMaker cannot tell whether FileMaker shows this object: its hide condition \
         needs data, account details or functions this window does not have.",
    );
}

/// An open popover takes the pointer from the objects under it; its own
/// objects, registered after it, interact above it.
fn popover_surface(ui: &Ui, rect: Rect, id: egui::Id) {
    ui.interact(rect, id, Sense::click_and_drag());
}

/// The wheel over an open popover is held back from the scroll areas painted
/// before it, such as a portal it covers: egui scrolls an area wherever the
/// pointer is over it in its layer, whatever covers it there. `surface` is
/// the popover's surface as of the last frame, where hit testing places the
/// pointer this frame.
fn hold_popover_wheel(ui: &Ui, surface: Option<egui::Id>) -> Option<egui::Vec2> {
    surface
        .and_then(|id| ui.ctx().read_response(id))
        .is_some_and(|response| response.contains_pointer())
        .then(|| {
            ui.ctx()
                .input_mut(|input| std::mem::take(&mut input.smooth_scroll_delta))
        })
}

/// Returns a held wheel delta before the popover paints, for its own scroll
/// areas and, after them, the document's.
fn release_popover_wheel(ui: &Ui, held: Option<egui::Vec2>) {
    if let Some(delta) = held {
        ui.ctx()
            .input_mut(|input| input.smooth_scroll_delta += delta);
    }
}

fn finite_size(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1_000_000.0) as f32
    } else {
        0.0
    }
}

/// Each List view row's vertical extent: a record's body, or with sorted
/// sub-summary groups, its leading bands, body and trailing bands.
enum Rows {
    Uniform(f32),
    /// Each row's top, then the bottom of the last.
    Grouped(Vec<f32>),
}

impl Rows {
    fn top(&self, index: usize) -> f32 {
        match self {
            Rows::Uniform(height) => index as f32 * height,
            Rows::Grouped(tops) => tops[index],
        }
    }

    fn height(&self, index: usize) -> f32 {
        match self {
            Rows::Uniform(height) => *height,
            Rows::Grouped(tops) => tops[index + 1] - tops[index],
        }
    }
}

fn show_list(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    calc: Option<&Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    use om_layout::PartKind;
    let layout = &document.layout;
    let spacing = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;
    // Sub-summary groups are read once per found set, not per paint. A
    // part shows once per group of a break field the found set is sorted
    // by (docs/re/layout-model.md 4.2), never once per record.
    if let Some(total) = session.list_groups_wanted() {
        match calc {
            Some(calc) => {
                if let Some(groups) = calc.list_groups(layout, total) {
                    session.accept_list_groups(total, groups);
                }
            }
            None if state.calculations_unavailable => session.accept_list_groups(
                total,
                Err("This window has no calculations for the layout".into()),
            ),
            None => {}
        }
    }
    let count = usize::try_from(session.total().unwrap_or(0)).unwrap_or(usize::MAX);
    let list = session.list_groups();
    let (groups, notice) = match &list {
        Some(Ok(Some(groups))) if groups.records.len() == count => (Some(&**groups), None),
        Some(Ok(Some(_))) => (
            None,
            Some("The found set changed while grouping its records"),
        ),
        Some(Ok(None)) | None => (None, None),
        Some(Err(why)) => (None, Some(why.as_str())),
    };
    let body: Vec<_> = layout
        .parts
        .iter()
        .filter(|part| part.kind() == PartKind::Body)
        .collect();
    // Parts are told apart by kind: their ids need not be unique.
    let below = |part: &om_layout::Part| {
        matches!(
            part.kind(),
            PartKind::Footer
                | PartKind::TitleFooter
                | PartKind::BottomNavigation
                | PartKind::TrailingGrandSummary
        )
    };
    let footer: Vec<_> = layout.parts.iter().filter(|part| below(part)).collect();
    let body_height = body
        .iter()
        .map(|part| finite_size(part.height))
        .sum::<f32>()
        .max(1.0);
    let part_height = |index: usize| finite_size(layout.parts[index].height);
    let rows = match groups {
        Some(groups) => {
            let mut tops = Vec::with_capacity(count + 1);
            let mut y = 0.0;
            for record in 0..count {
                tops.push(y);
                y += groups
                    .groups
                    .leading(record)
                    .chain(groups.groups.trailing(record))
                    .map(|band| part_height(band.part))
                    .sum::<f32>()
                    + body_height;
            }
            tops.push(y);
            Rows::Grouped(tops)
        }
        None => Rows::Uniform(body_height),
    };
    let width = finite_size(layout.width()).max(ui.available_width());
    for part in &layout.parts {
        if below(part)
            || matches!(
                part.kind(),
                PartKind::Body | PartKind::LeadingSubSummary | PartKind::TrailingSubSummary
            )
        {
            continue;
        }
        let (rect, _) =
            ui.allocate_exact_size(vec2(width, finite_size(part.height)), Sense::hover());
        paint_part(
            ui,
            session,
            document,
            state,
            calc,
            part,
            rect,
            session.current,
            actions,
        );
    }
    if let Some(why) = notice {
        list_notice(
            ui,
            width,
            &format!("Sub-summary parts are not shown: {why}."),
        );
    }
    let footer_height: f32 = footer.iter().map(|part| finite_size(part.height)).sum();
    let selected = match &rows {
        Rows::Grouped(_) => (session.current as usize).min(count.saturating_sub(1)),
        Rows::Uniform(_) => session.current as usize,
    };
    let selected_top = rows.top(selected);
    let frame = state.scroll.begin(
        ui,
        ("layout_list", layout.id),
        Some(selected_top..=selected_top + rows.height(selected)),
    );
    // The rows take what the footer leaves, however little: egui's least
    // scrolled height (64 points) would push a footer out of view when
    // zoomed in.
    let rows_height = (ui.available_height() - footer_height).max(0.0);
    let mut scroll = egui::ScrollArea::both()
        .id_salt(("layout_list", layout.id))
        .auto_shrink([false, false])
        .max_height(rows_height)
        .min_scrolled_height(rows_height);
    if let Some(offset) = state.list_scroll(session, selected_top, ui) {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let scroll = scroll.stick_to_bottom(frame.at_end());
    let mut paint_rows = |ui: &mut Ui, visible: std::ops::Range<usize>| {
        let origin = ui.max_rect().min - vec2(0.0, rows.top(visible.start));
        state.scroll.origin(ui, origin);
        ui.spacing_mut().item_spacing.y = 0.0;
        let extra = (!visible.contains(&(session.current as usize))
            && (state.active.is_some() || state.focus.is_some())
            && session.record_at(session.current).is_some())
        .then_some(session.current as usize);
        for index in visible.clone().chain(extra) {
            let mut offscreen = (!visible.contains(&index)).then(|| {
                ui.new_child(UiBuilder::new().scope_id(ui.scope_id()).max_rect(
                    Rect::from_min_size(
                        origin + vec2(0.0, rows.top(index)),
                        vec2(width, rows.height(index)),
                    ),
                ))
            });
            let ui = offscreen.as_mut().unwrap_or(ui);
            ui.push_id(("row", index), |ui| {
                let (row, _) =
                    ui.allocate_exact_size(vec2(width, rows.height(index)), Sense::hover());
                let leading: Vec<_> = groups
                    .map(|g| g.groups.leading(index).collect())
                    .unwrap_or_default();
                let trailing: Vec<_> = groups
                    .map(|g| g.groups.trailing(index).collect())
                    .unwrap_or_default();
                let lead_height: f32 = leading.iter().map(|band| part_height(band.part)).sum();
                let record =
                    Rect::from_min_size(row.min + vec2(0.0, lead_height), vec2(width, body_height));
                // Only the record's body selects it; a band is its group's.
                let response = ui.interact(record, ui.scope_id().with("record"), Sense::click());
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::Role::ListItem,
                        true,
                        session.current == index as u64,
                        format!("Record {}", index + 1),
                    )
                });
                if response.clicked() {
                    actions.push(Action::GoToRecord(index as u64));
                }
                if session.record_at(index as u64).is_none() {
                    actions.push(Action::LoadRecordRange(index as u64));
                    ui.painter().text(
                        record.left_center() + vec2(12.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        "Loading…",
                        FontId::proportional(13.0),
                        theme::WEAK_TEXT,
                    );
                    return;
                }
                let mut child = ui.new_child(UiBuilder::new().max_rect(row));
                child.set_clip_rect(ui.clip_rect().intersect(row));
                state.web_row = session.record_at(index as u64).map(|(_, r)| r.id);
                let mut top = record.top();
                for part in &body {
                    let rect = Rect::from_min_size(
                        pos2(row.left(), top),
                        vec2(width, finite_size(part.height)),
                    );
                    paint_part(
                        &mut child,
                        session,
                        document,
                        state,
                        calc,
                        part,
                        rect,
                        index as u64,
                        actions,
                    );
                    top = rect.bottom();
                }
                // The body paints first, so it is the field a scroll or a
                // focus request by name finds.
                let refused = groups.map(|g| &g.refused);
                let mut top = row.top();
                for band in &leading {
                    let rect = Rect::from_min_size(
                        pos2(row.left(), top),
                        vec2(width, part_height(band.part)),
                    );
                    paint_band(
                        &mut child, session, document, state, calc, band, refused, rect, index,
                        actions,
                    );
                    top = rect.bottom();
                }
                let mut top = record.bottom();
                for band in &trailing {
                    let rect = Rect::from_min_size(
                        pos2(row.left(), top),
                        vec2(width, part_height(band.part)),
                    );
                    paint_band(
                        &mut child, session, document, state, calc, band, refused, rect, index,
                        actions,
                    );
                    top = rect.bottom();
                }
                state.web_row = None;
            });
        }
    };
    let output = match &rows {
        Rows::Uniform(height) => scroll.show_rows(ui, *height, count, |ui, visible| {
            paint_rows(ui, visible);
            frame.hold(ui);
        }),
        // As `show_rows` does for one row height.
        Rows::Grouped(tops) => scroll.show_viewport(ui, |ui, viewport| {
            ui.set_height(tops[count]);
            let start = tops[1..].partition_point(|bottom| *bottom <= viewport.min.y);
            let end = (tops[..count].partition_point(|top| *top < viewport.max.y) + 1)
                .min(count)
                .max(start);
            let rect = Rect::from_x_y_ranges(
                ui.max_rect().x_range(),
                ui.max_rect().top() + tops[start]..=ui.max_rect().top() + tops[end],
            );
            ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                ui.skip_ahead_auto_ids(start);
                paint_rows(ui, start..end);
            });
            frame.hold(ui);
        }),
    };
    state.scroll.finish(frame, &output);
    for part in footer {
        let (rect, _) =
            ui.allocate_exact_size(vec2(width, finite_size(part.height)), Sense::hover());
        paint_part(
            ui,
            session,
            document,
            state,
            calc,
            part,
            rect,
            session.current,
            actions,
        );
    }
    ui.spacing_mut().item_spacing.y = spacing;
}

/// Browse's Table view of a drawn layout: the found set's `grid` between the
/// parts the layout's Table View Properties include, the top navigation and
/// header parts above it and the footer and bottom navigation parts below,
/// stacked at the window's bottom, as FileMaker Pro 26.1.1 places them
/// (layout-model.md 4.2). They show the current record, and their web
/// viewers one page each that follows it, as List view's header and footer
/// do.
///
/// The grid paints first: a field focus asked by name finds its cell
/// before a part's copy. A part's objects come after it, over it where
/// they reach past their part (an open popover). Tab goes through the
/// table's cells, as it does without the parts, and from a part's field
/// that has the keyboard to the first cell (Shift-Tab: the last); New
/// Record's focus goes to the table too [I: whether Pro's Tab reaches
/// the parts' fields is unmeasured].
pub(crate) fn show_table(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    calc: Option<&Evaluator<'_>>,
    actions: &mut Vec<Action>,
    grid: impl FnOnce(&mut Ui, &mut DocumentState, &mut Vec<Action>),
) {
    use om_layout::PartKind;
    take_requests(ui, session, state, actions);
    let layout = &document.layout;
    let included = |part: &&om_layout::Part| {
        layout
            .def
            .as_ref()
            .is_some_and(|def| def.table_view_includes(part.kind()))
    };
    let parts: Vec<&om_layout::Part> = layout.parts.iter().filter(included).collect();
    let below = |part: &om_layout::Part| {
        matches!(part.kind(), PartKind::Footer | PartKind::BottomNavigation)
    };
    let area = ui.max_rect();
    let width = finite_size(layout.width()).max(area.width());
    let mut placed = Vec::with_capacity(parts.len());
    let mut top = area.top();
    for part in parts.iter().filter(|part| !below(part)) {
        let height = finite_size(part.height);
        placed.push((
            *part,
            Rect::from_min_size(pos2(area.left(), top), vec2(width, height)),
        ));
        top += height;
    }
    let footer: f32 = parts
        .iter()
        .filter(|part| below(part))
        .map(|part| finite_size(part.height))
        .sum();
    // The grid takes what the parts leave, however little.
    let grid_bottom = (area.bottom() - footer).max(top);
    let mut bottom = grid_bottom;
    for part in parts.iter().filter(|part| below(part)) {
        let height = finite_size(part.height);
        placed.push((
            *part,
            Rect::from_min_size(pos2(area.left(), bottom), vec2(width, height)),
        ));
        bottom += height;
    }
    let rows = Rect::from_x_y_ranges(area.x_range(), top..=grid_bottom);
    ui.scope_builder(UiBuilder::new().max_rect(rows), |ui| {
        grid(ui, state, actions);
    });
    let focus_first = std::mem::take(&mut state.focus_first);
    let stops = state.tab_stops.len();
    for (part, rect) in placed {
        paint_part(
            ui,
            session,
            document,
            state,
            calc,
            part,
            rect,
            session.current,
            actions,
        );
    }
    for stop in &mut state.tab_stops[stops..] {
        stop.departure_only = true;
    }
    state.focus_first = focus_first;
}

/// Why List view shows no sub-summary parts, above its records.
fn list_notice(ui: &mut Ui, width: f32, text: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, theme::SEARCH_FILL);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::HAIRLINE),
    );
    painter.with_clip_rect(rect.intersect(ui.clip_rect())).text(
        rect.left_center() + vec2(12.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        FontId::proportional(12.0),
        theme::CAPTION,
    );
}

/// A sub-summary band of List view row `index`: its part, shown for the
/// group the row's record opens (leading) or closes (trailing). It shows
/// the group as the record's calculations and its group's summaries give
/// it, and takes no clicks, entry or focus (OpenMaker's choice; FileMaker's
/// is unmeasured). A part whose content OpenMaker cannot place in its group
/// shows why instead.
#[allow(clippy::too_many_arguments)]
fn paint_band(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    calc: Option<&Evaluator<'_>>,
    band: &om_layout::pagination::Band,
    refused: Option<&BTreeMap<usize, String>>,
    rect: Rect,
    index: usize,
    actions: &mut Vec<Action>,
) {
    let layout = &document.layout;
    let part = &layout.parts[band.part];
    let mut band_ui = ui.new_child(UiBuilder::new().max_rect(rect));
    band_ui.set_clip_rect(ui.clip_rect().intersect(rect));
    band_ui.disable();
    band_ui.set_opacity(ui.opacity());
    if let Some(why) = refused.and_then(|refused| refused.get(&band.part)) {
        paint_box(
            &band_ui,
            rect,
            &state.layout_cache.part_style(layout, part, 1, 1),
            ObjectKind::Rectangle,
        );
        let text = format!("Sub-summary part not shown: {why}.");
        band_ui
            .interact(rect, band_ui.scope_id().with("refused"), Sense::hover())
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, false, &text));
        paint_note(&band_ui, rect, &text);
        return;
    }
    // A focus request (Go to Field, the first field) is for the record's
    // own fields, not their copies in a band; an open popover belongs to
    // the record too (a band shows none).
    let focus = state.focus.take();
    let focus_first = std::mem::take(&mut state.focus_first);
    let pending_focus = state.pending_focus.take();
    let popover = state.popovers.remove(&layout.id);
    let group = calc.map(|calc| calc.in_group(band.field));
    paint_part(
        &mut band_ui,
        session,
        document,
        state,
        calc,
        part,
        rect,
        index as u64,
        actions,
    );
    drop(group);
    state.focus = focus;
    state.focus_first = focus_first;
    state.pending_focus = pending_focus;
    if let Some(popover) = popover {
        state.popovers.insert(layout.id, popover);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_part(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    calc: Option<&Evaluator<'_>>,
    part: &om_layout::Part,
    rect: Rect,
    record_index: u64,
    actions: &mut Vec<Action>,
) {
    let layout = &document.layout;
    let current = part.kind() == om_layout::PartKind::Body && record_index == session.current;
    paint_box(
        ui,
        rect,
        &state
            .layout_cache
            .part_style(layout, part, 1, if current { 7 } else { 1 }),
        ObjectKind::Rectangle,
    );
    let origin = rect.min - vec2(0.0, part.top as f32);
    let growth = vec2((rect.width() - layout.width() as f32).max(0.0), 0.0);
    let mut seen = BTreeSet::new();
    for object in &layout.objects.objects {
        if object.parent.is_some() {
            continue;
        }
        if !object
            .bounds
            .as_ref()
            .is_some_and(|bounds| bounds.top >= part.top && bounds.top < part.bottom())
        {
            continue;
        }
        paint_object(
            ui,
            session,
            document,
            state,
            calc,
            origin,
            growth,
            object.id,
            record_index,
            None,
            actions,
            &mut seen,
        );
    }
}

pub(crate) fn color(value: Color) -> Color32 {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(
        channel(value.red),
        channel(value.green),
        channel(value.blue),
        channel(value.alpha),
    )
}

/// A relationship's empty add-row is a visual template, not a synthetic
/// record. Drawing it cannot read parent values, queue calculations, run a
/// button or create an editor before related-record editing is supported.
fn paint_portal_template(
    ui: &Ui,
    cache: &layout_cache::LayoutCache,
    layout: &Layout,
    origin: egui::Pos2,
    growth: egui::Vec2,
    id: u64,
    visited: &mut BTreeSet<u64>,
) {
    if !visited.insert(id) {
        return;
    }
    let Some(object) = cache.object(layout, id) else {
        return;
    };
    if matches!(object.kind(), ObjectKind::Portal | ObjectKind::Popover) {
        return;
    }
    paint_template_box(ui, cache, layout, (origin, growth), object);
    for &child in &object.children {
        paint_portal_template(ui, cache, layout, origin, growth, child, visited);
    }
}

/// The web viewers inside portal `portal`, painted once over it, as objects
/// of the layout at their stored places, on the layout's record: Claris,
/// "Working with web viewers on layouts": "You can't place a web viewer in a
/// portal. If you place a web viewer in a portal, the web viewer appears as
/// an object on the layout that overlaps the portal." Its rows skip them
/// (`paint_object`). [I] that FileMaker stores such a viewer as a portal
/// child is unmeasured; a file that does gets the documented result.
///
/// Only what painting would show: a popover's contents never (none opens
/// inside a portal here), a tab or slide control's active panel only, and
/// nothing under a container whose hide condition does not show it on the
/// layout's record, where the viewer itself is painted.
fn paint_portal_web_viewers(
    ui: &mut Ui,
    (session, document, state, calc): (
        &Session,
        &RenderLayout,
        &mut DocumentState,
        Option<&Evaluator<'_>>,
    ),
    (origin, growth): (egui::Pos2, egui::Vec2),
    portal: &LayoutObject,
    record_index: u64,
    actions: &mut Vec<Action>,
    visited: &mut BTreeSet<u64>,
) {
    let layout = &document.layout;
    let shown = |state: &DocumentState, object: &LayoutObject| {
        let has_hide = object.calcs.hide.is_some()
            || object.calcs.undecodable.contains(&om_layout::calcs::HIDE);
        if !has_hide || state.find.is_some() || calc.is_some_and(Evaluator::layout_masked) {
            return true;
        }
        let record = session
            .record_at(record_index)
            .and_then(|(_, r)| u64::try_from(r.id).ok())
            .or_else(|| (session.total() == Some(0)).then_some(NO_RECORD));
        let (Some(calc), Some(record)) = (calc, record) else {
            return false;
        };
        calc.object_state_in_portal(object, record, None, || {
            object_value(session, document, calc, object, record_index, None)
        })
        .is_some_and(|calculated| matches!(calculated.visibility, Visibility::Shown))
    };
    // In the portal's own order (its children's, depth first), which is
    // the order they stack in.
    let mut viewers = Vec::new();
    let mut pending: Vec<(u64, usize)> = portal.children.iter().rev().map(|&id| (id, 0)).collect();
    let mut seen = BTreeSet::new();
    while let Some((id, depth)) = pending.pop() {
        if depth > 64 || !seen.insert(id) {
            continue;
        }
        let Some(object) = state.layout_cache.object(layout, id) else {
            continue;
        };
        if object.web_viewer().is_some() {
            viewers.push(id);
            continue;
        }
        if matches!(object.kind(), ObjectKind::Portal | ObjectKind::Popover)
            || !shown(state, object)
        {
            continue;
        }
        let mut children: Vec<u64> = object.children.clone();
        if matches!(
            object.kind(),
            ObjectKind::TabControl | ObjectKind::SlideControl
        ) {
            let panels: Vec<_> = children
                .iter()
                .filter_map(|id| state.layout_cache.object(layout, *id))
                .filter(|o| matches!(o.kind(), ObjectKind::TabPanel | ObjectKind::SlidePanel))
                .collect();
            let active = state
                .panels
                .get(&(layout.id, object.id))
                .copied()
                .unwrap_or_else(|| default_panel(object, &panels));
            let panel_ids: Vec<u64> = panels.iter().map(|p| p.id).collect();
            children.retain(|id| !panel_ids.contains(id) || *id == active);
        }
        pending.extend(children.iter().rev().map(|&child| (child, depth + 1)));
    }
    for id in viewers {
        paint_object(
            ui,
            session,
            document,
            state,
            calc,
            origin,
            growth,
            id,
            record_index,
            None,
            actions,
            visited,
        );
    }
}

/// Object `object` of a creation row, without its children: the outlines
/// of fields and shapes, no values.
fn paint_template_box(
    ui: &Ui,
    cache: &layout_cache::LayoutCache,
    layout: &Layout,
    (origin, growth): (egui::Pos2, egui::Vec2),
    object: &LayoutObject,
) {
    if let Some(rect) = cache
        .bounds(layout, object.id, growth)
        .map(|rect| rect.translate(origin.to_vec2()))
        .filter(|rect| rect.is_finite() && rect.width() >= 0.0 && rect.height() >= 0.0)
    {
        if matches!(
            object.kind(),
            ObjectKind::Field | ObjectKind::Rectangle | ObjectKind::Oval | ObjectKind::Line
        ) {
            let style = cache.formatted_style(layout, object, &[], 1, 1);
            if object.kind() == ObjectKind::Line {
                line::paint(ui.painter(), rect, object.flags, &style);
            } else {
                paint_box(ui, rect, &style, object.kind());
            }
        }
    }
}

/// Object `id` of a portal row of a record the user began in the creation
/// row (`row`) through record `parent`, or of the creation row itself: a
/// field of the portal's occurrence the user may edit has an editor,
/// which a click enters, or its value-list control, as in a saved row;
/// everything else shows as the creation row does, outlines without
/// values.
#[allow(clippy::too_many_arguments)]
fn paint_new_row(
    ui: &mut Ui,
    (session, document, state, calc): (&Session, &RenderLayout, &mut DocumentState, &Evaluator<'_>),
    (layout, origin, growth): (&Layout, egui::Pos2, egui::Vec2),
    id: u64,
    (row, number): (RelatedRow, usize),
    (parent, record_index): (i64, u64),
    actions: &mut Vec<Action>,
    visited: &mut BTreeSet<u64>,
) {
    let RelatedRow::New {
        occurrence,
        id: new_id,
    } = row
    else {
        return;
    };
    // A container takes a file through the row's own selection (`paint_object`).
    // A field the account's set gives no access has no editor: it shows
    // `<No Access>` as in a saved row (#695).
    let editable = state.layout_cache.object(layout, id).and_then(|object| {
        if calc.layout_masked() || calc.creation_field_denied(object.field_spec()?, occurrence) {
            return None;
        }
        let target = calc.new_related_field(object.field_spec()?, occurrence)?;
        (calc.field_type(object.field_spec()?)? != om_data::calc::ValueType::Container
            && state
                .capabilities
                .can_edit_related(target.occurrence, target.field))
        .then_some(target)
    });
    let Some(target) = editable else {
        let Some(object) = state.layout_cache.object(layout, id) else {
            return;
        };
        if matches!(object.kind(), ObjectKind::Portal | ObjectKind::Popover) {
            visited.insert(id);
            return;
        }
        if object.children.is_empty() || object.script_step().is_some() {
            // Everything else paints as in a saved row, over no record:
            // its fields read empty (as FileMaker reads the creation row),
            // its buttons run in this row (`PortalClick::unsaved`).
            let RelatedRow::New { id: begun, .. } = row else {
                return;
            };
            let (unsaved, record) = if state
                .new_rows(layout.id, parent, occurrence)
                .contains(&begun)
            {
                // A begun record reads its uncommitted values
                // (`Evaluator`'s overlay at `begun_record`).
                (
                    crate::app::UnsavedRow::Begun(begun),
                    crate::calc::begun_record(begun),
                )
            } else {
                (crate::app::UnsavedRow::Creation, NO_RECORD)
            };
            let outer = state.painting_unsaved.replace(unsaved);
            paint_object(
                ui,
                session,
                document,
                state,
                Some(calc),
                origin,
                growth,
                id,
                record_index,
                Some(PortalRow {
                    occurrence,
                    record,
                    number,
                }),
                actions,
                visited,
            );
            state.painting_unsaved = outer;
            return;
        }
        // A group paints its own outline; its fields may still take typing.
        if !visited.insert(id) {
            return;
        }
        paint_template_box(ui, &state.layout_cache, layout, (origin, growth), object);
        for &child in &object.children {
            paint_new_row(
                ui,
                (session, document, state, calc),
                (layout, origin, growth),
                child,
                (row, number),
                (parent, record_index),
                actions,
                visited,
            );
        }
        return;
    };
    if !visited.insert(id) {
        return;
    }
    let Some(object) = state.layout_cache.object(layout, id) else {
        return;
    };
    let Some(rect) = state
        .layout_cache
        .bounds(layout, id, growth)
        .map(|rect| rect.translate(origin.to_vec2()))
        .filter(|rect| rect.is_finite() && rect.width() >= 0.0 && rect.height() >= 0.0)
    else {
        return;
    };
    let begun = state
        .new_rows(layout.id, parent, occurrence)
        .contains(&new_id);
    let portal_row = PortalRow {
        occurrence,
        record: if begun {
            crate::calc::begun_record(new_id)
        } else {
            NO_RECORD
        },
        number,
    };
    let parent_id = u64::try_from(parent).ok();
    // A hide condition is decided in the row, as `paint_object` decides it
    // in a saved row: a hidden field takes nothing here either.
    if object.calcs.hide.is_some() || object.calcs.undecodable.contains(&om_layout::calcs::HIDE) {
        let visibility = parent_id.and_then(|parent_id| {
            calc.object_state_in_portal(object, parent_id, Some(portal_row), || {
                object_value(
                    session,
                    document,
                    calc,
                    object,
                    record_index,
                    Some(portal_row),
                )
            })
        });
        match visibility.map(|calculated| calculated.visibility) {
            Some(Visibility::Shown) => {}
            Some(Visibility::Hidden) => return,
            Some(Visibility::Unresolved) => {
                let id =
                    ui.scope_id()
                        .with((layout.id, id, record_index, portal_row, "unresolved"));
                unresolved(ui, rect, id, object);
                return;
            }
            None => {
                state.visibility_undecided();
                return;
            }
        }
    }
    let style = state
        .layout_cache
        .formatted_style(layout, object, &[], 1, 1);
    paint_box(ui, rect, &style, object.kind());
    let field = RelatedField {
        layout: layout.id,
        parent,
        table: target.table,
        row,
        field: target.cell,
    };
    // A begun record a step created holds what the step set: the editor
    // starts from it.
    let mut target = target;
    if let (true, Some(spec), Some(parent_id)) = (begun, object.field_spec(), parent_id) {
        if let Some(stored) =
            calc.field_value_in_portal(object.id, spec, parent_id, Some(portal_row))
        {
            target.stored = calc.display(&stored);
        }
    }
    let value = state
        .related
        .get(&field)
        .map_or_else(|| target.stored.clone(), |edit| edit.buffer.value.clone());
    let placeholder = object.placeholder.clone();
    let display = object
        .field_obj
        .as_ref()
        .map_or(0, |field| field.display_type);
    let is_calendar = calendar::is_calendar(object)
        && object.field_spec().and_then(|spec| calc.field_type(spec))
            == Some(om_data::calc::ValueType::Date);
    let set =
        (matches!(display, 2..=4) || drop_down::offers_list(object) || is_calendar).then(|| {
            OptionSet {
                object,
                rect,
                record_index,
                portal: Some(portal_row),
                conditions: &[],
                value: &value,
                placeholder: placeholder.as_deref(),
                targeted: false,
                labels: calc.shows_labels(object),
                items: if is_calendar {
                    Items::known(Ok(Default::default()))
                } else {
                    // The record is not in the file yet: a list of the
                    // values related to the row's record offers none.
                    let row = PortalRow {
                        record: NO_RECORD,
                        ..portal_row
                    };
                    Items::read(move || {
                        parent_id.map_or(Err(ChoiceError::Pending), |parent| {
                            calc.option_values(object, parent, Some(row))
                        })
                    })
                },
            }
        });
    // Checkbox and radio button sets and pop-up menus, as in a saved row: a
    // choice begins the record as typing does.
    let set = match set {
        Some(set) if matches!(display, 2..=4) => {
            let current = record_index == session.current;
            paint_related_option_set(ui, state, layout, set, (field, current), &target);
            return;
        }
        set => set,
    };
    let list = set.as_ref().map(|set| RelatedList {
        set,
        calendar: is_calendar.then(|| calc.context()),
        with_arrow: drop_down::has_arrow(object),
    });
    let Some(preview) = related_field(
        ui,
        state,
        (layout, object, rect),
        (field, Some(number as u64), record_index == session.current),
        &target,
        (&value, placeholder.as_deref()),
        list.as_ref(),
    ) else {
        return;
    };
    // An empty field shows its placeholder, in its placeholder style
    // (state 18), as in a saved row, beside its arrow or calendar icon.
    let placeholder = placeholder.as_deref().filter(|_| value.is_empty());
    let shown = placeholder.unwrap_or(&value);
    let state_id = if placeholder.is_some() { 18 } else { 1 };
    let text_style = field_text_box(
        object,
        layout,
        state.layout_cache.text_style(layout, object, &[], state_id),
    );
    let mut text_rect = rect;
    if has_drop_down_arrow(object) {
        text_rect.max.x -= drop_down::ARROW_WIDTH;
        paint_field_icon(ui, object, rect, text_style.color.map(color));
    }
    let cursor = paint_rich_text(ui, text_rect, shown, &text_style, None, Some(&preview));
    if preview.clicked() && preview.interact_pointer_pos().is_some() {
        state.pending_cursor = Some(if placeholder.is_some() {
            egui::text::CCursor::default()
        } else {
            cursor.unwrap_or_default()
        });
    }
}

/// What a click on action button `object` does: `Some(None)` outside a
/// portal; in a portal row, that row of the portal the button is in on
/// `parent`. `None` (no action) in a portal row this cannot identify.
fn button_click(
    layout: &Layout,
    object: u64,
    parent: Option<u64>,
    portal: Option<PortalRow>,
) -> Option<Option<PortalClick>> {
    let Some(row) = portal else {
        return Some(None);
    };
    Some(Some(PortalClick {
        layout: layout.id,
        portal: portals::enclosing(layout, object)?,
        occurrence: row.occurrence,
        parent: parent?,
        record: row.record,
        row: row.number as u64,
        current: true,
        unsaved: None,
    }))
}

/// Whether field `object` on the layout (not in a portal row) has the
/// keyboard focus on the record at `record_index`, in Browse: its control,
/// whether an edit box, pop-up menu, drop-down list, calendar or option
/// set, has the id of the field's editor. Such a field draws in its theme's
/// In Focus state (layout-model.md 6.4). A related field's control has
/// another id (`related_field`), and a container its own selection: both
/// keep the Normal state.
fn field_has_focus(
    ui: &Ui,
    session: &Session,
    layout: &Layout,
    object: &LayoutObject,
    record_index: u64,
    portal: Option<PortalRow>,
    finding: bool,
) -> bool {
    object.kind() == ObjectKind::Field
        && portal.is_none()
        && !finding
        && session.record_at(record_index).is_some_and(|(_, record)| {
            let id = ui.scope_id().with((layout.id, object.id, record.id));
            ui.memory(|memory| memory.has_focus(id))
        })
}

#[allow(clippy::too_many_arguments)]
fn paint_object(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    calc: Option<&Evaluator<'_>>,
    origin: egui::Pos2,
    growth: egui::Vec2,
    id: u64,
    record_index: u64,
    portal: Option<PortalRow>,
    actions: &mut Vec<Action>,
    visited: &mut BTreeSet<u64>,
) {
    if visited.contains(&id) {
        return;
    }
    // A web viewer in a portal is not one of its rows: the portal paints it
    // once, over itself (`paint_portal_web_viewers`).
    if portal.is_some()
        && state
            .layout_cache
            .object(&document.layout, id)
            .is_some_and(|object| object.web_viewer().is_some())
    {
        return;
    }
    // A field object showing several repetitions gives each its editor in
    // Browse (#185). A find request takes one criterion for the field, its
    // editor over the whole object, as before.
    let repetitions = state
        .layout_cache
        .object(&document.layout, id)
        .and_then(|object| object.field_obj.as_ref())
        .filter(|_| state.find.is_none())
        .map_or(1, |field| field.num_of_reps.unsigned_abs().clamp(1, 255));
    for index in 0..repetitions {
        // An object whose hide condition is kept from before the last
        // change paints its children stale too (`paint_object_repetition`),
        // and only those.
        let stale = state.painting_stale;
        paint_object_repetition(
            ui,
            session,
            document,
            state,
            calc,
            origin,
            growth,
            id,
            record_index,
            portal,
            actions,
            visited,
            index,
            repetitions,
        );
        state.painting_stale = stale;
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_object_repetition(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    calc: Option<&Evaluator<'_>>,
    origin: egui::Pos2,
    growth: egui::Vec2,
    id: u64,
    record_index: u64,
    portal: Option<PortalRow>,
    actions: &mut Vec<Action>,
    visited: &mut BTreeSet<u64>,
    repetition_index: u32,
    repetitions: u32,
) {
    if repetition_index == 0 && !visited.insert(id) {
        return;
    }
    let layout = &document.layout;
    let Some(object) = state.layout_cache.object(layout, id) else {
        return;
    };
    let mut repeated;
    let object = if repetitions > 1 {
        repeated = object.clone();
        if let Some(field) = &mut repeated.field_obj {
            if let Some(spec) = &mut field.field_spec {
                spec.repetition = spec
                    .repetition
                    .max(1)
                    .saturating_add(repetition_index as i32);
            }
        }
        &repeated
    } else {
        object
    };
    // Layout mode allows objects in the workspace beyond the layout's
    // design surface. They stay outside Browse, even when a larger window
    // adds room for auto-resizing the objects that belong to the layout.
    if !state.layout_cache.on_layout(layout, id) {
        return;
    }
    // A popover shows only while its button holds it open, and only on the
    // current record (layout-model.md 5.3).
    if object.kind() == ObjectKind::Popover {
        if record_index != session.current || state.popovers.get(&layout.id) != Some(&id) {
            return;
        }
        if let PopoverPaint::Defer(raised) = &mut state.popover_paint {
            raised.get_or_insert(RaisedPopover {
                id,
                origin,
                growth,
                record_index,
                portal,
            });
            return;
        }
    }
    let Some(rect) = state
        .layout_cache
        .bounds(layout, id, growth)
        .map(|rect| rect.translate(origin.to_vec2()))
    else {
        return;
    };
    let rect = if repetitions > 1 {
        let horizontal = object
            .field_obj
            .as_ref()
            .is_some_and(|field| field.num_of_reps < 0);
        let fraction = repetition_index as f32 / repetitions as f32;
        let next = (repetition_index + 1) as f32 / repetitions as f32;
        if horizontal {
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + rect.width() * fraction, rect.top()),
                egui::pos2(rect.left() + rect.width() * next, rect.bottom()),
            )
        } else {
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + rect.height() * fraction),
                egui::pos2(rect.right(), rect.top() + rect.height() * next),
            )
        }
    } else {
        rect
    };
    if !rect.is_finite() || rect.width() < 0.0 || rect.height() < 0.0 {
        return;
    }

    // The object's calculations on its record (layout-model.md 5.5), on no
    // record in an empty found set as FileMaker evaluates them there. An
    // object whose hide condition holds paints nothing, children included,
    // and takes no clicks; so does one whose condition waits for its data,
    // the calculation schema or the record. One OpenMaker cannot decide
    // shows a disabled stand-in.
    let record = session
        .record_at(record_index)
        .and_then(|(_, r)| u64::try_from(r.id).ok());
    let evaluated =
        record.or_else(|| (portal.is_none() && session.total() == Some(0)).then_some(NO_RECORD));
    let has_hide =
        object.calcs.hide.is_some() || object.calcs.undecodable.contains(&om_layout::calcs::HIDE);
    // Find mode has no record to calculate on: every object shows, in its
    // own style. Neither has a layout whose records the set gives no
    // access: no hide condition, conditional format, calculated label or
    // tooltip is evaluated there, and no tooltip shows (#707: FileMaker
    // Pro showed none for that account; their evaluation is unmeasured).
    let finding = state.find.is_some();
    let masked = calc.is_some_and(Evaluator::layout_masked);
    // Without a hide condition an object's formulas only change how it
    // looks (conditional formats, tooltip, label, placeholder, active
    // segment): outside the view nothing of them shows, so they wait until
    // it is scrolled into view, as FileMaker Pro evaluates them when it
    // draws the object (#1120: it opened no file only those of objects
    // outside its window read). One with a hide condition is evaluated
    // still: whether it shows also decides its clicks and focus.
    let unseen = !has_hide && !ui.clip_rect().intersects(rect);
    let calculated = match calc.zip(evaluated) {
        _ if finding || masked || unseen => Some(ObjectCalcState::default()),
        Some((calc, record)) => calc.object_state_in_portal(object, record, portal, || {
            object_value(session, document, calc, object, record_index, portal)
        }),
        None if calc.is_none() && state.calculations_unavailable => Some(ObjectCalcState {
            visibility: Visibility::Unresolved,
            ..ObjectCalcState::default()
        }),
        None => None,
    };
    // A hide condition kept from before the last change while its data is
    // read again: the object shows as it did, but takes no click, focus or
    // typing until the condition is decided afresh.
    if has_hide && calculated.as_ref().is_some_and(|c| c.kept) {
        state.painting_stale = true;
        state.visibility_undecided();
    }
    let segment_name = calculated
        .as_ref()
        .and_then(|calculated| calculated.active_segment.clone());
    let calc_state = match calculated {
        None if has_hide => {
            // Not decided yet: a portal click waiting for this object
            // keeps waiting.
            state.visibility_undecided();
            return;
        }
        None => ObjectState::default(),
        Some(calculated) => match calculated.visibility {
            Visibility::Shown => calculated.state,
            Visibility::Hidden => return,
            Visibility::Unresolved if has_hide => {
                unresolved(
                    ui,
                    rect,
                    ui.scope_id()
                        .with((layout.id, object.id, record_index, portal, "unresolved")),
                    object,
                );
                return;
            }
            Visibility::Unresolved => ObjectState::default(),
        },
    };
    if portal.is_none() && record_index == session.current && !finding {
        if let Some(name) = document.field_bindings.get(&object.id) {
            if let Some((_, record)) = session.record_at(record_index) {
                state.scroll.field_with_id(
                    ui,
                    name,
                    rect,
                    ui.scope_id().with((layout.id, object.id, record.id)),
                );
            }
        }
    }
    let conditions = &calc_state.conditions;

    // The active segment of the bar being painted shows its active state.
    let active = state.segment_bar.is_some_and(|(bar, active)| {
        object.parent == Some(bar) && active == ActiveSegment::Segment(object.id)
    });
    let segment = state
        .segment_bar
        .is_some_and(|(bar, _)| object.parent == Some(bar));
    let style_state =
        if active || matches!(object.kind(), ObjectKind::TabPanel | ObjectKind::SlidePanel) {
            7
        } else if field_has_focus(ui, session, layout, object, record_index, portal, finding) {
            4
        } else {
            1
        };
    let mut style = state
        .layout_cache
        .formatted_style(layout, object, conditions, 1, style_state);
    if !finding && object.kind() == ObjectKind::Text {
        // Browse draws no Text edge without a line style, even when the
        // definition retained its width and color (layout-model.md 6.2).
        for (width, line_style) in style.border_width.iter_mut().zip(style.border_style) {
            if line_style.is_none() {
                *width = None;
            }
        }
    }
    if object.kind() == ObjectKind::Line {
        line::paint(ui.painter(), rect, object.flags, &style);
    } else {
        if !finding
            && portal.is_none()
            && object.parent.is_none()
            && object.rotation == 0
            && object.kind() == ObjectKind::Rectangle
        {
            paint_outer_box_shadow(ui, rect, &style);
        }
        paint_box(ui, rect, &style, object.kind());
    }
    if object.kind() == ObjectKind::Popover {
        let id = ui
            .scope_id()
            .with((layout.id, object.id, record_index, portal, "surface"));
        popover_surface(ui, rect, id);
        state.popover_surface = Some(id);
        state.overlays.push(to_window(ui, rect));
    }
    let tooltip = calc_state
        .tooltip
        .clone()
        .or_else(|| object.tooltip.clone())
        .filter(|t| !t.is_empty() && !masked);
    // The file's Show hand cursor option belongs to the actionable object,
    // including a grouped button (layout-model.md 5.2). Use its actual hit
    // response so transforms and covering popovers decide hover normally.
    let hand_cursor = object.flags & 0x10000 != 0
        && !finding
        && !masked
        && !state.pending
        && !state.clicks_held
        && !state.painting_stale;
    let popover_button = object.popover_button.as_ref().filter(|_| portal.is_none());
    if let Some(button) = popover_button {
        let response = ui.interact(
            rect,
            ui.scope_id()
                .with((layout.id, object.id, record_index, "popover")),
            Sense::click(),
        );
        let response = if hand_cursor
            && response.enabled()
            && layout
                .objects
                .get(button.popover_key)
                .is_some_and(|object| object.kind() == ObjectKind::Popover)
        {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        };
        let text = object_text(
            session,
            object,
            (record_index, !finding),
            calc,
            &calc_state,
            portal,
        );
        let label = if text.is_empty() {
            format!("Popover button {}", object.id)
        } else {
            text
        };
        let open = record_index == session.current
            && state.popovers.get(&layout.id) == Some(&button.popover_key);
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Button,
                true,
                if segment { active } else { open },
                &label,
            )
        });
        ui.ctx()
            .accesskit_node_builder(response.id, |node| node.set_expanded(open));
        if record_index == session.current
            && state.popover_button_focus == Some((layout.id, object.id))
        {
            response.request_focus();
            state.popover_button_focus = None;
        }
        let response = match &tooltip {
            Some(tip) => response.on_hover_text(tip),
            None => response,
        };
        // The button toggles its popover. In List view a row's button makes
        // its record current, where the popover then shows.
        if response.clicked() {
            if open {
                let opener = layout
                    .objects
                    .get(button.popover_key)
                    .and_then(|popover| popover.popover.as_ref())
                    .map(|popover| popover.button_key);
                state.close_popover(layout.id, button.popover_key, opener);
            } else {
                state.popovers.insert(layout.id, button.popover_key);
                if record_index != session.current {
                    actions.push(Action::GoToRecord(record_index));
                }
            }
        }
    } else if let Some(click) = object
        .script_step()
        // Find mode's portal row has no record for a button to act on.
        .filter(|_| !(finding && portals::enclosing(layout, object.id).is_some()))
        .and_then(|_| button_click(layout, object.id, record, portal))
        .map(|click| {
            click.map(|click| PortalClick {
                unsaved: state.painting_unsaved,
                ..click
            })
        })
    {
        // A portal row's button runs in its row: the window's record stays
        // the button's record, the row's related record its active row.
        let response = ui.interact(
            rect,
            ui.scope_id()
                .with((layout.id, object.id, record_index, portal, "button")),
            Sense::click(),
        );
        let response = if hand_cursor && response.enabled() {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        };
        let text = object_text(
            session,
            object,
            (record_index, !finding),
            calc,
            &calc_state,
            portal,
        );
        let label = if text.is_empty() {
            format!("Button {}", object.id)
        } else {
            text
        };
        response.widget_info(|| {
            if segment {
                egui::WidgetInfo::selected(egui::Role::Button, true, active, &label)
            } else {
                egui::WidgetInfo::labeled(egui::Role::Button, true, &label)
            }
        });
        let response = match &tooltip {
            Some(tip) => response.on_hover_text(tip),
            None => response,
        };
        let resumed = click.zip(portal).and_then(|(click, row)| {
            let current = record_index == session.current;
            state.take_portal_click(
                layout.id,
                click.portal,
                (click.parent, current),
                object.id,
                row,
            )
        });
        if let Some(click) = resumed {
            actions.push(Action::RunButton {
                object_id: object.id,
                record_index,
                portal: Some(click),
            });
        } else if response.clicked() && (!state.painting_stale || click.is_some()) {
            // A row shown while its portal's records are read again may
            // show a record that has left the portal, at a number another
            // record has now: its click waits to find its record's row
            // once the records are in, as a held click does, and goes if
            // that record is no longer the portal's (portal_clicks.rs). It
            // is never dropped while the user's row may still be there.
            let click = click.map(|click| PortalClick {
                current: click.current && !state.painting_stale,
                ..click
            });
            actions.push(Action::RunButton {
                object_id: object.id,
                record_index,
                portal: click,
            });
        }
    } else if segment
        && matches!(
            object.kind(),
            ObjectKind::Button | ObjectKind::PopoverButton
        )
    {
        // A segment without an action still exposes its bar's selection,
        // but does not gain a click or focus action.
        let response = ui.interact(
            rect,
            ui.scope_id()
                .with((layout.id, object.id, record_index, portal, "segment")),
            Sense::hover(),
        );
        let text = object_text(
            session,
            object,
            (record_index, !finding),
            calc,
            &calc_state,
            portal,
        );
        let label = if text.is_empty() {
            format!("Button {}", object.id)
        } else {
            text
        };
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), active, &label)
        });
        if let Some(tip) = &tooltip {
            response.on_hover_text(tip);
        }
    } else if let Some(tip) = &tooltip {
        ui.interact(
            rect,
            ui.scope_id()
                .with((layout.id, object.id, record_index, "tooltip")),
            Sense::hover(),
        )
        .on_hover_text(tip);
    }
    match object.kind() {
        ObjectKind::Portal => {
            // Portals are layout-level containers. Never apply an outer
            // row to a nested/unsupported portal object.
            if portal.is_some() {
                return;
            }
            // Find mode: the first row, where criteria for the portal's
            // fields go (its objects are stored in that row).
            if finding {
                // One row: the layout's own visited set, so a portal
                // listing itself among its children paints once.
                for &child in &object.children {
                    paint_object(
                        ui,
                        session,
                        document,
                        state,
                        calc,
                        origin,
                        growth,
                        child,
                        record_index,
                        None,
                        actions,
                        visited,
                    );
                }
                return;
            }
            if let Some(parent) = record {
                let parent_id = session.record_at(record_index).map(|(_, r)| r.id);
                let occurrence = object
                    .portal
                    .as_ref()
                    .and_then(|portal| om_layout::occurrence_id(portal.table_alias_key));
                // A script's pending rows can already exist in a portal
                // whose relationship offers no blank creation row.
                let draft_context = calc
                    .zip(occurrence)
                    .zip(parent_id)
                    .filter(|_| record_index == session.current);
                let new_rows = draft_context.map_or_else(Vec::new, |((_, occurrence), parent)| {
                    state.new_rows(layout.id, parent, occurrence)
                });
                let can_create = draft_context.is_some_and(|((calc, occurrence), _)| {
                    calc.portal_allows_creation(object)
                        && state.capabilities.related.contains_key(&occurrence)
                });
                let table = calc
                    .zip(occurrence)
                    .and_then(|(calc, occurrence)| calc.table_of(occurrence));
                let selected = state
                    .selected_row
                    .filter(|row| {
                        (row.layout, Some(row.parent), row.portal)
                            == (layout.id, parent_id, object.id)
                    })
                    .map(|row| row.record);
                let deleted: Vec<u64> = state
                    .deleted_related
                    .iter()
                    .filter(|(t, _)| Some(*t) == table)
                    .map(|(_, record)| *record)
                    .collect();
                // Typing into the creation row begins the record this id
                // names.
                let next = state.next_new;
                let read = calc.and_then(|calc| calc.portal_records(object, parent));
                let pending = calc.is_some() && read.is_none();
                let records = state.portal_records((layout.id, object.id, parent), read);
                // A selected row whose record the records as read now leave
                // out is not selected any more: Delete Record acts on it.
                let selected = selected.filter(|record| {
                    let kept = pending || records.as_ref().is_some_and(|r| r.contains(record));
                    if !kept {
                        state.selected_row = None;
                    }
                    kept
                });
                let outer_stale = state.painting_stale;
                let shown = portals::show(
                    ui,
                    layout,
                    portals::styles(&state.layout_cache, layout, object),
                    object,
                    rect,
                    (parent, new_rows.len()),
                    (selected, &deleted),
                    calc,
                    (records, pending),
                    |ui, row| {
                        state.painting_stale = outer_stale || row.stale;
                        let mut row_visited = BTreeSet::new();
                        let draft = row.new.and_then(|index| {
                            let id = new_rows
                                .get(index)
                                .copied()
                                .or(can_create.then_some(next))?;
                            let ((calc, occurrence), parent) = draft_context?;
                            Some((id, calc, occurrence, parent))
                        });
                        for &child in &object.children {
                            match (row.context, draft) {
                                (Some(context), _) => paint_object(
                                    ui,
                                    session,
                                    document,
                                    state,
                                    calc,
                                    origin + row.origin_delta,
                                    growth,
                                    child,
                                    record_index,
                                    Some(context),
                                    actions,
                                    &mut row_visited,
                                ),
                                (None, Some((id, calc, occurrence, parent))) => paint_new_row(
                                    ui,
                                    (session, document, state, calc),
                                    (layout, origin + row.origin_delta, growth),
                                    child,
                                    (RelatedRow::New { occurrence, id }, row.number),
                                    (parent, record_index),
                                    actions,
                                    &mut row_visited,
                                ),
                                (None, None) => paint_portal_template(
                                    ui,
                                    &state.layout_cache,
                                    layout,
                                    origin + row.origin_delta,
                                    growth,
                                    child,
                                    &mut row_visited,
                                ),
                            }
                        }
                    },
                );
                state.painting_stale = outer_stale;
                if shown.pending {
                    state.visibility_undecided();
                }
                // A click in a row outside its fields selects the row, on the
                // current record.
                if let (Some((record, number)), Some(parent), Some(occurrence), Some(table)) =
                    (shown.selected, parent_id, occurrence, table)
                {
                    if record_index == session.current {
                        state.selected_row = Some(SelectedRow {
                            layout: layout.id,
                            parent,
                            portal: object.id,
                            occurrence,
                            table,
                            record,
                            number,
                            deletes: object
                                .portal
                                .as_ref()
                                .is_some_and(|portal| portal.flags & PORTAL_ALLOWS_DELETE != 0),
                        });
                    }
                }
                // The creation row's record was begun: the next creation
                // row begins another.
                if state.related.iter().any(|(key, edit)| {
                    matches!(key.row, RelatedRow::New { id, .. } if id == next)
                        && edit.buffer.ui_edited
                }) {
                    state.next_new = next + 1;
                }
                // A List view row's own click sensor lies under the portal.
                if shown.clicked && record_index != session.current {
                    actions.push(Action::GoToRecord(record_index));
                }
            }
            paint_portal_web_viewers(
                ui,
                (session, document, state, calc),
                (origin, growth),
                object,
                record_index,
                actions,
                visited,
            );
            return;
        }
        ObjectKind::TabControl | ObjectKind::SlideControl => {
            let panels: Vec<_> = object
                .children
                .iter()
                .filter_map(|id| state.layout_cache.object(layout, *id))
                .filter(|o| matches!(o.kind(), ObjectKind::TabPanel | ObjectKind::SlidePanel))
                .collect();
            let selected = state
                .panels
                .entry((layout.id, object.id))
                .or_insert_with(|| default_panel(object, &panels));
            if !panels.iter().any(|p| p.id == *selected) {
                *selected = default_panel(object, &panels);
            }
            // Paint only the active panel. Hidden panels contain overlapping
            // field objects and must never bleed into the selected panel.
            let active = *selected;
            let clip = ui.clip_rect();
            let contents = if !finding && object.kind() == ObjectKind::SlideControl {
                slide::paint_contents(ui, layout, object, rect)
            } else {
                None
            };
            if let Some(contents) = contents {
                ui.set_clip_rect(clip.intersect(contents));
            }
            for &child in &object.children {
                if state.layout_cache.object(layout, child).is_some_and(|o| {
                    matches!(o.kind(), ObjectKind::TabPanel | ObjectKind::SlidePanel)
                }) && child != active
                {
                    continue;
                }
                paint_object(
                    ui,
                    session,
                    document,
                    state,
                    calc,
                    origin,
                    growth,
                    child,
                    record_index,
                    portal,
                    actions,
                    visited,
                );
            }
            if contents.is_some() {
                ui.set_clip_rect(clip);
            }
            if object.kind() == ObjectKind::TabControl {
                let selected = state
                    .panels
                    .get_mut(&(layout.id, object.id))
                    .expect("selected panel");
                let focus_record = session
                    .current_record()
                    .filter(|_| !finding && portal.is_none() && record_index == session.current)
                    .map(|(_, record)| record.id);
                let keyboard = focus_record.is_some() && object.tab_order > 0;
                let widget = tab_headers(
                    ui,
                    &state.layout_cache,
                    layout,
                    object,
                    &panels,
                    (rect, keyboard),
                    selected,
                );
                if let Some((record, widget)) = focus_record.zip(widget) {
                    let target = (layout.id, record, object.id);
                    state.note_tab_stop(object.tab_order, target, widget, false);
                }
            } else if !finding && object.shows_slide_dots() {
                let selected = state
                    .panels
                    .get_mut(&(layout.id, object.id))
                    .expect("selected panel");
                slide::navigation(
                    ui,
                    layout,
                    object,
                    &panels,
                    rect,
                    ui.scope_id().with((record_index, portal)),
                    selected,
                );
            }
            return;
        }
        // Its segments paint over it, the active one in its active state.
        ObjectKind::ButtonBar => {
            let active = active_segment(
                layout,
                object,
                segment_name.as_ref(),
                portal.is_some(),
                calc.is_none() && state.calculations_unavailable,
            );
            if let ActiveSegment::Undetermined(why) = active {
                undetermined_segment(
                    ui,
                    rect,
                    ui.scope_id()
                        .with((layout.id, object.id, record_index, portal, "segments")),
                    why,
                );
            }
            let outer = state.segment_bar.replace((object.id, active));
            for &child in &object.children {
                paint_object(
                    ui,
                    session,
                    document,
                    state,
                    calc,
                    origin,
                    growth,
                    child,
                    record_index,
                    portal,
                    actions,
                    visited,
                );
            }
            state.segment_bar = outer;
            return;
        }
        ObjectKind::Text | ObjectKind::Button | ObjectKind::PopoverButton => {
            // "Image only" shows the icon alone and centered (layout-model.md
            // 5.6.1); its stored label stays the button's accessible name.
            let mut rich = object.rich_text.as_ref().map(Cow::Borrowed);
            let track_substitutions = rich.as_ref().is_some_and(|rich| {
                object.merge_source.as_deref().or(object.text.as_deref())
                    == Some(rich.text.as_str())
            });
            let (text, substitutions) = match object.shows_label() {
                true => object_text_with_substitutions(
                    session,
                    object,
                    (record_index, !finding),
                    calc,
                    &calc_state,
                    portal,
                    track_substitutions,
                ),
                false => (String::new(), Vec::new()),
            };
            let text_style = state
                .layout_cache
                .text_style(layout, object, conditions, style_state);
            if !substitutions.is_empty() {
                rich = rich
                    .map(|data| Cow::Owned(rich_text::substitute(&data, &text, &substitutions)));
            }
            if !conditions.is_empty() {
                if let Some(rich) = &mut rich {
                    rich_text::apply_conditional(
                        rich.to_mut(),
                        &state.layout_cache.conditional_text_style(
                            layout,
                            object,
                            conditions,
                            style_state,
                        ),
                    );
                }
            }
            // A calculated icon (FileMaker Pro 26.1) uses the library
            // icon until its formula is evaluated in this Browse context.
            let svg = object
                .active_icon_formula()
                .and(calc_state.icon_svg.as_deref());
            let (label, _) = paint_label_icon(
                ui,
                &state.layout_cache,
                (layout, object),
                rect,
                (&text, &text_style, rich.as_deref()),
                ButtonIconMode::Browse(svg),
                (style_state, style.border_color[0]),
            );
            if let Some(label) = label {
                let _ = paint_rich_text(ui, label, &text, &text_style, rich.as_deref(), None);
            }
        }
        ObjectKind::Graphic => {
            if let Some(graphic) = &object.graphic {
                paint_library_image(
                    ui,
                    &state.layout_cache,
                    layout,
                    graphic.library_key,
                    rect,
                    None,
                );
            }
        }
        ObjectKind::Field if finding => find_field(ui, state, (layout, object, rect)),
        ObjectKind::Field => {
            if state
                .empty_field_click
                .is_some_and(|pos| rect.intersect(ui.clip_rect()).contains(pos))
                && !state.pending
                && !state.clicks_held
                && !state.painting_stale
                && !masked
                && portal.is_none()
                && document.field_bindings.contains_key(&object.id)
                && object.field_obj.as_ref().is_some_and(|field| {
                    field.display_type == 0
                        && field.field_entry(false) == om_layout::FieldEntry::Edit
                })
            {
                state.empty_field_click = None;
                actions.push(Action::ClickEmptyField(layout.id));
            }
            let (mut value, mut rich_value, data) =
                field_content(session, document, calc, object, record_index, portal);
            // The value the editor begins from: a Refresh Object's, or a
            // global's, the session's value, as calculations read it, not
            // the one the record's page holds.
            let targeted_value = portal.is_none()
                && calc.is_some_and(|calc| {
                    calc.refreshed_value(object.id)
                        || object.field_spec().is_some_and(|spec| calc.is_global(spec))
                });
            // A global whose session value is being read again (a script
            // set it) shows the value from before: its editor takes no
            // typing until the value is in, so that no edit begins from the
            // old one. An edit begun before keeps its draft
            // (`FieldBuffer::refresh_targeted`).
            let global_pending = portal.is_none()
                && calc.zip(object.field_spec()).is_some_and(|(calc, spec)| {
                    calc.is_global(spec)
                        && session
                            .record_at(record_index)
                            .and_then(|(_, record)| u64::try_from(record.id).ok())
                            .is_some_and(|record| calc.value_pending(object.id, spec, record, None))
                });
            // A container's draft is a library reference, never shown: the
            // calculations read its file name through the drafts.
            let container =
                portal.is_none() && is_container(session, document, object.id, record_index);
            if let Some(rich) = &mut rich_value {
                rich_text::apply_conditional(
                    rich,
                    &state
                        .layout_cache
                        .conditional_text_style(layout, object, conditions, 1),
                );
            }
            // A cell the account may not see keeps `<No Access>`: no draft
            // replaces it, and it cannot be entered, in a portal row too.
            let denied = masked
                || portal.is_none()
                    && document
                        .field_bindings
                        .get(&object.id)
                        .is_some_and(|name| session.no_access(record_index, name))
                || calc
                    .zip(object.field_spec())
                    .is_some_and(|(calc, spec)| calc.related_no_access(spec, portal));
            let first_repetition = object.field_spec().is_none_or(|spec| spec.repetition <= 1);
            if portal.is_none() && !denied && first_repetition {
                if let (Some(name), Some((_, record))) = (
                    document.field_bindings.get(&object.id),
                    session.record_at(record_index),
                ) {
                    if let Some(buffer) =
                        state.buffers.get_mut(&(layout.id, record.id, name.clone()))
                    {
                        if targeted_value {
                            buffer.refresh_targeted(&value, session.record_page_key(record_index));
                        } else {
                            buffer.refresh_page(
                                bound_value(session, document, object.id, record_index),
                                session.record_page_key(record_index),
                            );
                        }
                        buffer.remember_version(session, record_index);
                        if !container {
                            value.clone_from(&buffer.value);
                        }
                    }
                }
            }
            // A related record's field the user may edit here: in a portal
            // row, or outside portals (the first related record).
            let related = calc
                .zip(record)
                .filter(|_| record_index == session.current && !denied)
                .and_then(|(calc, parent)| {
                    let spec = object.field_spec()?;
                    if calc.field_type(spec)? == om_data::calc::ValueType::Container {
                        return None;
                    }
                    let target = calc.related_record(spec, parent, portal)?;
                    if Some(target.occurrence) == crate::calc::layout_occurrence(session) {
                        let name = target.name.split_once("::")?.1;
                        if session.no_access(record_index, name)
                            || session
                                .record_access_at(record_index)
                                .is_some_and(|access| {
                                    access == 0
                                        || access == 1 && !state.capabilities.globals.contains(name)
                                })
                        {
                            return None;
                        }
                    }
                    state
                        .capabilities
                        .can_edit_related(target.occurrence, target.field)
                        .then_some(target)
                });
            if let (Some(target), Some((_, parent))) = (&related, session.record_at(record_index)) {
                let key = RelatedField {
                    layout: layout.id,
                    parent: parent.id,
                    table: target.table,
                    row: RelatedRow::Record(target.record),
                    field: target.cell,
                };
                if let Some(edit) = state.related.get(&key) {
                    value.clone_from(&edit.buffer.value);
                }
            }
            let value = value.as_str();
            // Data formatting changes the text the field shows, never the
            // value its editor opens with (data-format.md).
            let formatted = calc
                .zip(object.field_spec())
                .and_then(|(calc, spec)| Some((calc.field_type(spec)?, calc.context())))
                .map_or(Cow::Borrowed(value), |(kind, cx)| {
                    data_format::display(value, kind, object.data_format.as_ref(), &cx)
                });
            let placeholder_text = calc_state
                .placeholder
                .clone()
                .or_else(|| object.placeholder.clone());
            // Checkbox and radio button sets show their value list's items,
            // and pop-up menus offer them in a menu (layout-model.md 5.4);
            // one the user may change here takes clicks (and a set keys),
            // never typing. A cell the account may not see draws
            // `<No Access>` as text instead [I].
            if !denied
                && object
                    .field_obj
                    .as_ref()
                    .is_some_and(|field| matches!(field.display_type, 2..=4))
            {
                let items = Items::read(move || {
                    calc.zip(record)
                        .map_or(Err(ChoiceError::Pending), |(calc, parent)| {
                            calc.option_values(object, parent, portal)
                        })
                });
                let set = OptionSet {
                    object,
                    rect,
                    record_index,
                    portal,
                    conditions,
                    value,
                    placeholder: placeholder_text.as_deref(),
                    targeted: targeted_value,
                    labels: calc.is_some_and(|calc| calc.shows_labels(object)),
                    items,
                };
                if let (Some(target), Some((_, parent))) =
                    (&related, session.record_at(record_index))
                {
                    let field = RelatedField {
                        layout: layout.id,
                        parent: parent.id,
                        table: target.table,
                        row: RelatedRow::Record(target.record),
                        field: target.cell,
                    };
                    let current = record_index == session.current;
                    paint_related_option_set(ui, state, layout, set, (field, current), target);
                    return;
                }
                paint_option_set(ui, session, document, state, set, actions);
                return;
            }
            // A drop-down list is an edit box that also offers its value
            // list's items, from its arrow or, without one, when it is
            // entered (drop_down.rs); a drop-down calendar of a date field
            // offers a calendar the same way (calendar.rs).
            let with_arrow = drop_down::has_arrow(object);
            let is_calendar = calendar::is_calendar(object)
                && calc
                    .zip(object.field_spec())
                    .and_then(|(calc, spec)| calc.field_type(spec))
                    == Some(om_data::calc::ValueType::Date);
            let list_control = (drop_down::offers_list(object) || is_calendar).then(|| OptionSet {
                object,
                rect,
                record_index,
                portal,
                conditions,
                value,
                placeholder: placeholder_text.as_deref(),
                targeted: targeted_value,
                labels: false,
                items: if is_calendar {
                    Items::known(Ok(Default::default()))
                } else {
                    Items::read(move || {
                        calc.zip(record)
                            .map_or(Err(ChoiceError::Pending), |(calc, parent)| {
                                calc.option_values(object, parent, portal)
                            })
                    })
                },
            });
            // Why the list offers nothing; with `items`, also from the items,
            // which are read then: once entered, opened or clicked, or the
            // pointer rests on the arrow, never to paint a closed list.
            let list_refusal = |state: &DocumentState, items: bool| {
                list_control
                    .as_ref()
                    .and_then(|set| option_set_refusal(session, document, state, set, items))
            };
            // A related drop-down list or calendar offers its list or
            // calendar beside the related field's editor.
            let related_list =
                related
                    .as_ref()
                    .zip(list_control.as_ref())
                    .map(|(_, set)| RelatedList {
                        set,
                        calendar: calc.filter(|_| is_calendar).map(|calc| calc.context()),
                        with_arrow,
                    });
            // A field the account may see but not change, and one of a
            // record it may not see, are entered as any other, and refuse
            // what is typed (`guarded_editor`).
            let refusal = portal
                .is_none()
                .then(|| {
                    let name = document.field_bindings.get(&object.id)?;
                    let (fields, _) = session.record_at(record_index)?;
                    let field = fields.iter().find(|field| field.name == *name)?;
                    browse_refusal(session, &state.capabilities, field, record_index)
                })
                .flatten();
            let editable = first_repetition
                && portal.is_none()
                && !masked
                && !session.layout_masked()
                && (refusal.is_some()
                    || !denied
                        && session.record_at(record_index).is_some_and(|(fields, _)| {
                            browse_edits(document, fields, &state.capabilities, object.id)
                        }));
            let edit_id = session
                .record_at(record_index)
                .map(|(_, record)| ui.scope_id().with((layout.id, object.id, record.id)));
            let target = session
                .record_at(record_index)
                .map(|(_, record)| (layout.id, record.id, object.id));
            if let Some(id) =
                edit_id.filter(|_| target.is_some() && state.surrender_focus == target)
            {
                ui.memory_mut(|memory| {
                    if memory.has_focus(id) {
                        memory.surrender_focus(id);
                    }
                });
            }
            let pending_focus = record_index == session.current
                && !state.pending
                && target.is_some()
                && state.pending_focus == target;
            if let (Some(target), Some(widget)) = (
                target.filter(|_| {
                    editable
                        && record_index == session.current
                        && !session.is_loading_records()
                        && document.field_bindings.contains_key(&object.id)
                }),
                edit_id,
            ) {
                state.note_first_candidate(object, target, widget);
            }
            let editing = editable
                && document.field_bindings.get(&object.id).is_some_and(|name| {
                    (record_index == session.current && state.focus.as_deref() == Some(name))
                        || pending_focus
                        || edit_id.is_some_and(|id| ui.memory(|memory| memory.has_focus(id)))
                });
            // Container data shows as a picture or a file, never as text
            // (its file name).
            let data = data.filter(|_| !editable);
            // A container the account may not see takes no selection: Export
            // Field Contents would offer its file (#707). Nor does a later
            // repetition: a selection is of the field's first (Insert and
            // Clear write that cell).
            let selectable = container && !editable && !denied && first_repetition;
            let interactive = container::is_interactive(object);
            // An interactive container's PDF view takes the clicks on its
            // page bar and its scrolling: the field's selection goes under
            // it, a click on a page still selecting the field.
            let mut selected = false;
            if selectable && interactive {
                selected = select_container(
                    ui,
                    session,
                    document,
                    state,
                    (layout.id, object.id, record_index),
                    rect,
                    &formatted,
                    actions,
                );
            }
            if let (Some(data), Some(calc)) = (&data, calc) {
                let text = state.layout_cache.text_style(layout, object, conditions, 1);
                let frame = content_box(rect, &style, &text);
                // An interactive container's PDF shows as the PDF view
                // (#200, containers.md 6.6).
                // FileMaker offers it outside portal rows only.
                let view = (interactive && portal.is_none()).then(|| {
                    egui::Id::unique(("container pdf", layout.id, object.id, record_index))
                });
                // Its audio and video play in the window's player, one view
                // per object as its web viewers have: the current record's.
                let no_player = if portal.is_some() {
                    // FileMaker offers interactive containers outside
                    // portal rows only.
                    Some(MEDIA_NOT_IN_PORTALS)
                } else if record_index == session.current {
                    state.media_note
                } else {
                    Some(MEDIA_CURRENT_ONLY)
                };
                let media = interactive
                    .then(|| {
                        container::paint_media(
                            ui,
                            &mut state.pictures,
                            calc,
                            data,
                            frame,
                            &text,
                            no_player,
                            // "Start playback automatically" (containers.md 6.6).
                            object.graphic_format.unwrap_or_default().autoplay(),
                        )
                    })
                    .flatten();
                match media {
                    Some(Some(address)) => {
                        let (input, hovered, focus) =
                            paint_page(ui, &state.web_pages, object.id, None, frame, true);
                        state.web_viewers.push(crate::web_viewer::Placement {
                            object: object.id,
                            // Interaction only: the player's own controls.
                            options: om_layout::WebViewerOptions(0x1),
                            rect: to_window(ui, frame),
                            visible: to_window(ui, ui.clip_rect()),
                            address: Some(address),
                            // One player, following the current record.
                            record: None,
                            current: true,
                            zoom: state.zoom.factor(),
                            input,
                            hovered,
                            focus,
                        });
                    }
                    Some(None) => {}
                    None => container::paint(
                        ui,
                        &mut state.pictures,
                        calc,
                        data,
                        object.graphic_format.unwrap_or_default(),
                        frame,
                        &text,
                        view,
                    ),
                }
            }
            if selectable && !interactive {
                select_container(
                    ui,
                    session,
                    document,
                    state,
                    (layout.id, object.id, record_index),
                    rect,
                    &formatted,
                    actions,
                );
            } else if selected {
                // Its focus ring over the view.
                paint_container_selection(ui, rect);
            }
            // A related container in a portal row of the current record:
            // selected by a click, filled from the Insert menu.
            if let (Some(row), Some(calc), Some(spec), Some((_, parent))) = (
                portal,
                calc,
                object.field_spec(),
                session.record_at(record_index),
            ) {
                let field = om_layout::occurrence_id(spec.table_key).zip(spec.field_id());
                if record_index == session.current
                    && !denied
                    && spec.repetition <= 1
                    && calc.field_type(spec) == Some(om_data::calc::ValueType::Container)
                    && field.is_some_and(|(occurrence, id)| {
                        occurrence == row.occurrence
                            && state.capabilities.can_edit_related(occurrence, id)
                    })
                {
                    let selection = ContainerSelection {
                        layout: layout.id,
                        record: parent.id,
                        object: object.id,
                        field: calc.qualified_name(spec).unwrap_or_default(),
                        row: portals::enclosing(layout, object.id).map(|portal| ContainerRow {
                            portal,
                            occurrence: row.occurrence,
                            record: row.record,
                            number: row.number,
                            unsaved: state.painting_unsaved,
                        }),
                    };
                    select_related_container(ui, state, selection, rect, &formatted);
                }
            }
            let mut preview = None;
            if let (Some(target), Some((_, parent))) = (&related, session.record_at(record_index)) {
                let field = RelatedField {
                    layout: layout.id,
                    parent: parent.id,
                    table: target.table,
                    row: RelatedRow::Record(target.record),
                    field: target.cell,
                };
                match related_field(
                    ui,
                    state,
                    (layout, object, rect),
                    (
                        field,
                        portal.map(|row| row.number as u64),
                        record_index == session.current,
                    ),
                    target,
                    (&formatted, placeholder_text.as_deref()),
                    related_list.as_ref(),
                ) {
                    Some(response) => preview = Some(response),
                    None => return,
                }
            }
            if editable && !editing {
                let response = ui.interact(rect, edit_id.unwrap(), Sense::click());
                let name = &document.field_bindings[&object.id];
                response.widget_info(|| {
                    let mut info = egui::WidgetInfo::text_edit(
                        !state.pending,
                        &*formatted,
                        &*formatted,
                        placeholder_text.as_deref().unwrap_or(""),
                    );
                    info.label = Some(name.clone());
                    info
                });
                if response.clicked() || response.gained_focus() {
                    response.request_focus();
                    state.pending_focus = target;
                    state.pending_cursor = None;
                    if record_index != session.current {
                        actions.push(Action::GoToRecord(record_index));
                    }
                    ui.ctx().request_repaint();
                }
                // A click on a drop-down list's arrow enters the field as a
                // click on its text does, and opens the list once it is
                // entered. Without the arrow, entering it by a click or Tab
                // opens the list.
                if let (Some(set), Some((layout_id, record_id, object_id))) =
                    (&list_control, target)
                {
                    let opens = if with_arrow {
                        response.clicked()
                            && response
                                .interact_pointer_pos()
                                .is_some_and(|pointer| drop_down::arrow(rect).contains(pointer))
                    } else {
                        response.clicked() || response.gained_focus()
                    };
                    // Entered while its items are still being read: the
                    // list opens once they are.
                    if opens && !with_arrow && matches!(set.items(), Err(ChoiceError::Pending)) {
                        drop_down::open_once_read(ui, response.id);
                    }
                    // A list whose items are being read opens once they
                    // are (`apply_option_wait`).
                    if opens && list_refusal(state, false).is_none() && !state.clicks_held {
                        state.option_wait = Some(OptionWait {
                            layout: layout_id,
                            record: record_id,
                            object: object_id,
                            change: OptionChange::Open,
                            fresh: true,
                            seen: true,
                        });
                    }
                }
                state.saw(
                    &response,
                    name,
                    record_index == session.current,
                    Some(object.id),
                );
                preview = Some(response);
            }
            // A related field's list is the related editor's
            // (`related_field`), with its own reason over the arrow.
            if let (Some(set), Some(id), false, None) = (&list_control, edit_id, editing, &related)
            {
                // Why the arrow offers no list, where the pointer rests on it.
                if with_arrow {
                    let arrow = drop_down::arrow(rect);
                    let hover = match &preview {
                        Some(preview) => preview.clone(),
                        None => ui.interact(arrow, id.with("refusal"), Sense::hover()),
                    };
                    if hover
                        .hover_pos()
                        .is_some_and(|pointer| arrow.contains(pointer))
                    {
                        if let Some(refusal) = list_refusal(state, true) {
                            hover.on_hover_text(refusal);
                        }
                    }
                }
                apply_option_wait(ui, session, document, state, set, false, id);
            }
            if editing {
                // Entered: the list's items are read now.
                let drop_down_refusal = list_refusal(state, true);
                if let (Some(name), Some((_, record))) = (
                    document.field_bindings.get(&object.id),
                    session.record_at(record_index),
                ) {
                    // An entered cell of a record the account may not see
                    // still reads `<No Access>`.
                    let server_value = if denied {
                        NO_ACCESS
                    } else if targeted_value {
                        value
                    } else {
                        bound_value(session, document, object.id, record_index)
                    };
                    let edit = state
                        .buffers
                        .entry((layout.id, record.id, name.clone()))
                        .or_insert_with(|| FieldBuffer::new(server_value));
                    if targeted_value {
                        edit.refresh_targeted(server_value, session.record_page_key(record_index));
                    } else {
                        edit.refresh_page(server_value, session.record_page_key(record_index));
                    }
                    edit.remember_version(session, record_index);
                    let buffer = &mut edit.value;
                    if pending_focus {
                        ui.memory_mut(|memory| memory.request_focus(edit_id.unwrap()));
                        // Entered by Tab: the insertion point goes after
                        // the text, or a calendar's text is selected, before
                        // the editor takes this frame's keys, which the
                        // Tab's frame held for it.
                        if std::mem::take(&mut state.tab_entry) {
                            let mut edit = egui::TextEdit::load_state(ui.ctx(), edit_id.unwrap())
                                .unwrap_or_default();
                            edit.cursor
                                .set_char_range(Some(tab_entry_range(object, buffer)));
                            edit.store(ui.ctx(), edit_id.unwrap());
                        }
                        if let Some(cursor) = state.pending_cursor.take() {
                            let mut edit = egui::TextEdit::load_state(ui.ctx(), edit_id.unwrap())
                                .unwrap_or_default();
                            edit.cursor
                                .set_char_range(Some(egui::text::CCursorRange::one(cursor)));
                            edit.store(ui.ctx(), edit_id.unwrap());
                        }
                        state.pending_focus = None;
                    }
                    let list_id = edit_id.filter(|_| list_control.is_some());
                    let keep =
                        list_id.is_some_and(|id| drop_down::keeps_focus(ui, id, rect, with_arrow));
                    let list_open = list_id.is_some_and(|id| drop_down::is_open(ui, id));
                    let editor = ui.scope_id().with((layout.id, object.id, record.id));
                    return_goes_on(ui, &mut state.tab_held, object, editor);
                    let cache = &state.layout_cache;
                    let interactive = !state.pending && !global_pending;
                    let (response, changed) =
                        guarded_editor(ui, editor, buffer, refusal, actions, |ui, buffer| {
                            field_editor(
                                ui,
                                cache,
                                (layout, object, rect),
                                editor,
                                buffer,
                                placeholder_text.as_deref(),
                                drop_down::browse_keys(list_open),
                                interactive,
                                types_line_breaks(object),
                            )
                        });
                    if response.has_focus() {
                        state.entered_refusal = refusal.map(|refusal| (editor, refusal));
                    }
                    response.widget_info(|| {
                        let mut info = egui::WidgetInfo::text_edit(
                            !state.pending,
                            value,
                            buffer.as_str(),
                            placeholder_text.as_deref().unwrap_or(""),
                        );
                        info.label = Some(name.clone());
                        info
                    });
                    if record_index == session.current
                        && !session.is_loading_records()
                        && (state.focus.as_deref() == Some(name) || pending_focus)
                    {
                        response.request_focus();
                        state.scroll.resolve_focus(name, response.id);
                        if state.select_all {
                            if let Some(mut edit) =
                                egui::TextEdit::load_state(ui.ctx(), response.id)
                            {
                                edit.cursor.set_char_range(Some(
                                    egui::text::CCursorRange::select_all_str(buffer),
                                ));
                                edit.store(ui.ctx(), response.id);
                            }
                        }
                        state.focus = None;
                        state.focus_first = false;
                        state.select_all = false;
                    }
                    if response.gained_focus() && record_index != session.current {
                        actions.push(Action::GoToRecord(record_index));
                    }
                    if changed {
                        edit.ui_edited = true;
                    }
                    if let (Some(set), Some(id)) = (&list_control, list_id) {
                        if keep {
                            response.request_focus();
                        }
                        // The entered field's items, read as its list reads
                        // them (#751; a hosted file's one request, #786),
                        // complete what is typed (#870); none while read.
                        let choices = set.items.shown();
                        let completes = !is_calendar
                            && object
                                .field_obj
                                .as_ref()
                                .is_some_and(om_layout::FieldObject::auto_completes);
                        if changed && completes {
                            if let Some(item) =
                                drop_down::complete(ui, editor, &mut edit.value, &choices)
                            {
                                edit.ui_edited = true;
                                state.completed = Some(Completion {
                                    key: (layout.id, record.id, name.clone()),
                                    editor,
                                    text: edit.value.clone(),
                                    item,
                                });
                            }
                        }
                        // Left this frame (a click that also saves): the
                        // item's text before the save reads the draft.
                        if response.lost_focus()
                            && state.held_focus.is_none()
                            && state.pending_focus.is_none()
                            && state.completed.as_ref().is_some_and(|c| c.editor == editor)
                        {
                            if let Some(completion) = state.completed.take() {
                                edit.accept_completion(&completion);
                            }
                        }
                        let typed = edit.value.clone();
                        let enabled = !state.pending && !state.clicks_held;
                        let input = match calc.filter(|_| is_calendar) {
                            Some(calc) => calendar::show(
                                ui,
                                id,
                                rect,
                                with_arrow,
                                &typed,
                                &calc.context(),
                                drop_down_refusal.is_none(),
                                enabled,
                                drop_down_refusal.as_deref(),
                            ),
                            None => drop_down::show(
                                ui,
                                id,
                                rect,
                                with_arrow,
                                &typed,
                                &choices,
                                drop_down_refusal.is_none(),
                                enabled,
                                drop_down_refusal.as_deref(),
                            ),
                        };
                        let change = match input {
                            Some(drop_down::Input::Open) => Some(OptionChange::Open),
                            Some(drop_down::Input::Close) => {
                                egui::Popup::close_id(ui.ctx(), popup::menu_id(id));
                                None
                            }
                            Some(drop_down::Input::Choose(index)) => {
                                choices.items.get(index).map(|item| OptionChange::Pick {
                                    value: item.value.clone(),
                                    shift: false,
                                })
                            }
                            Some(drop_down::Input::Set(value)) => Some(OptionChange::Pick {
                                value,
                                shift: false,
                            }),
                            // Without its arrow, the list opens as the
                            // field is entered other than through its
                            // preview above (Go to Field), or once the
                            // items of a field entered while they were
                            // being read are read.
                            None if !with_arrow => {
                                let pending = matches!(set.items(), Err(ChoiceError::Pending));
                                if response.gained_focus() && pending {
                                    drop_down::open_once_read(ui, id);
                                }
                                let waited =
                                    drop_down::opens_once_read(ui, id, drop_down_refusal.is_none());
                                (drop_down_refusal.is_none()
                                    && !drop_down::is_open(ui, id)
                                    && (response.gained_focus() || waited))
                                    .then_some(OptionChange::Open)
                            }
                            None => None,
                        };
                        if let Some(change) = change {
                            state.option_wait = Some(OptionWait {
                                layout: layout.id,
                                record: record.id,
                                object: object.id,
                                change,
                                fresh: true,
                                seen: true,
                            });
                        }
                        state.saw(
                            &response,
                            name,
                            record_index == session.current,
                            Some(object.id),
                        );
                        apply_option_wait(ui, session, document, state, set, false, id);
                        // After every change of the focus and of the list
                        // this frame: the editor keeps Escape exactly while
                        // the list is open. Restoring the focus above, or
                        // opening or closing the list, would otherwise
                        // leave the next frame with the other filter.
                        let open = drop_down::is_open(ui, id);
                        ui.memory_mut(|memory| {
                            memory.set_focus_lock_filter(id, drop_down::browse_keys(open));
                        });
                        if open && ui.memory(|memory| memory.has_focus(id)) {
                            state.editor_list = Some(popup::menu_id(id));
                        }
                        return;
                    }
                    state.saw(
                        &response,
                        name,
                        record_index == session.current,
                        Some(object.id),
                    );
                    return;
                }
            }
            if data.is_some() {
                return;
            }
            let placeholder = value
                .is_empty()
                .then_some(placeholder_text.as_deref())
                .flatten();
            let shown = placeholder.unwrap_or(&formatted);
            let mut text_style = state.layout_cache.text_style(
                layout,
                object,
                if placeholder.is_some() {
                    &[]
                } else {
                    conditions
                },
                if placeholder.is_some() { 18 } else { 1 },
            );
            text_style = field_text_box(object, layout, text_style);
            if placeholder.is_some() && is_container(session, document, object.id, record_index) {
                text_style.text_align = Some(2);
                text_style.vertical_align = Some(1);
            }
            let mut text_rect = rect;
            if let Some(field) = &object.field_obj {
                if has_drop_down_arrow(object) {
                    text_rect.max.x -= 20.0;
                    paint_field_icon(ui, object, rect, text_style.color.map(color));
                }
                let cursor = if field.display_type == 7 {
                    paint_rich_text(
                        ui,
                        text_rect,
                        &"•".repeat(shown.chars().count()),
                        &text_style,
                        None,
                        preview.as_ref(),
                    )
                } else {
                    paint_rich_text(
                        ui,
                        text_rect,
                        shown,
                        &text_style,
                        rich_value.as_ref().filter(|_| formatted == value),
                        preview.as_ref(),
                    )
                };
                if preview.as_ref().is_some_and(|response| {
                    response.clicked() && response.interact_pointer_pos().is_some()
                }) {
                    state.pending_cursor = Some(if placeholder.is_some() {
                        egui::text::CCursor::default()
                    } else if formatted != value {
                        // No offset in the formatted text addresses the
                        // value the editor opens with: start at its end.
                        egui::text::CCursor::new(value.chars().count())
                    } else {
                        cursor.unwrap_or_default()
                    });
                }
            }
        }
        // A web viewer's page is a native view over the object (web_viewer
        // reconciles it with what is noted here); beneath it the object keeps
        // the file's styling. Each List row shows its record's page in a view
        // of its own, while the row is in sight; a List's header and footer,
        // like Form view, show the current record's. One in a portal shows
        // once, as a layout object (`paint_portal_web_viewers`).
        ObjectKind::External => {
            if let Some(options) = object.web_viewer() {
                // On a layout whose records the set gives no access, its
                // address is not calculated and no page is shown [I: #707,
                // unmeasured].
                let note = if masked {
                    Some(NO_ACCESS)
                } else if state.web_viewers.len() >= MAX_WEB_VIEWERS {
                    Some("Web viewer: this window shows too many web viewers.")
                } else if !crate::web_viewer::fits(to_window(ui, rect), ui.ctx().pixels_per_point())
                {
                    Some("Web viewer: too large to show.")
                } else {
                    state.web_note
                };
                let row = state.web_row;
                let shown = row.is_none() || ui.clip_rect().intersects(rect);
                match note {
                    Some(note) => paint_web_viewer_note(ui, rect, note),
                    None if shown && (row.is_some() || record_index == session.current) => {
                        let address = match (calc, record) {
                            (Some(calc), Some(record)) => calc.web_address(object, record),
                            // Without calculations only an empty address is known.
                            _ => object.calcs.web_address.is_none().then(String::new),
                        };
                        let (input, hovered, focus) = paint_page(
                            ui,
                            &state.web_pages,
                            object.id,
                            row,
                            rect,
                            options.allows_interaction(),
                        );
                        state.web_viewers.push(crate::web_viewer::Placement {
                            object: object.id,
                            options,
                            rect: to_window(ui, rect),
                            visible: to_window(ui, ui.clip_rect()),
                            address,
                            record: row,
                            current: record_index == session.current,
                            zoom: state.zoom.factor(),
                            input,
                            hovered,
                            focus,
                        });
                    }
                    None => {}
                }
            }
        }
        // Charts and plug-in objects keep only the file's styling: no
        // placeholder artwork that would compete with the layout's own icons.
        _ => {}
    }
    for &child in &object.children {
        paint_object(
            ui,
            session,
            document,
            state,
            calc,
            origin,
            growth,
            child,
            record_index,
            portal,
            actions,
            visited,
        );
    }
}

/// What a related drop-down list or calendar offers beside its field's
/// editor (`related_field`, #537): the list's items or a calendar (the
/// file's locale and today), its arrow or icon, whether the items are
/// still being read, and why it offers nothing.
struct RelatedList<'s, 'a> {
    /// The control, whose items are read once asked for: entered, opened
    /// or clicked.
    set: &'s OptionSet<'a>,
    calendar: Option<om_data::calc::Ctx<'s>>,
    with_arrow: bool,
}

impl RelatedList<'_, '_> {
    fn choices(&self) -> std::sync::Arc<Choices> {
        self.set.items.shown()
    }

    /// Whether the items are still being read.
    fn pending(&self) -> bool {
        matches!(self.set.items(), Err(ChoiceError::Pending))
    }
}

/// Why value-list control `set`, showing related record `target`'s field,
/// offers no choice (`choice::related_availability`; with `items`, which
/// are read then, also a list without items), or `None`.
fn related_refusal(
    state: &DocumentState,
    set: &OptionSet<'_>,
    target: &crate::calc::RelatedRecord,
    items: bool,
) -> Option<String> {
    let may_change = state
        .capabilities
        .can_edit_related(target.occurrence, target.field);
    if let Err(error) = choice::related_availability(set.object, may_change) {
        return Some(error.message());
    }
    if calendar::is_calendar(set.object) || !items {
        return None;
    }
    items_refusal(set.items())
}

/// Why a control offers no items: they could not be read (or are still
/// being read), or the list has none.
fn items_refusal(items: &Result<std::sync::Arc<Choices>, ChoiceError>) -> Option<String> {
    match items {
        Err(error) => Some(error.message()),
        Ok(choices) if choices.items.is_empty() => Some("The value list has no items.".into()),
        Ok(_) => None,
    }
}

/// A checkbox or radio button set or a pop-up menu showing `field` of a
/// related record (`target`, as last read; #537). As the related field's
/// edit box does (`related_field`), a choice is the related record's edit,
/// committed with the record the layout shows; it is made at once, with
/// no per-object triggers.
fn paint_related_option_set(
    ui: &mut Ui,
    state: &mut DocumentState,
    layout: &Layout,
    set: OptionSet<'_>,
    (field, current): (RelatedField, bool),
    target: &crate::calc::RelatedRecord,
) {
    let object = set.object;
    let display = object
        .field_obj
        .as_ref()
        .map_or(0, |field| field.display_type);
    let radio = display == 4;
    let pop_up = display == choice::POP_UP_MENU;
    let id = ui.scope_id().with(("related", field, object.id));
    let number = set.portal.map(|row| row.number as u64);
    // As `paint_option_set`: a closed pop-up menu reads no items unless it
    // shows their labels.
    let read = !pop_up || set.labels || egui::Popup::is_id_open(ui.ctx(), popup::menu_id(id));
    let refusal = related_refusal(state, &set, target, read);
    let editable = refusal.is_none();
    let scripted = state.scripted_value(&field);
    if let (Some(edit), RelatedRow::Record(_)) = (state.related.get_mut(&field), field.row) {
        edit.refresh(target, scripted);
    }
    let value = state
        .related
        .get(&field)
        .map_or_else(|| target.stored.clone(), |edit| edit.buffer.value.clone());
    let choices = match read {
        true => set.items.shown(),
        false => Default::default(),
    };
    let items = &choices.items;
    let single = object
        .field_obj
        .as_ref()
        .is_some_and(om_layout::FieldObject::single_unlabeled_checkbox);
    // Not in a row shown while its portal's records are read again.
    let enabled = !state.pending && !state.clicks_held && !state.painting_stale;
    let label = target.name.as_str();
    // A set of the current record's portal row is a Tab stop (#859). One a
    // Tab goes to takes the focus, in view, before it reads this frame's
    // keys, which then include those typed since the Tab.
    if current && editable {
        state.note_related_stop((layout, object), field, number, id);
        if state.reach_tab_goal(ui, (field, object.id), id) {
            ui.scroll_to_rect(set.rect, None);
        }
    }
    let shown = if pop_up {
        let placeholder = value.is_empty().then_some(set.placeholder).flatten();
        let text = match placeholder {
            Some(_) => state.layout_cache.text_style(layout, object, &[], 18),
            None => state
                .layout_cache
                .text_style(layout, object, set.conditions, 1),
        };
        // Inset as FileMaker Pro insets a pop-up's text (#721).
        let text = field_text_box(object, layout, text);
        let arrow = object
            .field_obj
            .as_ref()
            .is_some_and(|field| field.has_icon());
        let commands = match editable {
            true => choice::menu_commands(object),
            false => Vec::new(),
        };
        popup::show(
            ui,
            id,
            set.rect,
            &value,
            placeholder,
            (&*choices, &commands),
            &text,
            arrow,
            editable,
            enabled,
            label,
        )
    } else {
        // A related field's set keeps its normal state (#753's first slice).
        let styles = options::styles(&state.layout_cache, layout, object, set.conditions, false);
        let single_items = single.then(|| options::single_item(&choices));
        options::show(
            ui,
            id,
            set.rect,
            &value,
            single_items.as_deref().unwrap_or(items),
            radio,
            &styles,
            editable,
            enabled,
            label,
            single,
        )
    };
    if let Some(refusal) = &refusal {
        let response = ui.interact(set.rect, id.with("refusal"), Sense::hover());
        response.on_hover_text(refusal.as_str());
    }
    let Some(response) = shown.set else {
        return;
    };
    // A step put the focus in this set of this row (Go to Field, Go to
    // Object): it takes the keyboard as a related edit box does. A pop-up
    // menu's list stays closed, also after Go to Field [Select/perform]:
    // FileMaker Pro 26 leaves it so (#866). The request waits while the
    // set has no control to take it (out of view, its items still being
    // read) and while the row is one shown before its portal's records
    // were read again, whose number may belong to another record now.
    if !state.painting_stale
        && state.portal_focus.is_some_and(|focus| {
            (focus.layout, focus.parent, focus.field, Some(focus.row))
                == (layout.id, field.parent, object.id, number)
        })
    {
        state.portal_focus = None;
        response.request_focus();
    }
    if enabled && response.clicked() && response.interact_pointer_pos().is_some() {
        response.request_focus();
    }
    // A stale row's pop-up click waits for the same related record,
    // as a click on a related edit box does (#1099).
    if pop_up {
        if state.painting_stale && response.clicked() {
            state.field_click = Some(FieldClickWait {
                field,
                object: object.id,
                seen: true,
            });
        }
        if let Some(wait) = state
            .field_click
            .as_mut()
            .filter(|wait| wait.object == object.id && wait.field == field)
        {
            wait.seen = true;
            if enabled {
                state.field_click = None;
                response.request_focus();
                egui::Popup::open_id(ui.ctx(), popup::menu_id(id));
                ui.ctx().request_repaint();
            }
        }
    }
    // It keeps Tab from egui, as the record's own sets do (#859).
    if current && editable && response.has_focus() {
        let keys = egui::EventFilter {
            tab: true,
            horizontal_arrows: !pop_up,
            vertical_arrows: !pop_up,
            ..Default::default()
        };
        ui.memory_mut(|memory| memory.set_focus_lock_filter(response.id, keys));
    }
    let Some(input) = shown.input.filter(|_| enabled) else {
        return;
    };
    // Any newer interaction with a ready control supersedes the waiting click.
    state.field_click = None;
    let chosen = match input {
        options::Input::Open => {
            response.request_focus();
            egui::Popup::open_id(ui.ctx(), popup::menu_id(id));
            None
        }
        options::Input::Click { .. } | options::Input::Space(_) if single => {
            match choice::single_checkbox(&choices, &value) {
                Ok(value) => value,
                Err(error) => {
                    state.choice_refusal = Some(error);
                    None
                }
            }
        }
        options::Input::Click { index, shift } => items.get(index).and_then(|item| {
            if pop_up {
                Some(item.value.clone())
            } else {
                options::picked(&value, &item.value, radio, shift)
            }
        }),
        options::Input::Space(index) => items
            .get(index)
            .and_then(|item| options::picked(&value, &item.value, radio, false)),
        options::Input::Command(MenuCommand::Other) => {
            let text = match choice::other_checked(&choices, &value) {
                true => value.clone(),
                false => String::new(),
            };
            let target = value_dialogs::OtherTarget::Related {
                field,
                occurrence: target.occurrence,
                version: target.version,
                stored: target.stored.clone(),
            };
            state.other_dialog = Some(value_dialogs::OtherDialog::new(
                target,
                object.clone(),
                text,
            ));
            None
        }
        options::Input::Command(MenuCommand::Edit) => {
            match choice::edit_value_list(object) {
                Ok(list) => state.edit_list = Some(list),
                Err(error) => state.choice_refusal = Some(error),
            }
            None
        }
    };
    if let Some(chosen) = chosen {
        let scripted = state.scripted_value(&field);
        let edit = state.related.entry(field).or_insert_with(|| {
            RelatedBuffer::new(target.occurrence, target.version, &target.stored, scripted)
        });
        edit.buffer.value = chosen;
        edit.buffer.ui_edited = true;
        ui.ctx().request_repaint();
    }
}

/// Field object `object` showing `field` of a related record (`target`,
/// as last read): its editor while it has the focus (`None`), else the
/// click and focus sensor over its text, which a click or the keyboard's
/// focus turns into the editor (`Some`). The edit belongs to the record
/// the layout shows and is committed with it. A drop-down list or
/// calendar (`list`) offers its list or calendar beside the editor, as
/// the layout's own field does (`drop_down`, `calendar`); a choice there
/// replaces the edit's text.
#[allow(clippy::too_many_arguments)]
fn related_field(
    ui: &mut Ui,
    state: &mut DocumentState,
    (layout, object, rect): (&Layout, &LayoutObject, Rect),
    (field, number, current): (RelatedField, Option<u64>, bool),
    target: &crate::calc::RelatedRecord,
    (shown, placeholder): (&str, Option<&str>),
    list: Option<&RelatedList<'_, '_>>,
) -> Option<egui::Response> {
    // Each object showing the field has its own editor over one edit.
    let id = ui.scope_id().with(("related", field, object.id));
    // The field a trigger's script held: this editor, showing its record,
    // takes the focus back once the script ended (`release_related_focus`).
    if let Some(held) = state
        .held_related
        .as_mut()
        .filter(|held| held.focus.object == object.id && held.focus.field == field)
    {
        held.seen = true;
        if held.released && current {
            ui.memory_mut(|memory| memory.request_focus(id));
        }
    }
    // The user clicked this field in this record's row while the portal's
    // records were read again: it takes the focus once the row shows the
    // records as read now and nothing holds clicks (#1099).
    if let Some(wait) = state
        .field_click
        .as_mut()
        .filter(|wait| wait.object == object.id && wait.field == field)
    {
        wait.seen = true;
        if !state.painting_stale && !state.pending && !state.clicks_held {
            state.field_click = None;
            ui.memory_mut(|memory| memory.request_focus(id));
            state.related_click = Some(id);
            state.pending_cursor = None;
            ui.ctx().request_repaint();
        }
    }
    // A step put the focus in this field of this row.
    if let Some(focus) = state.portal_focus.filter(|focus| {
        (focus.layout, focus.parent, focus.field, Some(focus.row))
            == (layout.id, field.parent, object.id, number)
    }) {
        state.portal_focus = None;
        ui.memory_mut(|memory| memory.request_focus(id));
        state.related_select = focus.select.then_some(id);
        state.related_click = None;
    }
    // A record begun in a creation row has no stored values to refresh from.
    let scripted = state.scripted_value(&field);
    if let (Some(edit), RelatedRow::Record(_)) = (state.related.get_mut(&field), field.row) {
        edit.refresh(target, scripted);
    }
    // A field of the current record is a Tab stop (#859); one a Tab goes
    // to takes the focus now, its insertion point after its text.
    if current {
        state.note_related_stop((layout, object), field, number, id);
        if state.reach_tab_goal(ui, (field, object.id), id) {
            state.related_entry = Some(id);
        }
    }
    if ui.memory(|memory| memory.has_focus(id)) {
        // A field of the row has the focus, not the row.
        state.selected_row = None;
    } else {
        let response = ui.interact(rect, id, Sense::click());
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::text_edit(
                !state.pending,
                shown,
                shown,
                placeholder.unwrap_or(""),
            );
            info.label = Some(target.name.clone());
            info
        });
        // A row shown while its portal's records are read again takes no
        // new editor yet: its record may have left the portal. One with the
        // keyboard keeps it (above). A click waits for the row of the
        // record it was made on, which then takes the focus (above), or
        // goes when that record no longer shows (#1099).
        if state.painting_stale {
            if response.clicked() {
                state.field_click = Some(FieldClickWait {
                    field,
                    object: object.id,
                    seen: true,
                });
            }
            return Some(response);
        }
        if response.clicked() || response.gained_focus() {
            response.request_focus();
            state.related_click = Some(id);
            state.pending_cursor = None;
            ui.ctx().request_repaint();
        }
        if let Some(list) = list {
            // A click on the arrow enters the field and opens the list;
            // without the arrow, entering it does.
            let on_arrow = response
                .interact_pointer_pos()
                .is_some_and(|pointer| drop_down::arrow(rect).contains(pointer));
            let opens = if list.with_arrow {
                response.clicked() && on_arrow
            } else {
                response.clicked() || response.gained_focus()
            };
            if opens && (related_refusal(state, list.set, target, true).is_none() || list.pending())
            {
                drop_down::open_once_read(ui, id);
            }
            // Why the arrow offers no list, where the pointer rests on it:
            // the field's tooltip and its accessible description.
            if list.with_arrow
                && response
                    .hover_pos()
                    .is_some_and(|pointer| drop_down::arrow(rect).contains(pointer))
            {
                if let Some(refusal) = related_refusal(state, list.set, target, true) {
                    response.clone().on_hover_text(refusal);
                }
            }
        }
        return Some(response);
    }
    let enabled = !state.pending && !state.clicks_held;
    let keep = list.is_some_and(|list| drop_down::keeps_focus(ui, id, rect, list.with_arrow));
    let open = list.is_some() && drop_down::is_open(ui, id);
    // Entered: the list's items are read now.
    let refusal = list.and_then(|list| related_refusal(state, list.set, target, true));
    // A later repetition a script set opens with the script's value.
    let scripted = state.scripted_value(&field);
    let edit = state.related.entry(field).or_insert_with(|| {
        RelatedBuffer::new(target.occurrence, target.version, &target.stored, scripted)
    });
    if state.related_select == Some(id) {
        state.related_select = None;
        let mut editor = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        editor
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::select_all_str(
                &edit.buffer.value,
            )));
        editor.store(ui.ctx(), id);
    }
    // Entered by Tab: the insertion point after the text (#859), or a
    // calendar's text selected (#964).
    if state.related_entry == Some(id) {
        state.related_entry = None;
        let mut editor = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        editor
            .cursor
            .set_char_range(Some(tab_entry_range(object, &edit.buffer.value)));
        editor.store(ui.ctx(), id);
        // A row the portal shows only in part comes into view.
        ui.scroll_to_rect(rect, None);
    }
    if state.related_click == Some(id) {
        state.related_click = None;
        if let Some(cursor) = state.pending_cursor.take() {
            let mut editor = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            editor
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(cursor)));
            editor.store(ui.ctx(), id);
        }
    }
    return_goes_on(ui, &mut state.tab_held, object, id);
    let response = field_editor(
        ui,
        &state.layout_cache,
        (layout, object, rect),
        id,
        &mut edit.buffer.value,
        placeholder,
        related_keys(current, open),
        !state.pending,
        types_line_breaks(object),
    );
    if keep {
        response.request_focus();
    }
    // The user's focus and typing here run the object's triggers, as on
    // the layout's own fields (`DocumentState::saw`), with the row active.
    if current {
        if response.has_focus() {
            // Back in its editor after its trigger's script.
            if state.held_related.as_ref().is_some_and(|held| {
                held.released && held.focus.object == object.id && held.focus.field == field
            }) {
                state.held_related = None;
            }
            state.seen_active = Some(target.name.clone());
            state.seen_object = Some(object.id);
            state.seen_related = Some(RelatedFocus {
                object: object.id,
                field,
                portal: portals::enclosing(layout, object.id),
                occurrence: target.occurrence,
                row: number,
                repetition: object
                    .field_spec()
                    .map_or(1, |spec| spec.repetition.max(1) as u32),
            });
        }
        if response.changed() {
            state.changed.push((target.name.clone(), Some(object.id)));
        }
    }
    if let Some(list) = list {
        let offered = refusal.is_none();
        let choices = list.choices();
        // Typing completes from the items (#870), as on the layout's own
        // fields.
        let completes = list.calendar.is_none()
            && object
                .field_obj
                .as_ref()
                .is_some_and(om_layout::FieldObject::auto_completes);
        if response.changed() && completes {
            if let Some(item) = drop_down::complete(ui, id, &mut edit.buffer.value, &choices) {
                edit.buffer.ui_edited = true;
                state.related_completed = Some(Completion {
                    key: field,
                    editor: id,
                    text: edit.buffer.value.clone(),
                    item,
                });
            }
        }
        if response.lost_focus()
            && state.held_focus.is_none()
            && state.pending_focus.is_none()
            && state
                .related_completed
                .as_ref()
                .is_some_and(|c| c.editor == id)
        {
            if let Some(completion) = state.related_completed.take() {
                edit.buffer.accept_completion(&completion);
            }
        }
        let refusal = refusal.as_deref();
        let input = match &list.calendar {
            Some(cx) => calendar::show(
                ui,
                id,
                rect,
                list.with_arrow,
                &edit.buffer.value,
                cx,
                offered,
                enabled,
                refusal,
            ),
            None => drop_down::show(
                ui,
                id,
                rect,
                list.with_arrow,
                &edit.buffer.value,
                &choices,
                offered,
                enabled,
                refusal,
            ),
        };
        let chosen = match input {
            Some(drop_down::Input::Open) => {
                egui::Popup::open_id(ui.ctx(), popup::menu_id(id));
                None
            }
            Some(drop_down::Input::Close) => {
                egui::Popup::close_id(ui.ctx(), popup::menu_id(id));
                None
            }
            Some(drop_down::Input::Choose(index)) => {
                choices.items.get(index).map(|item| item.value.clone())
            }
            Some(drop_down::Input::Set(text)) => Some(text),
            None => {
                // Entered other than through the sensor above (Tab), or
                // entered while the items were being read.
                if !list.with_arrow && response.gained_focus() && (offered || list.pending()) {
                    drop_down::open_once_read(ui, id);
                }
                if drop_down::opens_once_read(ui, id, offered) && !drop_down::is_open(ui, id) {
                    egui::Popup::open_id(ui.ctx(), popup::menu_id(id));
                }
                None
            }
        };
        if let Some(chosen) = chosen {
            // The editor goes on after the chosen value.
            let mut editor = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            editor
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(chosen.chars().count()),
                )));
            editor.store(ui.ctx(), id);
            edit.buffer.value = chosen;
            edit.buffer.ui_edited = true;
            ui.ctx().request_repaint();
        }
        let open = drop_down::is_open(ui, id);
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(id, related_keys(current, open));
        });
    }
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::text_edit(
            !state.pending,
            shown,
            edit.buffer.value.as_str(),
            placeholder.unwrap_or(""),
        );
        info.label = Some(target.name.clone());
        info
    });
    if response.changed() {
        edit.buffer.ui_edited = true;
    }
    None
}

/// Whether Return types a line break in the field object's Browse editor:
/// an edit box's (display type 0). Drop-down lists, pop-up menus and
/// calendars keep one line, Find mode's criteria too (Return performs the
/// find), and a field whose "Go to next object using" includes Return
/// never sees it (`OpenMakerApp::shortcuts` makes it a Tab).
fn types_line_breaks(object: &LayoutObject) -> bool {
    object
        .field_obj
        .as_ref()
        .is_some_and(|field| field.display_type == 0)
}

/// A field's text as its editor changes it. FileMaker stores a line break
/// as a carriage return, so the `\n` that Return and a paste insert is
/// stored as `\r` (and a pasted `\r\n` as one `\r`). Text already stored,
/// an `\n` too, is left as it is: the editor lays both out as line breaks
/// (`field_galley`), one character each, so cursor positions stay the
/// buffer's.
struct FieldText<'a>(&'a mut String);

/// [`FieldText`]'s `TextBuffer::type_id`, which needs a `'static` type.
struct FieldTextBuffer;

impl egui::TextBuffer for FieldText<'_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.0
    }

    fn insert_text(&mut self, text: &str, char_index: egui::text::CharIndex) -> usize {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        self.0.insert_text(&text, char_index)
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<egui::text::CharIndex>) {
        self.0.delete_char_range(char_range);
    }

    /// Undo and Redo put back the text as it was, line breaks as they were
    /// stored: an `\n` or `\r\n` the user did not type stays.
    fn replace_with(&mut self, text: &str) {
        text.clone_into(self.0);
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<FieldTextBuffer>()
    }
}

/// The galley of a field's text in its editor: as `paint_rich_text` lays
/// the field out at rest (its font, color, alignment, line spacing and
/// wrapping, a carriage return breaking the line), without an uppercase
/// transform, which could change the number of characters. Zoomed, its
/// glyphs are rasterized at the zoom, as the field's text at rest is.
fn field_galley(
    ui: &Ui,
    text: &str,
    style: &ResolvedStyle,
    wrap_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut style = style.clone();
    style.text_transform = None;
    let job = ui.fonts_mut(|fonts| rich_text::layout_job(fonts, text, &style, None, wrap_width));
    let galley = layer_galley(ui, job.clone());
    // Rows snapped to whole pixels at a small zoom and font can land on one
    // another, where the caret could not move between them: those lines
    // keep their unzoomed raster.
    if galley
        .rows
        .windows(2)
        .any(|rows| rows[1].pos.y <= rows[0].pos.y)
    {
        return ui.fonts_mut(|fonts| rich_text::layout_galley(fonts, job));
    }
    galley
}

/// A field object's editor in Browse over `rect`: the field's font,
/// color and insets, and its placeholder as the read-only field shows it,
/// beside the drop-down arrow the field has. Conditional formatting is
/// ignored while a field is in focus. With `lines`, Return types a line
/// break, the text wraps as at rest and scrolls in the field. A focused
/// editor's text is kept for [`focused_field_text`], in Browse, Find mode
/// and portal rows alike.
#[allow(clippy::too_many_arguments)]
fn field_editor(
    ui: &mut Ui,
    cache: &layout_cache::LayoutCache,
    geometry: (&Layout, &LayoutObject, Rect),
    id: egui::Id,
    buffer: &mut String,
    placeholder_text: Option<&str>,
    keys: egui::EventFilter,
    interactive: bool,
    lines: bool,
) -> egui::Response {
    let response = show_field_editor(
        ui,
        cache,
        geometry,
        id,
        buffer,
        placeholder_text,
        keys,
        interactive,
        lines,
    );
    note_focused_text(ui, id, buffer);
    response
}

#[allow(clippy::too_many_arguments)]
fn show_field_editor(
    ui: &mut Ui,
    cache: &layout_cache::LayoutCache,
    (layout, object, rect): (&Layout, &LayoutObject, Rect),
    id: egui::Id,
    buffer: &mut String,
    placeholder_text: Option<&str>,
    keys: egui::EventFilter,
    interactive: bool,
    lines: bool,
) -> egui::Response {
    let style = field_text_box(object, layout, cache.text_style(layout, object, &[], 1));
    let size = style.font_size.unwrap_or(14.0) as f32;
    // As `paint_text` insets read-only text: the field's four insets make
    // the content box, and the line sits at its top, middle or bottom.
    let inset = |side: usize| style.padding[side].unwrap_or(0.0).clamp(0.0, 127.0) as f32;
    let (top_inset, right, bottom, left) = (inset(0), inset(1), inset(2), inset(3));
    let mut inner = rect;
    if has_drop_down_arrow(object) {
        inner.max.x -= 20.0;
        paint_field_icon(ui, object, rect, style.color.map(color));
    }
    // Several lines sit as a block, as `paint_rich_text` places them.
    let galley = lines.then(|| {
        let wrap = (inner.width() - left - right).max(0.0);
        field_galley(ui, buffer, &style, wrap)
    });
    let text_height = galley
        .as_ref()
        .map_or(size + 4.0, |galley| galley.size().y.max(size + 4.0));
    let free = (rect.height() - top_inset - bottom - text_height).max(0.0);
    let top = top_inset
        + match style.vertical_align {
            Some(1) => free / 2.0,
            Some(2) => free,
            _ => 0.0,
        };
    // The placeholder looks as it does in a read-only field: the field's
    // font in its placeholder style (state 18).
    let placeholder = cache.text_style(layout, object, &[], 18);
    let hint = RichText::new(placeholder_text.unwrap_or(""))
        .font(FontId::new(
            placeholder.font_size.map_or(size, |size| size as f32),
            theme::document_font(
                placeholder.font_family.as_deref(),
                placeholder
                    .font_weight
                    .is_some_and(|weight| weight == 2 || weight >= 600),
                matches!(placeholder.font_style, Some(2 | 3)),
            ),
        ))
        .color(placeholder.color.map(color).unwrap_or(theme::CAPTION));
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner));
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    // A custom frame replaces the editor's own margin, so the text's
    // insets are the frame's.
    let frame = egui::Frame::new().inner_margin(egui::Margin {
        left: left.round() as i8,
        right: right.round() as i8,
        top: top.min(127.0).round() as i8,
        bottom: bottom.round() as i8,
    });
    if lines {
        // Text taller than the field scrolls in it; text that fits keeps
        // the field's own place (a scroll area aligns its content to whole
        // points).
        let fits = galley.as_ref().is_none_or(|galley| {
            galley.rows.len() <= 1 || galley.size().y <= rect.height() - top_inset - bottom
        });
        let halign = galley
            .as_ref()
            .map_or(egui::Align::Min, |galley| galley.job.halign);
        return lines_editor(
            &mut child,
            (object, inner, fits, halign),
            id,
            buffer,
            &style,
            (keys, interactive),
            frame,
            hint,
        );
    }
    let font = FontId::new(
        size,
        theme::document_font(
            style.font_family.as_deref(),
            style
                .font_weight
                .is_some_and(|weight| weight == 2 || weight >= 600),
            matches!(style.font_style, Some(2 | 3)),
        ),
    );
    let text_color = style.color.map(color).unwrap_or(theme::TEXT);
    let editor = egui::TextEdit::singleline(buffer)
        .id(id)
        .event_filter(keys)
        .frame(frame)
        .interactive(interactive)
        .font(font.clone())
        .text_color(text_color)
        .hint_text(hint);
    if layer_scale(&child) == 1.0 {
        return child.add_sized(inner.size(), editor);
    }
    // Zoomed, the text is rasterized at the zoom, as the field's text at
    // rest is; otherwise egui lays it out as its own single-line editor does.
    let line_height =
        child.fonts_mut(|fonts| fonts.row_height(&font)) + child.spacing().extra_text_line_spacing;
    let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            text.as_str().to_owned(),
            font.clone(),
            text_color,
        );
        job.keep_trailing_whitespace = true;
        for section in &mut job.sections {
            section.format.line_height = Some(line_height);
        }
        layer_galley(ui, job)
    };
    child.add_sized(inner.size(), editor.layouter(&mut layouter))
}

/// [`field_editor`] with `lines`: a multi-line editor of [`FieldText`],
/// laid out as the field at rest ([`field_galley`]) and anchored as it
/// (`halign`: at its left, center or right). Text taller than the field
/// (`fits` false) scrolls in it to keep the insertion point in view, with a
/// scroll bar only where the field object has one (flag `0x100`,
/// layout-model.md 5.4.1). Up and Down move between lines. Tab stays the
/// document's: the editor inserts no tab character, and egui moves no
/// focus on it (`DocumentState::resolve_tab`).
#[allow(clippy::too_many_arguments)]
fn lines_editor(
    ui: &mut Ui,
    (object, inner, fits, halign): (&LayoutObject, Rect, bool, egui::Align),
    id: egui::Id,
    buffer: &mut String,
    style: &ResolvedStyle,
    (keys, interactive): (egui::EventFilter, bool),
    frame: egui::Frame,
    hint: RichText,
) -> egui::Response {
    let editor_keys = egui::EventFilter {
        tab: false,
        vertical_arrows: true,
        ..keys
    };
    let bar = object
        .field_obj
        .as_ref()
        .is_some_and(|field| field.flags & 0x100 != 0);
    let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        field_galley(ui, text.as_str(), style, wrap_width)
    };
    let mut text = FieldText(buffer);
    let edit = egui::TextEdit::multiline(&mut text)
        .id(id)
        .event_filter(editor_keys)
        .frame(frame)
        .interactive(interactive)
        .desired_rows(1)
        .desired_width(inner.width())
        .align(egui::Align2([halign, egui::Align::Min]))
        .layouter(&mut layouter)
        .hint_text(hint);
    // Whether the last frame drew this editor in its scroll area, with the
    // focus. A scroll area new this frame (the text has just grown taller
    // than the field, or a Tab put the insertion point after a long text)
    // brings the insertion point into view itself: egui asks for that only
    // as the text or the selection changes, which happened, if at all, in
    // a frame without the scroll area.
    // A new zoom counts as a new scroll area: the rows' heights change with
    // their raster, the area's offset does not.
    let scrolled = id.with("lines scrolled");
    let frame_nr = ui.ctx().cumulative_frame_nr();
    let scale = layer_scale(ui);
    let continued = ui
        .data(|data| data.get_temp::<(u64, f32)>(scrolled))
        .is_some_and(|(at, at_scale)| at + 1 == frame_nr && at_scale == scale);
    let response = if fits {
        // As the one-line editor sits: the field's own rect.
        ui.add_sized(inner.size(), edit)
    } else {
        egui::ScrollArea::vertical()
            .id_salt(id.with("lines"))
            // Scrolls at once, the editor's own to its insertion point too:
            // an animation under way at a change of zoom would carry the
            // field past the insertion point at the new one.
            .animated(false)
            // The field's height, not egui's least scrolled height (64
            // points), which would grow the area past the field instead.
            .max_height(inner.height())
            .min_scrolled_height(inner.height())
            .auto_shrink([false, true])
            .scroll_bar_visibility(if bar {
                egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
            } else {
                egui::scroll_area::ScrollBarVisibility::AlwaysHidden
            })
            .show(ui, |ui| {
                let output = edit.min_size(inner.size()).show(ui);
                if let Some(range) = output
                    .cursor_range
                    .filter(|_| !continued && output.response.has_focus())
                {
                    let caret = output.galley.pos_from_cursor(range.primary).translate(
                        output.galley_pos.to_vec2() - egui::vec2(output.galley.rect.left(), 0.0),
                    );
                    ui.scroll_to_rect(caret, None);
                }
                output.response.response
            })
            .inner
    };
    if !fits && response.has_focus() {
        ui.data_mut(|data| data.insert_temp(scrolled, (frame_nr, scale)));
    }
    // The editor locks its own keys while it has the focus; the document's
    // Tab stays locked from egui's traversal as on one line.
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    vertical_arrows: true,
                    ..keys
                },
            );
        });
    }
    response
}

/// How many frames the keys typed after a Tab wait for the portal row's
/// field it goes to (`DocumentState::tab_goal`) before they go where the
/// focus is.
const TAB_GOAL_FRAMES: u32 = 120;

/// What a Tab did (`DocumentState::finish_tab`).
enum TabMove {
    /// A field of the record took the focus.
    Moved,
    /// A portal row's field takes it as it draws.
    Aimed,
    /// Nothing moved.
    Stays,
}

/// Where a field's editor puts its selection when Tab enters it: after its
/// text, or a drop-down calendar's whole text
/// (`FieldObject::tab_selects_content`).
fn tab_entry_range(object: &LayoutObject, text: &str) -> egui::text::CCursorRange {
    if object
        .field_obj
        .as_ref()
        .is_some_and(om_layout::FieldObject::tab_selects_content)
    {
        egui::text::CCursorRange::select_all_str(text)
    } else {
        egui::text::CCursorRange::one(egui::text::CCursor::new(text.chars().count()))
    }
}

/// Take this frame's typing (keys, text, IME, clipboard) out of its input.
fn take_typing(ui: &Ui) -> Vec<egui::Event> {
    ui.ctx().input_mut(|input| {
        let (typing, rest) = std::mem::take(&mut input.events)
            .into_iter()
            .partition(|event| {
                matches!(
                    event,
                    egui::Event::Text(_)
                        | egui::Event::Key { .. }
                        | egui::Event::Paste(_)
                        | egui::Event::Ime(_)
                        | egui::Event::Copy
                        | egui::Event::Cut
                )
            });
        input.events = rest;
        typing
    })
}

/// Put `events` back at the front of this frame's input.
fn replay(ui: &Ui, events: Vec<egui::Event>) {
    if !events.is_empty() {
        ui.ctx().input_mut(|input| {
            let later = std::mem::replace(&mut input.events, events);
            input.events.extend(later);
        });
    }
}

/// The keys a related field's editor keeps from egui: Tab too on the
/// current record, whose fields Tab moves through (#859).
fn related_keys(current: bool, open: bool) -> egui::EventFilter {
    if current {
        drop_down::browse_keys(open)
    } else {
        drop_down::editor_keys(open)
    }
}

/// The first Tab (`false`) or Shift-Tab (`true`) pressed this frame, and
/// where it is among the frame's input events, without Ctrl, Alt or
/// Command, which are left to their own uses.
fn tab_pressed(ui: &Ui) -> Option<(usize, bool)> {
    ui.input(|input| {
        input
            .events
            .iter()
            .enumerate()
            .find_map(|(at, event)| tab_key(event).map(|backward| (at, backward)))
    })
}

/// Whether `event` is a Tab (`Some(false)`) or Shift-Tab (`Some(true)`)
/// press without Ctrl, Alt or Command.
/// Return in the field `id`, whose object's "Go to next object using"
/// includes the Return key (`FieldObject::next_object_on_return`), goes on
/// as Tab does: the keys after it wait for the next frame (`tab_held`),
/// from this field, before its editor would type the line break. It is
/// decided as the field that receives it draws, so a Return typed after a
/// Tab in the same frame, or replayed to the portal row's field a Tab went
/// to, goes by that field's option. An open list keeps Return for its
/// item.
fn return_goes_on(
    ui: &Ui,
    tab_held: &mut Option<(bool, Option<egui::Id>, Vec<egui::Event>)>,
    object: &LayoutObject,
    id: egui::Id,
) {
    let goes_on = object
        .field_obj
        .as_ref()
        .is_some_and(om_layout::FieldObject::next_object_on_return);
    if !goes_on || !ui.memory(|memory| memory.has_focus(id)) || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let Some(mut rest) = ui.ctx().input_mut(|input| {
        let at = input
            .events
            .iter()
            .position(crate::keypad::is_return_press)?;
        Some(input.events.split_off(at))
    }) else {
        return;
    };
    rest.remove(0);
    // A Tab after it this frame took the keys that follow it. That Tab is
    // still among the keys after the Return when `resolve_tab` held them;
    // a portal row's field a Tab reached took it out (`reach_tab_goal`).
    if let Some((backward, _, after)) = tab_held.take() {
        if !rest.iter().any(|event| tab_key(event).is_some()) {
            rest.push(egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: if backward {
                    egui::Modifiers::SHIFT
                } else {
                    egui::Modifiers::NONE
                },
            });
        }
        rest.extend(after);
    }
    *tab_held = Some((false, Some(id), rest));
    ui.ctx().request_repaint();
}

pub(crate) fn tab_key(event: &egui::Event) -> Option<bool> {
    match event {
        egui::Event::Key {
            key: egui::Key::Tab,
            pressed: true,
            modifiers,
            ..
        } if !modifiers.ctrl && !modifiers.alt && !modifiers.command && !modifiers.mac_cmd => {
            Some(modifiers.shift)
        }
        _ => None,
    }
}

/// The "record" of a Find mode tab stop: a criterion of the request shown,
/// which belongs to no record.
const FIND_REQUEST: i64 = i64::MIN;

/// The "object" of a Table view cell's tab stop: this bit and its column.
const TABLE_CELL: u64 = 1 << 63;

/// The focused editor `id` keeps Tab from egui's own traversal: the
/// document moves the focus (`DocumentState::resolve_tab`).
pub(crate) fn keep_tab(ui: &Ui, id: egui::Id) {
    ui.memory_mut(|memory| memory.set_focus_lock_filter(id, drop_down::browse_keys(false)));
}

/// A field object or Table view cell of the current record, or a criterion
/// of the request in Find mode, that Tab can move the focus to
/// (`DocumentState::resolve_tab`).
#[derive(Clone, Copy, Debug)]
struct TabStop {
    /// Its place in the layout's stored tab order; 0 outside it.
    place: u32,
    /// Its paint order in the frame.
    order: usize,
    /// Layout, record and object.
    target: (u64, i64, u64),
    /// The widget that takes the keyboard focus for it.
    widget: egui::Id,
    /// A field object of the layout (`DocumentState::note_tab_stop`).
    object: bool,
    /// A field of a portal row: the portal and the row's number. Tab goes
    /// through a portal row by row, at the portal's place in the order.
    row: Option<(u64, u64)>,
    /// A related field (a portal row's, or one on the layout): what it
    /// shows, which a Tab aims at (`tab_goal`), as its widget may change
    /// when its portal is read again.
    key: Option<(RelatedField, u64)>,
    /// A stop Tab leaves from but never goes to: a field of a part around
    /// Table view's table (`show_table`).
    departure_only: bool,
}

/// A checkbox or radio button set as `paint_object` found it.
struct OptionSet<'a> {
    object: &'a LayoutObject,
    rect: Rect,
    record_index: u64,
    portal: Option<PortalRow>,
    conditions: &'a [usize],
    /// The value shown: the record's draft, else its stored value.
    value: &'a str,
    /// The object's placeholder text, shown by an empty pop-up menu.
    placeholder: Option<&'a str>,
    /// The value is a targeted calculation refresh's, not the page's.
    targeted: bool,
    /// A pop-up menu shows the label of the item storing its value, so it
    /// reads its items also when closed (`Evaluator::shows_labels`).
    labels: bool,
    /// The list's items, or why there are none to offer (pending while
    /// they are read): read when first asked for ([`OptionSet::items`]).
    items: Items<'a>,
}

impl OptionSet<'_> {
    /// The list's items, built the first time they are asked for. A closed
    /// drop-down list or pop-up menu does not ask, so painting it reads
    /// and builds no list; it asks once entered, opened or clicked.
    fn items(&self) -> &Result<std::sync::Arc<Choices>, ChoiceError> {
        self.items.get()
    }
}

/// A control's value-list items, read once when first asked for.
struct Items<'a> {
    read: Box<dyn Fn() -> Result<std::sync::Arc<Choices>, ChoiceError> + 'a>,
    items: std::cell::OnceCell<Result<std::sync::Arc<Choices>, ChoiceError>>,
}

impl<'a> Items<'a> {
    /// Items `read` gives when first asked for.
    fn read(read: impl Fn() -> Result<std::sync::Arc<Choices>, ChoiceError> + 'a) -> Self {
        Items {
            read: Box::new(read),
            items: std::cell::OnceCell::new(),
        }
    }

    /// Items known already (a calendar offers days, no list).
    fn known(items: Result<std::sync::Arc<Choices>, ChoiceError>) -> Self {
        Items {
            read: Box::new(|| Err(ChoiceError::Pending)),
            items: std::cell::OnceCell::from(items),
        }
    }

    fn get(&self) -> &Result<std::sync::Arc<Choices>, ChoiceError> {
        self.items.get_or_init(|| (self.read)())
    }

    /// The items, or none while they are being read, failed or not asked.
    fn shown(&self) -> std::sync::Arc<Choices> {
        self.get().clone().unwrap_or_default()
    }
}

/// Whether Browse lets the user change layout field object `object` (not
/// in a portal row) on a record whose page has `fields`: the provider binds
/// it to a field of the layout's own record, and the record's capabilities
/// let that field be edited. Edit boxes and option sets share this gate.
pub(crate) fn browse_edits(
    document: &RenderLayout,
    fields: &[om_data::Field],
    capabilities: &om_data::RecordCapabilities,
    object: u64,
) -> bool {
    document.field_bindings.get(&object).is_some_and(|name| {
        fields
            .iter()
            .any(|field| field.name == *name && capabilities.can_edit(field))
    })
}

/// Why typing into field `field` of the record at `record_index` is
/// refused with an alert, when the account's privilege set lets the user
/// enter the field but not change it (`RecordCapabilities::typing_refusal`).
/// A field without access, on a record the set may view, is not entered
/// (FileMaker Pro unmeasured).
fn browse_refusal(
    session: &Session,
    capabilities: &RecordCapabilities,
    field: &Field,
    record_index: u64,
) -> Option<EditRefusal> {
    let access = session.record_access_at(record_index);
    if access != Some(0) && session.no_access(record_index, &field.name) {
        return None;
    }
    capabilities.typing_refusal(field, access)
}

/// Show a field's text editor `id` over `buffer` with `show`, and whether
/// the user changed the value this frame. Under `refusal` the user still
/// enters the field, moves the cursor, selects and copies, as FileMaker Pro
/// lets them; a change is undone in the same frame, cursor and all, and the
/// refusal's alert says why (#690).
fn guarded_editor(
    ui: &mut Ui,
    id: egui::Id,
    buffer: &mut String,
    refusal: Option<EditRefusal>,
    actions: &mut Vec<Action>,
    show: impl FnOnce(&mut Ui, &mut String) -> egui::Response,
) -> (egui::Response, bool) {
    #[cfg(target_os = "macos")]
    let show = |ui: &mut Ui, buffer: &mut String| {
        crate::system_menu::with_field_edit(ui, id, |ui| show(ui, buffer))
    };
    let Some(refusal) = refusal else {
        let response = show(ui, buffer);
        let changed = response.changed();
        note_focused_text(ui, id, buffer);
        return (response, changed);
    };
    let before = (buffer.clone(), egui::TextEdit::load_state(ui.ctx(), id));
    let mut response = show(ui, buffer);
    if *buffer != before.0 {
        *buffer = before.0;
        if let Some(edit) = before.1 {
            edit.store(ui.ctx(), id);
        }
        // Nothing changed: no OnObjectModify trigger (`DocumentState::saw`).
        response.flags.remove(egui::response::Flags::CHANGED);
        actions.push(Action::RefuseTyping(refusal));
    }
    note_focused_text(ui, id, buffer);
    (response, false)
}

/// Where [`note_focused_text`] keeps the focused field's text.
fn focused_text_id() -> egui::Id {
    egui::Id::unique("openmaker-focused-field-text")
}

/// Keep the focused field editor's text as this pass left it, for
/// [`focused_field_text`]: an input method counts what it deletes in
/// UTF-16 units, which OpenMaker Go converts to characters against it.
pub(crate) fn note_focused_text(ui: &Ui, id: egui::Id, buffer: &str) {
    if ui.memory(|memory| memory.has_focus(id)) {
        ui.ctx()
            .data_mut(|data| data.insert_temp(focused_text_id(), (id, buffer.to_owned())));
    }
}

/// Where [`note_scroll_area`] keeps the pass's scroll areas.
fn scroll_areas_id() -> egui::Id {
    egui::Id::unique("openmaker-scroll-areas")
}

/// Keep the visible rectangle of a scroll area this pass draws inside the
/// layout (a portal's rows), for [`scroll_areas`]: a screen reader's page
/// there is that area's, not the window's.
pub(crate) fn note_scroll_area(ui: &Ui, rect: Rect) {
    let pass = ui.ctx().cumulative_pass_nr();
    ui.ctx().data_mut(|data| {
        let (at, rects) = data.get_temp_mut_or_default::<(u64, Vec<Rect>)>(scroll_areas_id());
        if *at != pass {
            *at = pass;
            rects.clear();
        }
        rects.push(rect);
    });
}

/// The scroll areas inside the layout the last completed pass drew.
pub(crate) fn scroll_areas(ctx: &egui::Context) -> Vec<Rect> {
    let completed = ctx.cumulative_pass_nr();
    ctx.data(|data| data.get_temp::<(u64, Vec<Rect>)>(scroll_areas_id()))
        .filter(|(at, _)| *at + 1 == completed)
        .map(|(_, rects)| rects)
        .unwrap_or_default()
}

/// The text of the field editor that has the keyboard focus, as the last
/// pass left it; `None` when no field editor has it.
pub(crate) fn focused_field_text(ctx: &egui::Context) -> Option<String> {
    let focused = ctx.memory(|memory| memory.focused())?;
    ctx.data(|data| data.get_temp::<(egui::Id, String)>(focused_text_id()))
        .filter(|(id, _)| *id == focused)
        .map(|(_, text)| text)
}

/// Why the user cannot change value-list control `object` (a set or a
/// pop-up menu) of the record at `record_index` here, if they can
/// (`om_data::choice::availability`): a portal row's or a related field's
/// control, other repetitions, a field entry behavior other than Edit in
/// Browse mode, a field this connection may not change, or, with `items`
/// (which are read then), a list without items OpenMaker offers (still
/// being read, unreadable or empty).
fn option_set_refusal(
    session: &Session,
    document: &RenderLayout,
    state: &DocumentState,
    set: &OptionSet<'_>,
    items: bool,
) -> Option<String> {
    let object = set.object;
    let alias = document
        .layout
        .def
        .as_ref()
        .map_or(0, |d| d.table_alias_key);
    let editable = session
        .record_at(set.record_index)
        .is_some_and(|(fields, _)| browse_edits(document, fields, &state.capabilities, object.id));
    if let Err(error) = choice::availability(object, alias, set.portal.is_some(), editable) {
        return Some(error.message());
    }
    // A calendar offers days, not a value list's items.
    if calendar::is_calendar(object) || !items {
        return None;
    }
    items_refusal(set.items())
}

/// Show the Other… dialog while it is open, and apply its answer: OK puts
/// the text in the record's own field as a waiting change, once the field
/// has its focus again, or in the related record's edit; Cancel changes
/// nothing (#869).
fn show_other_dialog(ui: &Ui, session: &Session, state: &mut DocumentState) {
    let Some(dialog) = state.other_dialog.as_mut() else {
        return;
    };
    // A script or another window's change moved the record or layout from
    // under the dialog: it goes, entering nothing.
    let here = match &dialog.target {
        value_dialogs::OtherTarget::Own { layout, record, .. } => (*layout, *record),
        value_dialogs::OtherTarget::Related { field, .. } => (field.layout, field.parent),
    };
    let current = session
        .layout
        .map(u64::from)
        .zip(session.current_record().map(|(_, record)| record.id));
    if current != Some(here) {
        state.other_dialog = None;
        state.held_focus = None;
        return;
    }
    let Some(answer) = value_dialogs::other(ui.ctx(), dialog) else {
        return;
    };
    let Some(dialog) = state.other_dialog.take() else {
        return;
    };
    let own = matches!(dialog.target, value_dialogs::OtherTarget::Own { .. });
    match (answer, dialog.target) {
        (
            value_dialogs::Answer::Ok(text),
            value_dialogs::OtherTarget::Own {
                layout,
                record,
                object,
            },
        ) => {
            state.option_wait = Some(OptionWait {
                layout,
                record,
                object,
                change: OptionChange::Other(text),
                fresh: true,
                seen: true,
            });
        }
        (
            value_dialogs::Answer::Ok(text),
            value_dialogs::OtherTarget::Related {
                field,
                occurrence,
                version,
                stored,
            },
        ) => {
            let at = ChoiceTarget {
                layout: field.layout,
                object: dialog.object.id,
                record: u64::try_from(field.parent).unwrap_or(u64::MAX),
            };
            if let Ok(edit) = choice::other_value(&dialog.object, at, at, &text) {
                let scripted = state.scripted_value(&field);
                let related = state
                    .related
                    .entry(field)
                    .or_insert_with(|| RelatedBuffer::new(occurrence, version, &stored, scripted));
                related.buffer.value = edit.value;
                related.buffer.ui_edited = true;
            }
        }
        (value_dialogs::Answer::Cancel, _) => {}
    }
    if own {
        state.release_focus();
    }
    ui.ctx().request_repaint();
}

/// Paint a checkbox or radio button set or a pop-up menu, and take the
/// user's change of it. A change, and a pop-up menu's opening, waits until
/// the control has been entered (`OptionWait`): the first frame of a click
/// only enters it as a click on an edit box does, so the field the focus
/// leaves runs its exit triggers first.
fn paint_option_set(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    set: OptionSet<'_>,
    actions: &mut Vec<Action>,
) {
    let layout = &document.layout;
    let object = set.object;
    let display = object
        .field_obj
        .as_ref()
        .map_or(0, |field| field.display_type);
    let radio = display == 4;
    let pop_up = display == choice::POP_UP_MENU;
    let name = document.field_bindings.get(&object.id);
    let row = session.record_at(set.record_index).map(|(_, r)| r.id);
    let target = row.map(|record| (layout.id, record, object.id));
    let current = set.record_index == session.current;
    // A set shows its items. A pop-up menu reads them only when its menu
    // is open, a change of it waits, or it shows the label of the item
    // that stores its value (`OptionSet::labels`): painting it closed
    // builds no list.
    let mut refusal = option_set_refusal(session, document, state, &set, !pop_up);
    // An editable set has the id of the field's editor (scrolling, a
    // cancelled click's surrender); a shown one, its row's.
    let id = match row.filter(|_| refusal.is_none() && name.is_some()) {
        Some(record) => ui.scope_id().with((layout.id, object.id, record)),
        None => ui.scope_id().with((
            layout.id,
            object.id,
            set.record_index,
            set.portal,
            "options",
        )),
    };
    // With the keyboard focus, the set's items and markers take its theme's
    // In Focus state, as its frame does (`paint_object`).
    let focused = set.portal.is_none() && ui.memory(|memory| memory.has_focus(id));
    let styles = options::styles(&state.layout_cache, layout, object, set.conditions, focused);
    let waiting = state
        .option_wait
        .as_ref()
        .is_some_and(|wait| Some((wait.layout, wait.record, wait.object)) == target);
    let read =
        !pop_up || set.labels || waiting || egui::Popup::is_id_open(ui.ctx(), popup::menu_id(id));
    // While its items are being read, a menu stays as it was, focus and
    // waiting click included (`apply_option_wait`), and opens once read.
    let reading = read && matches!(set.items(), Err(ChoiceError::Pending));
    if read && pop_up && !reading {
        refusal = option_set_refusal(session, document, state, &set, true);
    }
    let editable = refusal.is_none() && name.is_some() && row.is_some();
    if editable && state.surrender_focus == target {
        ui.memory_mut(|memory| {
            if memory.has_focus(id) {
                memory.surrender_focus(id);
            }
        });
    }
    let choices = match read {
        true => set.items.shown(),
        false => Default::default(),
    };
    let items = &choices.items;
    let single = object
        .field_obj
        .as_ref()
        .is_some_and(om_layout::FieldObject::single_unlabeled_checkbox);
    let enabled = !state.pending && !state.clicks_held;
    let label = name.map_or("", String::as_str);
    let shown = if pop_up {
        // An empty menu shows the placeholder in its own style (state 18),
        // as an empty edit box does.
        let placeholder = set.value.is_empty().then_some(set.placeholder).flatten();
        let text = match placeholder {
            Some(_) => state.layout_cache.text_style(layout, object, &[], 18),
            None => state
                .layout_cache
                .text_style(layout, object, set.conditions, 1),
        };
        // Inset as FileMaker Pro insets a pop-up's text (#721).
        let text = field_text_box(object, layout, text);
        let arrow = object
            .field_obj
            .as_ref()
            .is_some_and(|field| field.has_icon());
        let commands = match editable {
            true => choice::menu_commands(object),
            false => Vec::new(),
        };
        popup::show(
            ui,
            id,
            set.rect,
            set.value,
            placeholder,
            (&*choices, &commands),
            &text,
            arrow,
            editable,
            enabled,
            label,
        )
    } else {
        let single_items = single.then(|| options::single_item(&choices));
        options::show(
            ui,
            id,
            set.rect,
            set.value,
            single_items.as_deref().unwrap_or(items),
            radio,
            &styles,
            editable,
            enabled,
            label,
            single,
        )
    };
    if let (Some(refusal), Some(_)) = (&refusal, name) {
        let response = ui.interact(set.rect, id.with("refusal"), Sense::hover());
        response.on_hover_text(refusal.as_str());
    }
    let (Some(set_response), Some(name), Some(target)) = (shown.set, name, target) else {
        return;
    };
    // The set takes the focus as an edit box does: Go to Field, a click
    // that waited for a save, a click or Tab (on another row in List
    // view, that row's record becomes current first).
    if !state.pending
        && !state.clicks_held
        && set_response.clicked()
        && set_response.interact_pointer_pos().is_some()
    {
        set_response.request_focus();
    }
    if editable && current && !session.is_loading_records() {
        state.note_first_candidate(object, target, set_response.id);
    }
    if current && !session.is_loading_records() && state.focus.as_deref() == Some(name.as_str()) {
        set_response.request_focus();
        state.scroll.resolve_focus(name, set_response.id);
        state.focus = None;
        state.select_all = false;
    }
    if current && !state.pending && state.pending_focus == Some(target) {
        set_response.request_focus();
        state.scroll.resolve_focus(name, set_response.id);
        state.pending_focus = None;
        state.select_all = false;
        state.tab_entry = false;
    }
    // Tab moves the focus through the layout's tab order, not egui's
    // widget order (#848): the set keeps it from egui, as it keeps its
    // arrow keys (`options::show`); a pop-up menu keeps no arrow keys.
    if current && set_response.has_focus() {
        let keys = egui::EventFilter {
            tab: true,
            horizontal_arrows: !pop_up,
            vertical_arrows: !pop_up,
            ..Default::default()
        };
        ui.memory_mut(|memory| memory.set_focus_lock_filter(set_response.id, keys));
    }
    if let Some(options::Input::Command(command)) =
        shown.input.filter(|_| !state.pending && !state.clicks_held)
    {
        match command {
            // The menu opened on the entered field: its dialog keeps the
            // field entered, as an alert does (`hold_focus`).
            MenuCommand::Other => {
                let text = match choice::other_checked(&choices, set.value) {
                    true => set.value.to_owned(),
                    false => String::new(),
                };
                state.hold_focus(
                    u32::try_from(target.0)
                        .ok()
                        .map(|layout| (layout, target.1)),
                );
                let target = value_dialogs::OtherTarget::Own {
                    layout: target.0,
                    record: target.1,
                    object: target.2,
                };
                state.other_dialog = Some(value_dialogs::OtherDialog::new(
                    target,
                    object.clone(),
                    text,
                ));
            }
            MenuCommand::Edit => match choice::edit_value_list(object) {
                Ok(list) => state.edit_list = Some(list),
                Err(error) => state.choice_refusal = Some(error),
            },
        }
    }
    if let Some(input) = shown.input.filter(|_| !state.pending && !state.clicks_held) {
        let change = match input {
            options::Input::Click { index, shift } => items.get(index).map(|item| (item, shift)),
            options::Input::Space(index) => items.get(index).map(|item| (item, false)),
            options::Input::Open | options::Input::Command(_) => None,
        }
        .map(|(item, shift)| OptionChange::Pick {
            value: item.value.clone(),
            shift,
        })
        .or(matches!(input, options::Input::Open).then_some(OptionChange::Open));
        if let Some(change) = change {
            set_response.request_focus();
            state.option_wait = Some(OptionWait {
                layout: target.0,
                record: target.1,
                object: target.2,
                change,
                fresh: true,
                seen: true,
            });
            // Another row's record becomes current first; a refusal of
            // that gives the focus up again (cancel_pending_focus).
            if !current {
                state.pending_focus = Some(target);
            }
        }
    }
    if (set_response.clicked() || set_response.gained_focus() || shown.input.is_some()) && !current
    {
        actions.push(Action::GoToRecord(set.record_index));
    }
    state.saw(&set_response, name, current, Some(object.id));
    apply_option_wait(ui, session, document, state, &set, radio, id);
}

/// Make the waiting change of `set` (`OptionWait`) when the set shows on
/// the current record, entered and focused, and nothing holds it; drop it
/// when it no longer can be.
#[allow(clippy::too_many_arguments)]
fn apply_option_wait(
    ui: &Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    set: &OptionSet<'_>,
    radio: bool,
    id: egui::Id,
) {
    let layout = &document.layout;
    let object = set.object;
    let Some((_, record)) = session.record_at(set.record_index) else {
        return;
    };
    let Some(wait) = state.option_wait.as_mut().filter(|wait| {
        wait.layout == layout.id && wait.record == record.id && wait.object == object.id
    }) else {
        return;
    };
    wait.seen = true;
    let fresh = std::mem::take(&mut wait.fresh);
    let name = document.field_bindings.get(&object.id);
    let current = set.record_index == session.current;
    let editable =
        option_set_refusal(session, document, state, set, true).is_none() && !state.pending;
    let focused = ui.memory(|memory| memory.has_focus(id));
    // A change waits while the list's items are read: a click on a closed
    // list or menu is often what first asks for them.
    let reading = matches!(set.items(), Err(ChoiceError::Pending));
    let held = state.clicks_held || state.pending || session.is_loading_records() || reading;
    // Entered: the last frame painted the set with the focus (its focus
    // change and triggers were handled), and no script or Go to Field
    // sends the focus elsewhere now.
    let entered = name.is_some_and(|name| state.active.as_deref() == Some(name.as_str()))
        && state.painted_object == Some(object.id)
        && state.focus.is_none()
        && !state.focus_first;
    if !(current && editable && focused && entered && !held) {
        if !fresh && !held && !(current && editable && focused) {
            // Its record did not become current, the set lost the focus
            // or can no longer be changed: the click goes.
            state.option_wait = None;
            if state.pending_focus == Some((layout.id, record.id, object.id)) {
                state.pending_focus = None;
            }
        } else {
            ui.ctx().request_repaint();
        }
        return;
    }
    let Some(wait) = state.option_wait.take() else {
        return;
    };
    let name = name.cloned().unwrap_or_default();
    let server = if set.targeted {
        set.value
    } else {
        bound_value(session, document, object.id, set.record_index)
    };
    let buffer = state
        .buffers
        .entry((layout.id, record.id, name.clone()))
        .or_insert_with(|| FieldBuffer::new(server));
    if set.targeted {
        buffer.refresh_targeted(server, session.record_page_key(set.record_index));
    } else {
        buffer.refresh_page(server, session.record_page_key(set.record_index));
    }
    buffer.remember_version(session, set.record_index);
    let choices = set.items.shown();
    let choices = &*choices;
    let items = &choices.items;
    let single = object
        .field_obj
        .as_ref()
        .is_some_and(om_layout::FieldObject::single_unlabeled_checkbox);
    let display = object.field_obj.as_ref().map(|field| field.display_type);
    let is_drop_down = display == Some(choice::DROP_DOWN_LIST);
    let is_calendar = display == Some(calendar::DROP_DOWN_CALENDAR);
    let one_item = is_drop_down || display == Some(choice::POP_UP_MENU);
    let edit = match wait.change {
        // The field is entered: its menu or list opens.
        OptionChange::Open => {
            egui::Popup::open_id(ui.ctx(), popup::menu_id(id));
            None
        }
        // A day picked in a calendar is its date's text, as if typed.
        OptionChange::Pick { value, .. } if is_calendar => Some(value),
        // A pop-up menu's or a drop-down list's choice is the shared
        // operation's edit, on the record and control it was made on.
        OptionChange::Pick { value, .. } if one_item => {
            let target = |record: i64| ChoiceTarget {
                layout: wait.layout,
                object: wait.object,
                record: u64::try_from(record).unwrap_or(u64::MAX),
            };
            choice::choose(
                object,
                target(wait.record),
                target(record.id),
                choices,
                &value,
            )
            .ok()
            .map(|edit| edit.value)
        }
        // Other…'s text, through the shared operation, as a choice.
        OptionChange::Other(text) => {
            let target = |record: i64| ChoiceTarget {
                layout: wait.layout,
                object: wait.object,
                record: u64::try_from(record).unwrap_or(u64::MAX),
            };
            choice::other_value(object, target(wait.record), target(record.id), &text)
                .ok()
                .map(|edit| edit.value)
        }
        // A single checkbox toggles between the list's first and second
        // values (`choice::single_checkbox`), by a click or Space; a letter
        // a keystroke trigger let through picks nothing in it.
        OptionChange::Pick { .. } if single => choice::single_checkbox(choices, &buffer.value)
            .unwrap_or_else(|error| {
                state.choice_refusal = Some(error);
                None
            }),
        OptionChange::Key(key) if single => match key.as_str() {
            " " => choice::single_checkbox(choices, &buffer.value).unwrap_or_else(|error| {
                state.choice_refusal = Some(error);
                None
            }),
            _ => None,
        },
        OptionChange::Pick { value, shift } => items
            .iter()
            .position(|item| item.value == value)
            .and_then(|index| options::picked(&buffer.value, &items[index].value, radio, shift)),
        OptionChange::Key(key) => options::key(ui, id, &buffer.value, items, &key)
            .and_then(|index| options::picked(&buffer.value, &items[index].value, radio, false)),
    };
    if let Some(value) = edit {
        if is_drop_down || is_calendar {
            // The editor goes on after the chosen value (where FileMaker
            // puts the insertion point is unmeasured).
            let mut editor = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            editor
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(value.chars().count()),
                )));
            editor.store(ui.ctx(), id);
        }
        buffer.value = value;
        buffer.ui_edited = true;
        state.changed.push((name, Some(object.id)));
    }
    ui.ctx().request_repaint();
}

/// `rect` of the layer `ui` paints in, in window points (the document's
/// zoom transforms its layer).
fn to_window(ui: &Ui, rect: Rect) -> Rect {
    ui.ctx()
        .layer_transform_to_global(ui.layer_id())
        .map_or(rect, |transform| transform * rect)
}

/// The scale of the layer `ui` paints in: the window's zoom in the
/// document's zoom layer, 1 outside it.
pub(crate) fn layer_scale(ui: &Ui) -> f32 {
    ui.ctx()
        .layer_transform_to_global(ui.layer_id())
        .map_or(1.0, |transform| transform.scaling)
}

/// `job` laid out in `ui`'s layer, its glyphs rasterized at the layer's
/// scale so that zoomed text stays sharp (`scaled_text`).
pub(crate) fn layer_galley(ui: &Ui, job: egui::text::LayoutJob) -> std::sync::Arc<egui::Galley> {
    let scale = layer_scale(ui);
    ui.fonts_mut(|fonts| scaled_text::layout_galley(fonts, job, scale))
}

/// A short caption centered in a part that shows none of its content.
fn paint_note(ui: &Ui, rect: Rect, note: &str) {
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        note,
        FontId::proportional(12.0),
        theme::CAPTION,
    );
}

/// The most web viewers one window shows pages in at once (each is a native
/// browser view), OpenMaker's bound for a layout or a List of many; the rest
/// show a note.
const MAX_WEB_VIEWERS: usize = 32;

/// Why a web viewer shows no page. FileMaker Pro always shows the page, so
/// this note and its look are OpenMaker's own. The caption sits on a plate
/// centered along the viewer's bottom edge, which keeps it legible over
/// whatever the file draws under the viewer. It wraps to the viewer's
/// width, and an ellipsis ends what does not fit in the viewer's height. In
/// a viewer too short for the plate it is one row, centered. The plate and
/// text never leave the viewer or the document's clip, and take no input.
/// The text is rasterized at the window's zoom (`layer_galley`), so it stays
/// as sharp as the layout's own text; a note cut short with an ellipsis
/// keeps the unzoomed raster, which `scaled_text` does not rebuild.
/// A page Browse paints itself (`web_viewer::composited`): its newest
/// picture over `rect`, where it lies among the layout's objects, so that
/// the objects after it, and whatever egui draws above the document, cover
/// it. With `interactive`, the pointer's input over it this frame, in the
/// page's points (the window's), whether the pointer is on it, and its egui
/// id; scrolling over it scrolls the page, not the document. Native views
/// paint nothing.
fn paint_page(
    ui: &mut Ui,
    pages: &[crate::web_viewer::PageTexture],
    object: u64,
    record: Option<i64>,
    rect: Rect,
    interactive: bool,
) -> (Vec<crate::web_viewer::PageInput>, bool, Option<egui::Id>) {
    use crate::web_viewer::PageInput;
    if !crate::web_viewer::composited() {
        return (Vec::new(), false, None);
    }
    let page = pages
        .iter()
        .find(|p| p.object == object && p.record == record);
    if let Some(page) = page {
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(page.texture, rect, uv, Color32::WHITE);
    }
    if !interactive {
        return (Vec::new(), false, None);
    }
    let response = ui.interact(
        rect,
        ui.scope_id().with(("web viewer page", object, record)),
        Sense::click_and_drag(),
    );
    // Input comes in the window's points, as the page counts (its size is
    // the object's in the window).
    let window = to_window(ui, rect);
    let at = |p: egui::Pos2| (p.x - window.min.x, p.y - window.min.y);
    let on = response.hovered() || response.dragged();
    // A page with the keyboard holds egui's focus, with Tab, the arrows and
    // Escape its own, so that no widget of the window takes the focus while
    // the page takes the keys.
    if !page.is_some_and(|p| p.keyboard) {
        // It lost the keyboard to a press elsewhere: egui's focus goes too.
        if response.has_focus() {
            response.surrender_focus();
        }
    } else {
        // Asked for only when it lacks it: asking again each frame would
        // read as a new focus, and end the input method's composition.
        if !response.has_focus() {
            response.request_focus();
        }
        ui.memory_mut(|m| m.set_focus_lock_filter(response.id, crate::web_viewer::PAGE_KEYS));
    }
    if let (true, Some(page)) = (response.hovered(), page) {
        ui.ctx().set_cursor_icon(match page.cursor {
            crate::web_viewer::PageCursor::Default => egui::CursorIcon::Default,
            crate::web_viewer::PageCursor::Pointer => egui::CursorIcon::PointingHand,
            crate::web_viewer::PageCursor::Text => egui::CursorIcon::Text,
        });
    }
    // A press the page took: its release goes to the page wherever the
    // pointer is, in whichever frame it comes.
    let held_id = response.id.with("pressed");
    // The buttons it holds, one bit each.
    let mut held = ui.data(|d| d.get_temp::<u8>(held_id)).unwrap_or(0);
    let mut input = Vec::new();
    let events = ui.input(|i| i.events.clone());
    for event in events.iter().cloned() {
        match event {
            egui::Event::PointerMoved(p) if on => {
                let (x, y) = at(p);
                input.push(PageInput::Move { x, y });
            }
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                ..
            } => {
                let button = match button {
                    egui::PointerButton::Primary => 0,
                    egui::PointerButton::Secondary => 1,
                    egui::PointerButton::Middle => 2,
                    _ => continue,
                };
                // A press on the page, and the release of a press it took.
                let bit = 1u8 << button;
                if (pressed && response.hovered()) || (!pressed && (on || held & bit != 0)) {
                    if pressed {
                        held |= bit;
                    } else {
                        held &= !bit;
                    }
                    let (x, y) = at(pos);
                    input.push(PageInput::Button {
                        x,
                        y,
                        button,
                        pressed,
                    });
                }
            }
            _ => {}
        }
    }
    let pointer = ui.input(|i| i.pointer.hover_pos());
    if let (true, Some(pointer)) = (response.hovered(), pointer) {
        let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
        let delta = events_delta(&events, line, window.height());
        if delta != egui::Vec2::ZERO {
            let (x, y) = at(pointer);
            input.push(PageInput::Wheel {
                x,
                y,
                dx: delta.x,
                dy: delta.y,
            });
        }
        // The document's scroll area leaves what the page scrolls.
        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    }
    ui.data_mut(|d| {
        if held != 0 {
            d.insert_temp(held_id, held);
        } else {
            d.remove::<u8>(held_id);
        }
    });
    (input, on, Some(response.id))
}

/// The wheel's scroll in this frame's `events`, in points (a line is
/// `line` points, a page `page`).
fn events_delta(events: &[egui::Event], line: f32, page: f32) -> egui::Vec2 {
    events
        .iter()
        .map(|event| match event {
            egui::Event::MouseWheel { unit, delta, .. } => match unit {
                egui::MouseWheelUnit::Point => *delta,
                egui::MouseWheelUnit::Line => *delta * line,
                egui::MouseWheelUnit::Page => *delta * page,
            },
            _ => egui::Vec2::ZERO,
        })
        .fold(egui::Vec2::ZERO, |sum, delta| sum + delta)
}

fn paint_web_viewer_note(ui: &Ui, rect: Rect, note: &str) {
    /// The plate's inset from the viewer's edges.
    const MARGIN: f32 = 4.0;
    /// The text's inset from the plate's edges.
    const PADDING: egui::Vec2 = vec2(6.0, 3.0);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let font = FontId::proportional(12.0);
    let row = painter.fonts_mut(|fonts| fonts.row_height(&font));
    let text = rect.shrink2(PADDING + vec2(MARGIN, MARGIN));
    let mut job =
        egui::text::LayoutJob::simple(note.to_owned(), font, theme::CAPTION, text.width().max(row));
    job.wrap.max_rows = ((text.height() / row).floor() as usize).max(1);
    job.halign = Align::Center;
    let galley = layer_galley(ui, job);
    let size = galley.size() + 2.0 * PADDING;
    let bottom = (rect.bottom() - MARGIN).max(rect.center().y + size.y / 2.0);
    let plate = Rect::from_center_size(pos2(rect.center().x, bottom - size.y / 2.0), size);
    painter.rect_filled(plate, CornerRadius::same(2), theme::MENU_FILL);
    painter.rect_stroke(
        plate,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::HAIRLINE),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        plate.min + PADDING - galley.rect.min.to_vec2(),
        galley,
        theme::CAPTION,
    );
}

/// Pop-up menus, drop-down lists and drop-down calendars show an icon at
/// the right edge (layout-model.md 5.4), each only with its `HasIcon`
/// ("Include arrow", "Include icon to show and hide calendar").
fn has_drop_down_arrow(object: &LayoutObject) -> bool {
    object
        .field_obj
        .as_ref()
        .is_some_and(|field| match field.display_type {
            choice::DROP_DOWN_LIST | choice::POP_UP_MENU | calendar::DROP_DOWN_CALENDAR => {
                field.has_icon()
            }
            _ => false,
        })
}

/// The icon of `object` in `field`: a calendar's calendar, otherwise an
/// arrow. OpenMaker's own glyphs; the theme's `-fm-icon` is not read.
fn paint_field_icon(ui: &Ui, object: &LayoutObject, field: Rect, color: Option<Color32>) {
    if calendar::is_calendar(object) {
        paint_icon(ui, Icon::Calendar, 14.0, field, color);
    } else {
        paint_drop_down_arrow(ui, field, color);
    }
}

/// The arrow of a pop-up menu or drop-down list in `field`.
fn paint_drop_down_arrow(ui: &Ui, field: Rect, color: Option<Color32>) {
    paint_icon(ui, Icon::ChevronDown, 12.0, field, color);
}

/// `icon`, `side` points square, centered 12 points from `field`'s right
/// edge.
fn paint_icon(ui: &Ui, icon: Icon, side: f32, field: Rect, color: Option<Color32>) {
    icons::paint(
        ui.painter(),
        Rect::from_center_size(
            pos2(field.right() - 12.0, field.center().y),
            vec2(side, side),
        ),
        icon,
        color.unwrap_or(theme::TEXT),
        Color32::TRANSPARENT,
    );
}

enum ButtonIconMode<'a> {
    Browse(Option<&'a str>),
    Layout,
}

/// Paint the available icon and return the label's bounds, if it shows,
/// and whether there is icon content. The design canvas uses the latter
/// to decide whether to show its outline and caption for an image-only button.
fn paint_label_icon(
    ui: &Ui,
    cache: &layout_cache::LayoutCache,
    (layout, object): (&Layout, &LayoutObject),
    rect: Rect,
    (text, text_style, rich): (&str, &ResolvedStyle, Option<&om_layout::StyledText>),
    mode: ButtonIconMode<'_>,
    (state, border): (i32, Option<Color>),
) -> (Option<Rect>, bool) {
    let icon = match (&object.button, &object.popover_button) {
        (Some(button), _) => Some((
            button.library_key,
            button.icon_size,
            button.display_type,
            button.calculated_icon(),
        )),
        (None, Some(button)) => Some((
            button.library_key,
            button.icon_size,
            button.display_type,
            button.calculated_icon(),
        )),
        (None, None) => None,
    };
    let Some((key, size, direction, calculated)) = icon else {
        return (Some(rect), false);
    };
    // Browse supplies its visible label. Layout keeps an authored
    // image-only button image-only even when its library key is missing.
    let label_shows = !matches!(mode, ButtonIconMode::Layout) || direction != 1;
    let full_label = label_shows.then_some(rect);
    let svg = match mode {
        ButtonIconMode::Browse(svg) => svg,
        ButtonIconMode::Layout if calculated => return (full_label, false),
        ButtonIconMode::Layout => None,
    };
    let style = cache.formatted_style(layout, object, &[], 4, state);
    let tint = style.icon_color.or(style.color).or(border).map(color);
    let library = if svg.is_some() {
        None
    } else {
        let Some(source) = (key != 0)
            .then(|| cache.library_image(layout, key, tint))
            .flatten()
        else {
            return (full_label, false);
        };
        Some(source)
    };
    let size = (size.max(1) as f32).min(rect.width()).min(rect.height());
    let (at, label) = button_icon_and_label(
        rect,
        size,
        direction,
        label_shows && !text.is_empty(),
        || {
            let [top, right, bottom, left] = text_style.padding.map(|p| p.unwrap_or(0.0) as f32);
            ui.fonts_mut(|fonts| {
                let job = rich_text::layout_job(
                    fonts,
                    text,
                    text_style,
                    rich,
                    (rect.width() - left - right).max(0.0),
                );
                fonts.layout_job(job).size().y + top + bottom
            })
        },
    );
    if let Some(svg) = svg {
        paint_calculated_icon(ui, svg, at, tint);
    } else if let Some(image) = library {
        egui::Image::new(image)
            .show_loading_spinner(false)
            .paint_at(ui, at);
    }
    (label_shows.then_some(label), true)
}

/// The icon and label's bounds. For vertical choices the
/// label uses the whole width and the pair is centered together.
fn button_icon_and_label(
    rect: Rect,
    size: f32,
    display_type: i32,
    has_label: bool,
    label_height: impl FnOnce() -> f32,
) -> (Rect, Rect) {
    let mut label = rect;
    let center = if !has_label {
        rect.center()
    } else {
        match display_type {
            2 | 3 => {
                // Eight points between the icon and the label's line box
                // in the measured standard-theme buttons (layout-model 5.6.1).
                let available = (rect.height() - size).max(0.0);
                let gap = available.min(8.0);
                let height = label_height().clamp(0.0, available - gap);
                let top = rect.center().y - (size + gap + height) / 2.0;
                label.min.y = if display_type == 2 {
                    top + size + gap
                } else {
                    top
                };
                label.max.y = label.min.y + height;
                let icon_top = if display_type == 2 {
                    top
                } else {
                    label.max.y + gap
                };
                pos2(rect.center().x, icon_top + size / 2.0)
            }
            5 => {
                label.max.x -= size + 10.0;
                pos2(rect.right() - size / 2.0 - 6.0, rect.center().y)
            }
            _ => {
                label.min.x += size + 10.0;
                pos2(rect.left() + size / 2.0 + 6.0, rect.center().y)
            }
        }
    };
    (Rect::from_center_size(center, vec2(size, size)), label)
}

fn paint_library_image(
    ui: &Ui,
    cache: &layout_cache::LayoutCache,
    layout: &Layout,
    key: u64,
    rect: Rect,
    tint: Option<Color32>,
) {
    if let Some(image) = cache.library_image(layout, key, tint) {
        egui::Image::new(image)
            .show_loading_spinner(false)
            .paint_at(ui, rect);
    }
}

/// Calculated SVGs by URI, ordered by when they were last drawn. Each
/// forgotten URI releases its bytes and rasters from egui's loaders.
#[derive(Clone, Default)]
struct CalculatedIcons(
    std::collections::VecDeque<(String, std::sync::Arc<[u8]>, std::time::Instant)>,
);

/// Retain at most this many icons after they leave the screen.
const CALCULATED_ICONS: usize = 64;
/// Absolute ceiling across the application's viewports. If all slots are
/// in use, warn on additional icons and retry when a slot is available.
const MAX_CALCULATED_ICONS: usize = 256;
const ICON_SHOWN: std::time::Duration = std::time::Duration::from_secs(1);

/// A button's calculated icon, the SVG text `svg` its formula gave
/// (`ObjectState::icon_svg`), over `rect`, tinted as the button's library
/// icon is (OpenMaker's choice). Text that is empty (none, or longer than
/// `om_data::host::layout::MAX_ICON_SVG`) or that the SVG worker cannot
/// draw shows a warning sign instead, as FileMaker Pro shows a warning
/// icon for SVG it does not support (Claris); the sign's look, and which
/// SVG FileMaker Pro refuses, are unmeasured.
fn paint_calculated_icon(ui: &Ui, svg: &str, rect: Rect, tint: Option<Color32>) {
    use std::hash::{Hash, Hasher};
    // Form view can paint objects outside the current scroll clip.
    // Only visible icons can admit or renew a cache entry.
    if !ui.is_rect_visible(rect) || !rect.intersect(ui.clip_rect()).is_positive() {
        return;
    }
    let warning = || {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{26a0}",
            FontId::proportional(rect.height().max(1.0)),
            tint.unwrap_or(theme::TEXT),
        );
    };
    if svg.trim().is_empty() {
        warning();
        return;
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    svg.hash(&mut hash);
    tint.hash(&mut hash);
    let uri = format!(
        "{}{:016x}.svg",
        crate::svg_loader::CALCULATED_ICON_URI,
        hash.finish()
    );
    let id = egui::Id::unique("openmaker calculated icons");
    let now = std::time::Instant::now();
    let (bytes, forgotten) = ui.data_mut(|data| {
        let icons = data.get_temp_mut_or_default::<CalculatedIcons>(id);
        let known = icons
            .0
            .iter()
            .position(|(known, ..)| *known == uri)
            .and_then(|at| icons.0.remove(at));
        let mut forgotten = Vec::new();
        while icons.0.len() >= CALCULATED_ICONS
            && icons
                .0
                .front()
                .is_some_and(|(.., drawn)| now.duration_since(*drawn) >= ICON_SHOWN)
        {
            forgotten.extend(icons.0.pop_front().map(|(old, ..)| old));
        }
        if known.is_none() && icons.0.len() >= MAX_CALCULATED_ICONS {
            return (None, forgotten);
        }
        // Copied and tinted once, as it first shows.
        let bytes = known.map_or_else(
            || {
                let image = om_layout::LibraryImage {
                    format: om_layout::LibraryImageFormat::Svg,
                    data: svg.as_bytes().to_vec(),
                };
                std::sync::Arc::<[u8]>::from(tinted_image(&image, tint))
            },
            |(_, bytes, _)| bytes,
        );
        icons.0.push_back((uri.clone(), bytes.clone(), now));
        (Some(bytes), forgotten)
    });
    for old in forgotten {
        ui.ctx().forget_image(&old);
    }
    let Some(bytes) = bytes else {
        warning();
        ui.ctx().request_repaint_after(ICON_SHOWN);
        return;
    };
    let source = egui::ImageSource::Bytes {
        uri: uri.into(),
        bytes: bytes.into(),
    };
    // As `Image::paint_at` asks for it: the SVG worker draws exact sizes.
    let ppp = ui.pixels_per_point();
    let pixels = (ppp * egui::emath::GuiRounding::round_to_pixels(rect, ppp).size()).round();
    let hint = egui::SizeHint::Size {
        width: pixels.x as u32,
        height: pixels.y as u32,
        maintain_aspect_ratio: false,
    };
    if source
        .clone()
        .load(ui.ctx(), egui::TextureOptions::default(), hint)
        .is_err()
    {
        warning();
        return;
    }
    egui::Image::new(source)
        .show_loading_spinner(false)
        .paint_at(ui, rect);
}

/// A library image's encoded bytes, an SVG's tinted, under a URI naming
/// those bytes: egui's loaders keep the first bytes they get for a URI.
fn library_image(
    layout: &Layout,
    key: u64,
    tint: Option<Color32>,
) -> Option<egui::ImageSource<'static>> {
    use std::hash::{Hash, Hasher};
    let image = layout.library_images.get(&key)?;
    let data = tinted_image(image, tint);
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hash);
    let uri = format!(
        "bytes://openmaker/{:016x}.{}",
        hash.finish(),
        image.format.extension()
    );
    Some(egui::ImageSource::Bytes {
        uri: uri.into(),
        bytes: data.into(),
    })
}

/// A library image's bytes; an SVG's with `tint` filling its `fm_fill`
/// shapes, as Browse paints icons. Reports print icons from the same bytes.
pub(crate) fn tinted_image(image: &om_layout::LibraryImage, tint: Option<Color32>) -> Vec<u8> {
    let mut data = image.data.clone();
    if image.format == om_layout::LibraryImageFormat::Svg {
        if let (Some(tint), Ok(svg)) = (tint, std::str::from_utf8(&data)) {
            if let Some(start) = svg_root_content_offset(svg) {
                let opacity = f32::from(tint.a()) / 255.0;
                let css = format!(
                    "<style xmlns=\"http://www.w3.org/2000/svg\">.fm_fill{{fill:#{:02x}{:02x}{:02x}!important;\
                     fill-opacity:{opacity}!important}}</style>",
                    tint.r(),
                    tint.g(),
                    tint.b(),
                );
                data.splice(start..start, css.bytes());
            }
        }
    }
    data
}

/// Find the root's opening tag without expanding entities or parsing the
/// SVG body. The rendering worker still decides whether the SVG is valid.
fn svg_root_content_offset(svg: &str) -> Option<usize> {
    use quick_xml::events::Event;

    let xml = svg.strip_prefix('\u{feff}').unwrap_or(svg);
    let prefix_len = svg.len() - xml.len();
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event().ok()? {
            Event::Start(root) if root.local_name().as_ref() == b"svg" => {
                return Some(prefix_len + usize::try_from(reader.buffer_position()).ok()?);
            }
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
            Event::Text(text) if text.iter().all(u8::is_ascii_whitespace) => {}
            _ => return None,
        }
    }
}

fn default_panel(object: &LayoutObject, panels: &[&LayoutObject]) -> u64 {
    let tab = object.tab.as_ref();
    [
        tab.map_or(0, |t| t.default_panel_key),
        tab.map_or(0, |t| t.current_panel_key),
    ]
    .into_iter()
    .find(|key| panels.iter().any(|p| p.id == *key))
    .unwrap_or_else(|| panels.first().map_or(0, |p| p.id))
}

fn tab_headers(
    ui: &mut Ui,
    cache: &layout_cache::LayoutCache,
    layout: &Layout,
    control: &LayoutObject,
    panels: &[&LayoutObject],
    (rect, keyboard): (Rect, bool),
    selected: &mut u64,
) -> Option<egui::Id> {
    let height = control
        .tab
        .as_ref()
        .map_or(30.0, |tab| tab.tab_height.max(1) as f32);
    let mut left = rect.left();
    let mut focus_stop = None;
    for panel in panels {
        let info = panel.tab_panel.as_ref();
        let width = info.map_or(100.0, |tab| tab.tab_width.max(1) as f32);
        let x = info.map_or(left, |tab| rect.left() + tab.tab_left_edge as f32);
        let header = Rect::from_min_size(pos2(x, rect.top()), vec2(width, height));
        let label = panel.text.as_deref().unwrap_or("");
        let response = ui.interact(
            header,
            ui.scope_id().with((layout.id, panel.id, "tab")),
            Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Tab,
                ui.is_enabled(),
                *selected == panel.id,
                label,
            )
        });
        if response.clicked() {
            *selected = panel.id;
        }
        let active = *selected == panel.id;
        if active && keyboard && ui.is_rect_visible(header) {
            focus_stop = Some(response.id);
            if response.has_focus() {
                keep_tab(ui, response.id);
            }
        }
        let state = if active {
            7
        } else if response.hovered() {
            2
        } else {
            1
        };
        paint_box(
            ui,
            header,
            &cache.formatted_style(layout, panel, &[], 1, state),
            ObjectKind::Rectangle,
        );
        let mut text = cache.text_style(layout, panel, &[], state);
        let control_text = cache.text_style(layout, control, &[], state);
        if text.font_size.is_none() {
            text.font_size = control_text.font_size;
        }
        if text.color.is_none() {
            text.color = control_text.color;
        }
        text.text_align = Some(2);
        text.vertical_align = Some(1);
        paint_text(ui, header, label, &text);
        if focus_stop == Some(response.id) && response.has_focus() {
            ui.painter().rect_stroke(
                header,
                CornerRadius::ZERO,
                Stroke::new(1.0, theme::FOCUS),
                egui::StrokeKind::Inside,
            );
        }
        left = header.right();
    }
    focus_stop
}

fn bound_value<'a>(
    session: &'a Session,
    document: &RenderLayout,
    id: u64,
    record_index: u64,
) -> &'a str {
    let Some(name) = document.field_bindings.get(&id) else {
        return "";
    };
    value_named(session, name, record_index)
}

/// What a field object shows: a calculation's value when the record page
/// cannot hold it (related fields, other repetitions, unstored
/// calculations, summaries, globals), else the page's value; and container
/// data (stored data or a library reference), which shows as a picture or
/// a file rather than as text.
fn field_content(
    session: &Session,
    document: &RenderLayout,
    calc: Option<&Evaluator<'_>>,
    object: &LayoutObject,
    record_index: u64,
    portal: Option<PortalRow>,
) -> (
    String,
    Option<om_layout::StyledText>,
    Option<om_data::calc::Container>,
) {
    // A record the account may not view, or a field without access, shows
    // FileMaker Pro's `<No Access>` (#137's reading); its value is empty.
    // A related field without access likewise, in a portal row or on the
    // layout (#695's reading).
    // On a layout whose records the set gives no access, every field
    // object shows it, in a portal row too (#707's reading; portals there
    // unmeasured [I]).
    let denied = calc.is_some_and(Evaluator::layout_masked)
        || portal.is_none()
            && document
                .field_bindings
                .get(&object.id)
                .is_some_and(|name| session.no_access(record_index, name))
        || calc
            .zip(object.field_spec())
            .is_some_and(|(calc, spec)| calc.related_no_access(spec, portal));
    if denied {
        return (NO_ACCESS.to_owned(), None, None);
    }
    // A field of another file that is not open: FileMaker Pro's
    // `<File Missing>` (reported by a FileMaker Pro 26 run on a local pair,
    // #214).
    let missing = calc.zip(object.field_spec()).is_some_and(|(calc, spec)| {
        om_layout::occurrence_id(spec.table_key).is_some_and(|o| calc.file_missing(o))
    });
    if missing {
        return (FILE_MISSING.to_owned(), None, None);
    }
    let page = if portal.is_some() {
        ""
    } else {
        bound_value(session, document, object.id, record_index)
    };
    // A container's library reference (containers.md 1.4) is not shown
    // while its file name is read.
    let page = match om_data::containers::reference(page) {
        Some(_) if is_container(session, document, object.id, record_index) => "",
        _ => page,
    };
    let (Some(calc), Some(spec)) = (calc, object.field_spec()) else {
        return (page.to_owned(), None, None);
    };
    let Some(record) = session
        .record_at(record_index)
        .and_then(|(_, r)| u64::try_from(r.id).ok())
    else {
        return (page.to_owned(), None, None);
    };
    match calc.field_value_in_portal(object.id, spec, record, portal) {
        // Its file name stays the field's value for assistive technology.
        Some(Value::Container(data)) if !data.streams.is_empty() || data.library.is_some() => {
            (data.text.as_str().to_owned(), None, Some(data))
        }
        Some(value) => {
            let rich = match &value {
                Value::Text(text) | Value::NumberText(_, text) | Value::DateText(_, text) => {
                    Some(rich_text::value_text(text))
                }
                _ => None,
            };
            if portal.is_some()
                || calc.computes(spec)
                || calc.refreshed_value(object.id)
                || matches!(value, Value::Container(_))
            {
                // A container without data shows its text.
                (calc.display(&value), rich, None)
            } else {
                // Stored cells retain the server's text, including time and
                // timestamp formatting. Only matching text can share offsets.
                (page.to_owned(), rich.filter(|rich| rich.text == page), None)
            }
        }
        None => (page.to_owned(), None, None),
    }
}

/// What FileMaker Pro shows in a field the account may not see.
const NO_ACCESS: &str = "<No Access>";
/// Why an interactive container of another List record plays no audio or
/// video: the window's player shows one view per object (#200).
const MEDIA_CURRENT_ONLY: &str = "Plays for the current record only.";
const MEDIA_NOT_IN_PORTALS: &str = "Plays outside portal rows only.";

/// What FileMaker Pro shows in a field of a file that is not open.
const FILE_MISSING: &str = "<File Missing>";

/// A container field's content box: inside each side's border width
/// (`style`, the object's box) and its padding (`text`, its text style).
fn content_box(rect: Rect, style: &ResolvedStyle, text: &ResolvedStyle) -> Rect {
    let side =
        |index: usize| (container_border(style, index) + text.padding[index].unwrap_or(0.0)) as f32;
    Rect::from_min_max(
        rect.min + vec2(side(3), side(0)),
        rect.max - vec2(side(1), side(2)),
    )
}

/// How far a container field's picture or file sits inside side `side`
/// (0 top, 1 right, 2 bottom, 3 left) of its frame for its border: the
/// side's stored width, whether or not a style draws it. Every starter
/// container stores 1 point on each side and draws none, and FileMaker Pro
/// 26 places its picture 1 point inside the frame all round, as it insets an
/// edit box's text (data-format.md "Graphic format", layout-model.md 6.5).
pub(crate) fn container_border(style: &ResolvedStyle, side: usize) -> f64 {
    style.border_width[side]
        .filter(|width| width.is_finite() && *width > 0.0)
        .map_or(0.0, |width| width.min(1000.0))
}

/// `Self` for an object's calculations: a field object's value, a text or
/// button object's text.
fn object_value(
    session: &Session,
    document: &RenderLayout,
    calc: &Evaluator<'_>,
    object: &LayoutObject,
    record_index: u64,
    portal: Option<PortalRow>,
) -> Value {
    if object.kind() != ObjectKind::Field {
        return Value::text(object_text(
            session,
            object,
            (record_index, true),
            Some(calc),
            &ObjectState::default(),
            portal,
        ));
    }
    let record = session
        .record_at(record_index)
        .and_then(|(_, r)| u64::try_from(r.id).ok());
    if let (Some(spec), Some(record)) = (object.field_spec(), record) {
        if let Some(value) = calc.field_value_in_portal(object.id, spec, record, portal) {
            return value;
        }
    }
    Value::text(if portal.is_some() {
        ""
    } else {
        bound_value(session, document, object.id, record_index)
    })
}

fn value_named<'a>(session: &'a Session, name: &str, record_index: u64) -> &'a str {
    let Some((fields, record)) = session.record_at(record_index) else {
        return "";
    };
    fields
        .iter()
        .position(|f| f.name == name)
        .and_then(|index| record.values.get(index))
        .and_then(Option::as_deref)
        .unwrap_or("")
}

/// A container field object outside portals in Browse: a click selects it
/// (`DocumentState::container`), as does a script's Go to Field; the
/// selection shows as a focus ring. `shown` is what the field shows.
#[allow(clippy::too_many_arguments)]
fn select_container(
    ui: &mut Ui,
    session: &Session,
    document: &RenderLayout,
    state: &mut DocumentState,
    (layout, object, record_index): (u64, u64, u64),
    rect: Rect,
    shown: &str,
    actions: &mut Vec<Action>,
) -> bool {
    let (Some(name), Some((_, record))) = (
        document.field_bindings.get(&object),
        session.record_at(record_index),
    ) else {
        return false;
    };
    let selection = ContainerSelection {
        layout,
        record: record.id,
        object,
        field: name.clone(),
        row: None,
    };
    let response = ui.interact(
        rect,
        ui.scope_id().with((layout, object, record.id, "container")),
        Sense::click(),
    );
    let enabled = !state.pending;
    let selected = state.container.as_ref() == Some(&selection);
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, name);
        info.current_text_value = Some(shown.to_owned());
        info
    });
    let current = record_index == session.current;
    if response.clicked() && enabled {
        state.container = Some(selection.clone());
        if !current {
            actions.push(Action::GoToRecord(record_index));
        }
    } else if current
        && !session.is_loading_records()
        && state.focus.as_deref() == Some(name.as_str())
    {
        // A script went to the field.
        state.container = Some(selection.clone());
        state.focus = None;
        state.select_all = false;
    } else if selected && clicked_elsewhere_on_layout(ui, &response) {
        state.container = None;
    }
    let selected = state.container.as_ref() == Some(&selection);
    if selected {
        paint_container_selection(ui, rect);
    }
    selected
}

/// A selected container field's focus ring.
fn paint_container_selection(ui: &Ui, rect: Rect) {
    ui.painter().rect_stroke(
        rect.shrink(1.0),
        CornerRadius::same(2),
        Stroke::new(2.0, theme::FOCUS),
        egui::StrokeKind::Inside,
    );
}

/// A related container in a portal row ([`ContainerSelection::row`]): a
/// click selects it (the Insert menu then fills that row's record), a
/// click elsewhere on the layout ends it.
fn select_related_container(
    ui: &mut Ui,
    state: &mut DocumentState,
    selection: ContainerSelection,
    rect: Rect,
    shown: &str,
) {
    let response = ui.interact(
        rect,
        ui.scope_id()
            .with(("related container", selection.object, selection.row)),
        Sense::click(),
    );
    let enabled = !state.pending;
    let selected = state.container.as_ref() == Some(&selection);
    response.widget_info(|| {
        let mut info =
            egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, &selection.field);
        info.current_text_value = Some(shown.to_owned());
        info
    });
    if response.clicked() && enabled {
        state.selected_row = None;
        state.container = Some(selection.clone());
    } else if selected && clicked_elsewhere_on_layout(ui, &response) {
        state.container = None;
    }
    if state.container.as_ref() == Some(&selection) {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(2),
            Stroke::new(2.0, theme::FOCUS),
            egui::StrokeKind::Inside,
        );
    }
}

/// A click elsewhere on the layout itself: not on a menu, popup or dialog
/// over it, nor outside the document's visible part (the menu bar, the
/// toolbar), where commands for the selected field are chosen.
fn clicked_elsewhere_on_layout(ui: &Ui, response: &egui::Response) -> bool {
    if !response.clicked_elsewhere() {
        return false;
    }
    let Some(pos) = ui.input(|input| input.pointer.interact_pos()) else {
        return false;
    };
    ui.ctx().layer_id_at(pos) == Some(ui.layer_id()) && to_window(ui, ui.clip_rect()).contains(pos)
}

fn is_container(session: &Session, document: &RenderLayout, id: u64, record_index: u64) -> bool {
    let Some(name) = document.field_bindings.get(&id) else {
        return false;
    };
    session.record_at(record_index).is_some_and(|(fields, _)| {
        fields
            .iter()
            .any(|field| field.name == *name && field.kind == FieldKind::Container)
    })
}

/// An object's text: its calculated label, else its text with merge fields,
/// merge variables and symbols filled in.
/// An object's text on the record at `record_index`, its merge fields
/// showing the record's values through the object's data format, or
/// nothing when `values` is false (Find mode).
fn object_text(
    session: &Session,
    object: &LayoutObject,
    (record_index, values): (u64, bool),
    calc: Option<&Evaluator<'_>>,
    state: &ObjectState,
    portal: Option<PortalRow>,
) -> String {
    object_text_with_substitutions(
        session,
        object,
        (record_index, values),
        calc,
        state,
        portal,
        false,
    )
    .0
}

fn object_text_with_substitutions(
    session: &Session,
    object: &LayoutObject,
    (record_index, values): (u64, bool),
    calc: Option<&Evaluator<'_>>,
    state: &ObjectState,
    portal: Option<PortalRow>,
    track_substitutions: bool,
) -> (String, Vec<TextSubstitution>) {
    if let Some(label) = &state.label {
        return (label.clone(), Vec::new());
    }
    let record = session
        .record_at(record_index)
        .and_then(|(_, r)| u64::try_from(r.id).ok());
    let merged = |text: &str, track_substitutions| {
        calc.and_then(|calc| {
            calc.merge_text_in_portal(
                text,
                record,
                portal,
                object.data_format.as_ref(),
                track_substitutions,
            )
        })
        .unwrap_or_else(|| (text.to_owned(), Vec::new()))
    };
    if object.text_runs.is_empty() {
        return merged(
            object.text.as_deref().unwrap_or_default(),
            track_substitutions,
        );
    }
    let field_value = |name: &str| {
        if !values {
            String::new()
        } else if calc.is_some_and(Evaluator::layout_masked) {
            // A layout whose records the set gives no access shows
            // `<No Access>` for a merge field (#707's reading).
            NO_ACCESS.to_owned()
        } else {
            if let (Some(calc), Some(record)) = (calc, record) {
                if let Some(value) = calc.named_value_in_portal(name, record, portal) {
                    // A merge field shows its value through the object's
                    // own data format, as a field object does.
                    return data_format::display(
                        &calc.display(&value),
                        value.value_type(),
                        object.data_format.as_ref(),
                        &calc.context(),
                    )
                    .into_owned();
                }
            }
            if portal.is_some() {
                String::new()
            } else {
                value_named(session, name, record_index).to_owned()
            }
        }
    };
    let mut output = String::new();
    let mut substitutions = Vec::new();
    let mut fields = object.stored_merge_field_ranges();
    let mut source = track_substitutions.then_some(0);
    for run in &object.text_runs {
        match run {
            TextRun::Literal(text) => {
                let (shown, ranges) = merged(text, source.is_some());
                let start = output.len();
                output.push_str(&shown);
                if let Some(source) = &mut source {
                    substitutions.extend(ranges.into_iter().map(|range| TextSubstitution {
                        source: (*source + range.source.start)..(*source + range.source.end),
                        display: (start + range.display.start)..(start + range.display.end),
                    }));
                    *source += text.len();
                }
            }
            TextRun::MergeField(name) => {
                let start = output.len();
                output.push_str(&field_value(name));
                if let Some(expected_start) = source {
                    if let Some(range) = fields.next().filter(|r| r.start == expected_start) {
                        source = Some(range.end);
                        substitutions.push(TextSubstitution {
                            source: range,
                            display: start..output.len(),
                        });
                    } else {
                        source = None;
                        substitutions.clear();
                    }
                }
            }
        }
    }
    (output, substitutions)
}

/// An object's style for `component` in `state`, with the conditional
/// formats whose conditions hold laid over it in stored order.
pub(crate) fn formatted_style(
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
    component: i32,
    state: i32,
) -> ResolvedStyle {
    let mut style = layout.object_style(object, component, state);
    for &i in conditions {
        if let Some(format) = object.calcs.conditions.get(i) {
            style.overlay(&layout.conditional_style(object, &format.condition, component, state));
        }
    }
    style
}

/// An object's text component's margins (properties 105-108), top,
/// right, bottom, left, in points: an em scales with `size`, the font size
/// (layout-model.md, "Where a field's text sits"). The starters' edit boxes
/// store 0.875 / 0.25 / 0.625 / 0.25 em, the values of their theme's
/// `edit_box` text rule. `None` when a margin has no concrete inset (a
/// percent, a line count, an automatic value) or is not a valid length.
pub(crate) fn text_margins(layout: &Layout, object: &LayoutObject, size: f64) -> Option<[f64; 4]> {
    let mut margins = [0.0; 4];
    for (i, id) in (105..=108).enumerate() {
        let Some(length) = layout
            .object_property(object, 3, 1, id)
            .and_then(|p| p.length.as_ref())
        else {
            continue;
        };
        // Em scales with the text; absolute units convert as everywhere
        // ([`om_layout::Length::points`]).
        let margin = match (length.unit, length.auto_value) {
            (7, 0 | 1) => length.value * size,
            _ => length.points()?,
        };
        if !margin.is_finite() || margin < 0.0 {
            return None;
        }
        margins[i] = margin;
    }
    Some(margins)
}

/// A field's text style as Browse draws and edits its text: its text
/// margins ([`text_margins`]) and its border widths added to its padding,
/// as FileMaker Pro 26 insets a field's text (layout-model.md 6.5): every
/// side's width for an edit box, drop-down list or calendar, and for a
/// pop-up menu only the sides whose border style draws a line. Its vertical
/// alignment is defaulted ([`default_vertical_align`]). A margin with no
/// concrete inset leaves the plain padding.
fn field_text_box(
    object: &LayoutObject,
    layout: &Layout,
    mut style: ResolvedStyle,
) -> ResolvedStyle {
    let size = style.font_size.unwrap_or(12.0);
    let pop_up = object
        .field_obj
        .as_ref()
        .is_some_and(|field| field.display_type == 2);
    if let Some(mut insets) = text_margins(layout, object, size) {
        let sides = insets
            .iter_mut()
            .zip(style.border_width.iter().zip(style.border_style));
        for (inset, (width, border)) in sides {
            let width = width.unwrap_or(0.0);
            let drawn = !matches!(border, Some(0 | 1));
            if width.is_finite() && width > 0.0 && (drawn || !pop_up) {
                *inset += width.min(1000.0);
            }
        }
        for (padding, inset) in style.padding.iter_mut().zip(insets) {
            *padding = Some(padding.unwrap_or(0.0) + inset);
        }
    }
    style
        .vertical_align
        .get_or_insert(default_vertical_align(object));
    style
}

/// Where a field's text sits in its box when its style does not say
/// (property 611): at the top, as FileMaker Pro draws an edit box, a
/// drop-down list and a calendar, and centered for a pop-up menu
/// (layout-model.md, "Numeric style values checked against file-contained
/// themes").
fn default_vertical_align(object: &LayoutObject) -> i32 {
    match object.field_obj.as_ref().map(|field| field.display_type) {
        Some(2) => 1,
        _ => 0,
    }
}

/// Components override only the properties they specify; text commonly lives
/// on `self`, while icon-bearing controls also use a `text` component.
pub(crate) fn text_style(
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
    state: i32,
) -> ResolvedStyle {
    let mut base = formatted_style(layout, object, conditions, 1, state);
    let text = formatted_style(layout, object, conditions, 3, state);
    macro_rules! overlay {
        ($($field:ident),*) => {
            $(if text.$field.is_some() {
                base.$field = text.$field;
            })*
        };
    }
    overlay!(
        color,
        font_family,
        font_size,
        font_weight,
        font_style,
        text_align,
        vertical_align,
        text_transform,
        line_height
    );
    for i in 0..4 {
        if text.padding[i].is_some() {
            base.padding[i] = text.padding[i];
        }
    }
    base
}

/// Only the conditional overrides: stored character runs inherit every
/// other layout attribute, but a condition can replace an explicit color.
pub(crate) fn conditional_text_style(
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
    state: i32,
) -> ResolvedStyle {
    let mut result = ResolvedStyle::default();
    for &index in conditions {
        let Some(format) = object.calcs.conditions.get(index) else {
            continue;
        };
        for component in [1, 3] {
            result.overlay(&layout.conditional_style(object, &format.condition, component, state));
        }
    }
    result
}

/// Apply each object's anchors in its own container. A child receives only
/// the size change of its parent, so fixed groups constrain child autosizing.
fn resized_bounds(layout: &Layout, id: u64, mut growth: egui::Vec2) -> Option<Rect> {
    let mut chain = Vec::new();
    let mut current = Some(id);
    let mut seen = BTreeSet::new();
    while let Some(id) = current {
        if !seen.insert(id) {
            return None;
        }
        let object = layout.objects.get(id)?;
        chain.push(object);
        current = object.parent;
    }
    let mut origin = egui::Pos2::ZERO;
    let mut rect = Rect::NOTHING;
    for object in chain.into_iter().rev() {
        let bounds = object.bounds.as_ref()?;
        let mut local = Rect::from_min_max(
            pos2(bounds.left as f32, bounds.top as f32),
            pos2(bounds.right as f32, bounds.bottom as f32),
        );
        let original = local.size();
        let anchors = object.autosizing_anchors();
        for (axis, leading, trailing) in [
            (0, anchors.left, anchors.right),
            (1, anchors.top, anchors.bottom),
        ] {
            if trailing {
                local.max[axis] += growth[axis];
                if !leading {
                    local.min[axis] += growth[axis];
                }
            }
        }
        growth = local.size() - original;
        rect = local.translate(origin.to_vec2());
        origin = rect.min;
    }
    Some(rect)
}

/// A solid rectangle hides the part of its outer shadow behind its own fill.
/// Other shapes need separate clipping and are outside this path.
fn paint_outer_box_shadow(ui: &Ui, rect: Rect, style: &ResolvedStyle) {
    let Some([shadow]) = style.box_shadows.as_deref() else {
        return;
    };
    if shadow.kind != 1
        || shadow.inset
        || style.border_radius.unwrap_or(0.0) != 0.0
        || !style.background.is_some_and(|fill| fill.alpha == 1.0)
    {
        return;
    }
    let Some(tint) = shadow.color.filter(|tint| tint.alpha == 1.0) else {
        return;
    };
    // Keep non-finite and excessive file coordinates out of tessellation.
    let points = |length: Option<&om_layout::Length>| {
        length
            .and_then(om_layout::Length::points)
            .filter(|value| value.abs() <= 4096.0)
            .map(|value| value as f32)
    };
    let (Some(x), Some(y), Some(blur), Some(spread)) = (
        points(shadow.offset[0].as_ref()),
        points(shadow.offset[1].as_ref()),
        points(shadow.blur.as_ref()),
        points(shadow.spread.as_ref()),
    ) else {
        return;
    };
    if blur < 0.0 || spread < 0.0 || !rect.is_positive() {
        return;
    }
    let bounds = rect.translate(egui::vec2(x, y)).expand(spread);
    // Epaint centers its feathering on the edge. Use a full width of
    // twice the blur distance to reach about one blur distance outward.
    ui.painter().add(
        egui::epaint::RectShape::filled(bounds, CornerRadius::ZERO, color(tint))
            .with_blur_width(2.0 * blur),
    );
}

fn paint_box(ui: &Ui, rect: Rect, style: &ResolvedStyle, kind: ObjectKind) {
    let painter = ui.painter();
    if kind == ObjectKind::Line {
        line::paint(painter, rect, 0, style);
        return;
    }
    let radius = style.border_radius.unwrap_or(0.0).clamp(0.0, 255.0) as u8;
    if let Some(fill) = style.background.map(color) {
        if kind == ObjectKind::Oval {
            painter.add(egui::epaint::EllipseShape::filled(
                rect.center(),
                rect.size() / 2.0,
                fill,
            ));
        } else {
            painter.rect_filled(rect, CornerRadius::same(radius), fill);
        }
    }
    let segments = [
        [rect.left_top(), rect.right_top()],
        [rect.right_top(), rect.right_bottom()],
        [rect.left_bottom(), rect.right_bottom()],
        [rect.left_top(), rect.left_bottom()],
    ];
    for (side, points) in segments.into_iter().enumerate() {
        if let Some(width) = border_width(style, side) {
            painter.line_segment(
                points,
                Stroke::new(
                    width,
                    style.border_color[side]
                        .map(color)
                        .unwrap_or(Color32::BLACK),
                ),
            );
        }
    }
}

/// The width of the rule `style` draws on `side` (top, right, bottom,
/// left), `None` when it draws none: no positive width, or border style 0
/// or 1 (none, layout-model.md). Painting and the space a frame takes from
/// its contents (a portal's rows) both follow this.
fn border_width(style: &ResolvedStyle, side: usize) -> Option<f32> {
    let width = style.border_width[side]?.clamp(0.0, 1000.0) as f32;
    (width > 0.0 && !matches!(style.border_style[side], Some(0 | 1))).then_some(width)
}

fn paint_text(ui: &mut Ui, rect: Rect, text: &str, style: &ResolvedStyle) {
    let _ = paint_rich_text(ui, rect, text, style, None, None);
}

fn paint_rich_text(
    ui: &mut Ui,
    rect: Rect,
    text: &str,
    style: &ResolvedStyle,
    rich: Option<&om_layout::StyledText>,
    field: Option<&egui::Response>,
) -> Option<egui::text::CCursor> {
    if text.is_empty() {
        return None;
    }
    let rect = Rect::from_min_max(
        rect.min
            + vec2(
                style.padding[3].unwrap_or(0.0) as f32,
                style.padding[0].unwrap_or(0.0) as f32,
            ),
        rect.max
            - vec2(
                style.padding[1].unwrap_or(0.0) as f32,
                style.padding[2].unwrap_or(0.0) as f32,
            ),
    );
    if !rect.is_positive() {
        return None;
    }
    let color = style.color.map(color).unwrap_or(Color32::BLACK);
    let vertical = match style.vertical_align {
        Some(1) => Align::Center,
        Some(2) => Align::Max,
        _ => Align::Min,
    };
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    let scale = child
        .ctx()
        .layer_transform_to_global(child.layer_id())
        .map_or(1.0, |t| t.scaling);
    let galley = child.fonts_mut(|fonts| {
        let job = rich_text::layout_job(fonts, text, style, rich, rect.width());
        scaled_text::layout_galley(fonts, job, scale)
    });
    let align = galley.job.halign;
    // Native fields keep their first line visible when text exceeds the
    // box; centering an overflowing block would cut that line in half.
    let extra_height = (rect.height() - galley.size().y).max(0.0);
    let y = match vertical {
        Align::Center => rect.top() + extra_height / 2.0,
        Align::Max => rect.top() + extra_height,
        Align::Min => rect.top(),
    };
    let x = match align {
        Align::Min => rect.left(),
        Align::Center => rect.center().x,
        Align::Max => rect.right(),
    };
    let response = child.allocate_rect(rect, Sense::hover());
    if field.is_none() {
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, child.is_enabled(), text));
    }
    let cursor = field
        .filter(|response| response.clicked())
        .and_then(egui::Response::interact_pointer_pos)
        .map(|pointer| {
            source_cursor(
                text,
                style.text_transform == Some(3),
                galley.cursor_from_pos(pointer - pos2(x, y)),
            )
        });
    child.painter().galley(pos2(x, y), galley, color);
    cursor
}

/// The painted uppercase form can expand a character (ß -> SS). Editors use
/// source character offsets, never UTF-8 byte or transformed-text offsets.
fn source_cursor(
    text: &str,
    uppercase: bool,
    mut cursor: egui::text::CCursor,
) -> egui::text::CCursor {
    let mut painted = 0;
    let mut source = 0;
    for ch in text.chars() {
        if painted >= cursor.index.0 {
            break;
        }
        painted += if uppercase {
            ch.to_uppercase().count()
        } else {
            1
        };
        source += 1;
    }
    cursor.index = source.into();
    cursor
}

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod visibility_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refresh_preserves_an_override_cleared_to_the_stored_value() {
        let mut edit = RelatedBuffer::new(1, Some(7), "AB", Some("ABC".into()));
        edit.buffer.value = "AB".into();
        let newer = crate::calc::RelatedRecord {
            occurrence: 1,
            table: 2,
            record: 3,
            field: 4,
            cell: 4,
            name: "Name".into(),
            stored: "XY".into(),
            version: Some(8),
        };
        edit.refresh(&newer, Some("ABC".into()));
        assert!(edit.changed());
        assert_eq!(edit.buffer.value, "AB");
        assert_eq!(edit.buffer.original, "AB");
        assert_eq!(edit.version, Some(7));
    }

    #[test]
    fn button_icon_above_or_below_leaves_the_full_width_for_the_label() {
        let frame = Rect::from_min_size(pos2(20.0, 40.0), vec2(100.0, 90.0));
        let (above, below_label) = button_icon_and_label(frame, 48.0, 2, true, || 18.0);
        let (below, above_label) = button_icon_and_label(frame, 48.0, 3, true, || 18.0);
        assert_eq!(below_label.width(), frame.width());
        assert_eq!(above_label.width(), frame.width());
        assert_eq!(above.center().x, frame.center().x);
        assert_eq!(below.center().x, frame.center().x);
        assert_eq!(below_label.top() - above.bottom(), 8.0);
        assert_eq!(below.top() - above_label.bottom(), 8.0);
    }

    fn layout_button_icon(
        frame: Rect,
        button: om_layout::ButtonObject,
        has_library_image: bool,
    ) -> (Option<Rect>, bool) {
        let mut layout = Layout::default();
        if has_library_image {
            layout.library_images.insert(
                button.library_key,
                om_layout::LibraryImage {
                    format: om_layout::LibraryImageFormat::Svg,
                    data: br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><circle cx="4" cy="4" r="4"/></svg>"#.to_vec(),
                },
            );
        }
        let object = LayoutObject {
            button: Some(button),
            ..Default::default()
        };
        let cache = layout_cache::LayoutCache::default();
        let ctx = egui::Context::default();
        let mut result = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            result = Some(paint_label_icon(
                ui,
                &cache,
                (&layout, &object),
                frame,
                ("Open list", &ResolvedStyle::default(), None),
                ButtonIconMode::Layout,
                (1, None),
            ));
        })
        .drop_without_applying_deltas();
        result.unwrap()
    }

    fn button_frame() -> Rect {
        Rect::from_min_size(pos2(20.0, 40.0), vec2(100.0, 90.0))
    }

    #[test]
    fn missing_layout_button_icon_keeps_the_full_label() {
        let frame = button_frame();
        let button = om_layout::ButtonObject {
            library_key: 7,
            icon_size: 48,
            display_type: 4,
            ..Default::default()
        };
        assert_eq!(
            layout_button_icon(frame, button, false),
            (Some(frame), false)
        );
    }

    #[test]
    fn missing_image_only_button_icon_requests_the_layout_fallback() {
        let button = om_layout::ButtonObject {
            icon_size: 48,
            display_type: 1,
            ..Default::default()
        };
        assert_eq!(
            layout_button_icon(button_frame(), button, false),
            (None, false)
        );
    }

    #[test]
    fn layout_image_only_button_centers_its_icon_without_a_label() {
        let frame = button_frame();
        let button = om_layout::ButtonObject {
            library_key: 7,
            icon_size: 48,
            display_type: 1,
            ..Default::default()
        };
        let (label, has_icon) = layout_button_icon(frame, button, true);
        assert_eq!((label, has_icon), (None, true));
        let (icon, _) = button_icon_and_label(frame, 48.0, 1, label.is_some(), || {
            panic!("an image-only button does not measure a label")
        });
        assert_eq!(icon.center(), frame.center());
    }

    #[test]
    fn layout_calculated_icon_skips_a_stale_library_key() {
        let frame = button_frame();
        let button = om_layout::ButtonObject {
            flags: 0x4,
            library_key: 7,
            icon_size: 48,
            display_type: 5,
            ..Default::default()
        };
        assert_eq!(
            layout_button_icon(frame, button, true),
            (Some(frame), false)
        );
    }

    #[test]
    fn clipped_calculated_icons_do_not_claim_visible_cache_slots() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let clip = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 40.0));
            ui.set_clip_rect(clip);
            let outside = clip.translate(vec2(0.0, 40.0));
            for icon in 0..MAX_CALCULATED_ICONS {
                paint_calculated_icon(ui, &format!("<svg>{icon}</svg>"), outside, None);
            }
            let id = egui::Id::unique("openmaker calculated icons");
            assert!(ui.data(|data| data.get_temp::<CalculatedIcons>(id).is_none()));
            paint_calculated_icon(ui, "<svg/>", clip.shrink(10.0), None);
            assert_eq!(
                ui.data(|data| data.get_temp::<CalculatedIcons>(id).unwrap().0.len()),
                1
            );
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn svg_tint_starts_inside_the_root_after_an_xml_prolog() {
        let data = br#"<?xml version="1.0"?>
<!-- Example <svg -->
<?example svg?>
<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" data-example=">"><rect class="fm_fill" width="8" height="8"/></svg>"#;
        for data in [data.to_vec(), [b"\xef\xbb\xbf".as_slice(), data].concat()] {
            let image = om_layout::LibraryImage {
                format: om_layout::LibraryImageFormat::Svg,
                data,
            };
            let tinted = tinted_image(&image, Some(Color32::RED));
            let picture = crate::report::browse_svg(&tinted, [8, 8]).unwrap();
            assert_eq!(&picture.rgba[..4], &[255, 0, 0, 255]);
        }
        assert!(crate::report::browse_svg(b"<svg>", [8, 8]).is_err());
    }

    /// A field click waiting for its row (#1099) keeps waiting through a
    /// frame that cannot tell yet whether its row shows, and goes once
    /// another field takes the keyboard: it never takes the focus from
    /// the field the user chose since.
    #[test]
    fn a_field_taking_the_keyboard_ends_a_waiting_field_click() {
        let mut state = DocumentState::default();
        let wait = FieldClickWait {
            field: RelatedField {
                layout: 1,
                parent: 1,
                table: 2,
                row: RelatedRow::Record(3),
                field: 4,
            },
            object: 7,
            seen: true,
        };
        state.field_click = Some(wait);
        state.begin_frame();
        state.visibility_undecided();
        state.end_frame();
        assert!(state.field_click.is_some(), "still deciding");
        state.begin_frame();
        state.visibility_undecided();
        state.seen_active = Some("People::Note".into());
        state.seen_object = Some(3);
        let (_, entered, _) = state.end_frame();
        assert!(entered.is_some());
        assert!(
            state.field_click.is_none(),
            "another field has the keyboard"
        );
    }

    /// A portal row field given the focus back after a refused exit
    /// (#1050) waits through a frame whose hide conditions are still being
    /// decided, and through one painting its portal before its editor, and
    /// is let go once a frame that decided paints no such portal.
    #[test]
    fn a_refocused_row_waits_for_its_portal_and_goes_with_it() {
        let mut state = DocumentState::default();
        let focus = RelatedFocus {
            object: 7,
            field: RelatedField {
                layout: 1,
                parent: 1,
                table: 2,
                row: RelatedRow::Record(3),
                field: 4,
            },
            portal: Some(5),
            occurrence: 6,
            row: Some(1),
            repetition: 1,
        };
        state.refocus_related("Notes::Body".into(), focus);
        state.begin_frame();
        state.visibility_undecided();
        state.end_frame();
        assert_eq!(state.held_related_object(), Some(7), "still deciding");
        state.begin_frame();
        state.shown_portals[0].insert((1, 5, 1), std::sync::Arc::from(vec![3u64]));
        state.end_frame();
        assert_eq!(state.held_related_object(), Some(7), "its portal painted");
        state.begin_frame();
        state.end_frame();
        assert_eq!(state.held_related_object(), None, "its portal gone");
    }

    /// The original editor remains live while preparation runs; a reply for a
    /// discarded editor cannot approve a later candidate with the same key.
    #[test]
    fn entry_candidates_keep_input_until_their_own_preparation_answers() {
        let mut state = DocumentState::default();
        state.capabilities.prepare_edit = true;
        let key = (3, 1, "Name".to_owned());
        let mut buffer = FieldBuffer::new("stored");
        buffer.accepted_version = Some(7);
        buffer.value = "typed".into();
        buffer.ui_edited = true;
        state.buffers.insert(key.clone(), buffer);
        let (token, request) = state.prepare_candidates(true).pop().unwrap();
        assert_eq!(request.baseline.version, Some(7));
        assert!(state.drafts().is_empty());
        let mut left = Some(("Name".into(), Some(5)));
        let mut entered = None;
        let mut modified = Vec::new();
        let find = Action::SetMode(crate::Mode::Find);
        let command = Action::MenuFieldExit(Box::new(find.clone()));
        let mut actions = vec![find];
        state.active = Some("Name".into());
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut close = vec![Action::CloseFile];
            state.menu_leaves_field(ui, &mut close);
            assert_eq!(close, [Action::CloseFile]);
            state.hold_candidate_actions(&mut None, &mut None, &mut Vec::new(), &mut close);
            assert_eq!(close, [Action::CloseFile]);

            state.menu_leaves_field(ui, &mut actions);
            assert_eq!(actions.as_slice(), std::slice::from_ref(&command));
            state.hold_candidate_actions(&mut left, &mut entered, &mut modified, &mut actions);
            assert!(actions.is_empty());
            assert!(left.is_none());

            state.active = None;
            let mut second = vec![Action::Quit];
            state.menu_leaves_field(ui, &mut second);
            assert!(second.is_empty(), "Quit cannot bypass the pending exit");
            let mut close = vec![Action::CloseFile];
            state.menu_leaves_field(ui, &mut close);
            assert_eq!(close, [Action::CloseFile]);
        })
        .drop_without_applying_deltas();
        state
            .buffers
            .get_mut(&key)
            .unwrap()
            .value
            .push_str(" paste IME");
        assert!(state.accept_edit_preparation(token, &request, true));
        assert_eq!(state.drafts()[&(3, 1)][0].value, "typed paste IME");
        state.hold_candidate_actions(&mut left, &mut entered, &mut modified, &mut actions);
        assert_eq!(left, Some(("Name".into(), Some(5))));
        assert_eq!(actions, [command]);
        state.clear_record(3, 1);
        let mut next = FieldBuffer::new("stored");
        next.value = "next".into();
        next.ui_edited = true;
        state.buffers.insert(key, next);
        let (new_token, new_request) = state.prepare_candidates(true).pop().unwrap();
        assert!(!state.accept_edit_preparation(token, &request, true));
        assert!(state.drafts().is_empty());
        // Both Prepared and Unavailable acknowledge ordinary entry; errors do
        // not. Only the owning provider knows whether recovery was prepared.
        assert!(state.accept_edit_preparation(new_token, &new_request, true));
        assert_eq!(state.drafts()[&(3, 1)][0].value, "next");
        state.clear_record(3, 1);
        let mut failed = FieldBuffer::new("stored");
        failed.accepted_version = Some(7);
        failed.value = "refused stale input".into();
        failed.ui_edited = true;
        state.buffers.insert((3, 1, "Name".into()), failed);
        let (failed_token, failed_request) = state.prepare_candidates(true).pop().unwrap();
        assert!(state.accept_edit_preparation(failed_token, &failed_request, false));
        assert!(state.failed_edit());
        assert!(state.drafts().is_empty());
        state.capabilities.prepare_edit = false;
        assert!(
            state.drafts().is_empty(),
            "a capability refresh cannot accept refused input"
        );
        state.capabilities.prepare_edit = true;
        let mut actions = vec![Action::DismissAlert, Action::CommitRecord];
        state.hold_candidate_actions(&mut None, &mut None, &mut Vec::new(), &mut actions);
        assert!(matches!(actions.as_slice(), [Action::DismissAlert]));
        let mut actions = vec![Action::DiscardEntryCandidates];
        state.hold_candidate_actions(&mut None, &mut None, &mut Vec::new(), &mut actions);
        assert!(matches!(
            actions.as_slice(),
            [Action::DiscardEntryCandidates]
        ));
        assert_eq!(
            state.entry_candidate_records(),
            [(3, 1)].into_iter().collect()
        );
        assert_eq!(
            state.buffers[&(3, 1, "Name".into())].value,
            "refused stale input"
        );
        state.put_draft(3, 1, "Other", "original".into(), "accepted edit".into());
        state.discard_failed_edits();
        assert!(!state.failed_edit());
        assert_eq!(state.unsaved(), 1);
        assert_eq!(state.drafts()[&(3, 1)][0].value, "accepted edit");
        assert!(!state.accept_edit_preparation(failed_token, &failed_request, true));
        assert!(
            state.held_edit_actions.is_empty(),
            "discard never replays Save"
        );
    }

    /// Leaving a field auto-complete completed keeps the item's text only
    /// while the field still holds exactly the completed text (#870): typing
    /// on, Backspace or a script's draft (`ACME`, set on purpose) is kept.
    #[test]
    fn leaving_a_completed_field_keeps_the_item_only_over_the_completed_text() {
        let completion = Completion {
            key: (),
            editor: egui::Id::unique("company"),
            text: "acme".to_owned(),
            item: "Acme".to_owned(),
        };
        let left = |value: &str| {
            let mut buffer = FieldBuffer::new("");
            buffer.value = value.to_owned();
            buffer.accept_completion(&completion);
            (buffer.value, buffer.ui_edited)
        };
        assert_eq!(left("acme"), ("Acme".to_owned(), true));
        assert_eq!(left("ACME"), ("ACME".to_owned(), false));
        assert_eq!(left("ac"), ("ac".to_owned(), false));
        assert_eq!(left("acmex"), ("acmex".to_owned(), false));
    }

    #[test]
    fn transformed_caret_offsets_address_source_characters() {
        let source = "aß😀z";
        for (painted, original) in [(0, 0), (1, 1), (2, 2), (3, 2), (4, 3), (5, 4)] {
            assert_eq!(
                source_cursor(source, true, egui::text::CCursor::new(painted))
                    .index
                    .0,
                original
            );
        }
        assert_eq!(
            source_cursor(source, false, egui::text::CCursor::new(3))
                .index
                .0,
            3
        );
    }

    fn object(id: u64, parent: Option<u64>, flags: u64) -> LayoutObject {
        LayoutObject {
            id,
            parent,
            flags,
            bounds: Some(om_layout::Rect {
                left: 10.0,
                top: 20.0,
                right: 110.0,
                bottom: 70.0,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn anchors_stretch_or_move_objects_in_their_container() {
        let layout = Layout {
            objects: om_layout::ObjectStore {
                objects: vec![object(1, None, 0xf0000000), object(2, Some(1), 0xc0000000)],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            resized_bounds(&layout, 1, vec2(80.0, 60.0)),
            Some(Rect::from_min_max(pos2(10.0, 20.0), pos2(190.0, 130.0)))
        );
        assert_eq!(
            resized_bounds(&layout, 2, vec2(80.0, 60.0)),
            Some(Rect::from_min_max(pos2(100.0, 100.0), pos2(200.0, 150.0)))
        );
    }

    #[test]
    fn fixed_containers_constrain_child_anchors_and_cycles_are_rejected() {
        let mut layout = Layout {
            objects: om_layout::ObjectStore {
                objects: vec![object(1, None, 0x30000000), object(2, Some(1), 0xf0000000)],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            resized_bounds(&layout, 2, vec2(80.0, 60.0)),
            Some(Rect::from_min_max(pos2(20.0, 40.0), pos2(120.0, 90.0)))
        );
        layout.objects.objects[0].parent = Some(2);
        assert!(resized_bounds(&layout, 2, vec2(80.0, 60.0)).is_none());
    }
}
