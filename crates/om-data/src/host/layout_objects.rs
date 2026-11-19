//! `GetLayoutObjectAttribute` (calc-functions.md 5.8, "Layout objects"):
//! the attributes of a named object of the window's layout, from the layout
//! model (`om_layout`) and what the host knows about its window: where the
//! layout sits on the screen, the object with focus, whether the body shows
//! a record, a field's formatted contents and an object's hide condition.
//!
//! FileMaker Server's script engine answers in layout coordinates (its
//! window has no toolbar and sits at 0, 0); FileMaker Pro in screen points
//! from the top left of the desktop area (calc-functions.md 6.3 O5), where
//! the window shows the layout ([`ScreenView`]): zoomed below the top
//! navigation part, objects anchored to the window's edges moved or
//! stretched by the room the window has beyond the layout, a list's body
//! at the current record's row.

use om_calc::eval::{CalcHost, Context};
use om_calc::{Ctx, Value};
use om_layout::{Layout, LayoutObject, ObjectKind, PartKind, Rect};

use super::schema::CalcSchema;
use super::session::{SessionInfo, WindowMode};
use super::window::points;

/// What a host tells about the window showing the layout.
pub struct ObjectWindow<'a> {
    pub layout: &'a Layout,
    /// Where a desktop window shows the layout; `None` answers in layout
    /// coordinates, as FileMaker Server does.
    pub screen: Option<ScreenView>,
    /// The object with focus.
    pub focus: Option<u64>,
    /// Whether the body shows a record: a list or table view without
    /// records draws no body, so its objects have no bounds or contents.
    pub body_shown: bool,
}

/// A window's view of its layout (`Get ( LayoutViewState )`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Form,
    List,
    Table,
}

/// Where a desktop window shows its layout, in points.
#[derive(Clone, Copy, Debug)]
pub struct ScreenView {
    /// The content area's top left from the desktop area's, and its size.
    pub origin: (f64, f64),
    pub size: (f64, f64),
    /// 1.0 at 100 %.
    pub zoom: f64,
    pub view: View,
    /// Preview draws pages: objects have no bounds there.
    pub preview: bool,
    /// The current record's row in a list, 0 for the first.
    pub row: u64,
    /// The window is the active one of the active application.
    pub active: bool,
}

impl ScreenView {
    /// The view of the session's window, `None` without one.
    pub fn of(session: &SessionInfo) -> Option<ScreenView> {
        let window = session.window.as_ref()?;
        let view = match session.layout_view_state {
            Some(1) => View::List,
            Some(2) => View::Table,
            _ => View::Form,
        };
        Some(ScreenView {
            origin: window.content_origin(),
            size: (window.content.width, window.content.height),
            zoom: window.scale_factor(),
            view,
            preview: session.window_mode == WindowMode::Preview,
            row: session
                .active_record_number
                .filter(|_| session.window_mode != WindowMode::Find)
                .map_or(0, |n| n.saturating_sub(1)),
            active: window.active,
        })
    }
}

/// The host's answers about one object on the current record.
pub trait ObjectState {
    /// A field object's data formatted as the object shows it, in
    /// `repetition` (1-based) of portal row `row` (1-based); empty without a
    /// record.
    fn content(&mut self, object: &LayoutObject, repetition: u32, row: u32) -> String;
    /// Whether "Hide object when" holds for the current record.
    fn hidden(&mut self, object: &LayoutObject) -> bool;
}

/// A host with no record: fields show nothing and nothing is hidden.
pub struct NoRecord;

impl ObjectState for NoRecord {
    fn content(&mut self, _: &LayoutObject, _: u32, _: u32) -> String {
        String::new()
    }

    fn hidden(&mut self, _: &LayoutObject) -> bool {
        false
    }
}

/// The objects of `layout` depth first in object order.
pub fn objects_in_order(layout: &Layout) -> Vec<&LayoutObject> {
    let mut out = Vec::new();
    let mut stack: Vec<u64> = layout.objects.root.iter().rev().copied().collect();
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        let Some(object) = layout.objects.get(id) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        out.push(object);
        stack.extend(object.children.iter().rev());
    }
    out
}

/// The first object named `name` (in any case), depth first.
pub fn named_object<'l>(layout: &'l Layout, name: &str) -> Option<&'l LayoutObject> {
    let lower = name.to_lowercase();
    objects_in_order(layout)
        .into_iter()
        .find(|o| o.name.as_deref().is_some_and(|n| n.to_lowercase() == lower))
}

/// How a button bar chooses its active segment (layout-model.md 5.5;
/// Claris, "Defining or changing a button bar": None, one segment, or
/// "Specify", a formula that returns the object name of the segment).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SegmentChoice<'a> {
    /// No segment (None), or the one Button Bar Setup chose
    /// (`segmentedBarObj.activeSegmentKey`).
    Fixed(Option<u64>),
    /// The `KActiveObjectCalc` formula: its result names the segment
    /// ([`named_segment`]).
    Calculated(&'a om_formula::Formula),
    /// What the bar stores cannot be read as either: why.
    Undetermined(&'static str),
}

/// The active segment choice of `bar` (a button bar; any other object has
/// none). `segmentedBarObj.flags` bit `0x2` marks "Specify" [O: the one
/// calculated bar of the corpus (MBS Phidget Voltage Input, layout 1,
/// object 62) stores flags 2, no key and the formula `0`; its None bar and
/// every template bar store flags 0, the chosen segment's key or none].
/// A calculation that does not decode or is not stored, one stored on a
/// bar not marked as calculated, or a key that is not one of the bar's
/// segments is [`SegmentChoice::Undetermined`] rather than a guess.
pub fn segment_choice(bar: &LayoutObject) -> SegmentChoice<'_> {
    /// `segmentedBarObj.flags`: the active segment is calculated.
    const CALCULATED: u32 = 0x2;
    if bar.kind() != ObjectKind::ButtonBar {
        return SegmentChoice::Fixed(None);
    }
    let (flags, key) = bar
        .segmented_bar
        .as_ref()
        .map_or((0, 0), |b| (b.flags, b.active_segment_key));
    let undecodable = bar
        .calcs
        .undecodable
        .contains(&om_layout::calcs::ACTIVE_SEGMENT);
    let formula = bar.calcs.active_segment.as_ref();
    if flags & CALCULATED != 0 {
        return match formula {
            _ if undecodable => {
                SegmentChoice::Undetermined("its active segment calculation does not decode")
            }
            Some(formula) => SegmentChoice::Calculated(formula),
            None => SegmentChoice::Undetermined("its active segment calculation is not stored"),
        };
    }
    if formula.is_some() || undecodable {
        return SegmentChoice::Undetermined(
            "it stores an active segment calculation it is not set to use",
        );
    }
    match key {
        0 => SegmentChoice::Fixed(None),
        key if bar.children.contains(&key) => SegmentChoice::Fixed(Some(key)),
        _ => SegmentChoice::Undetermined("its active segment is not one of its segments"),
    }
}

/// The segment of `bar` an active segment calculation's result `name`
/// names: the object `GetLayoutObjectAttribute` finds by that name
/// ([`named_object`], any case), when it is one of the bar's segments.
/// Empty text, or a name of no segment of this bar, makes none active.
pub fn named_segment(layout: &Layout, bar: &LayoutObject, name: &str) -> Option<u64> {
    if name.is_empty() {
        return None;
    }
    named_object(layout, name)
        .filter(|object| object.parent == Some(bar.id))
        .map(|object| object.id)
}

/// `objectType` (catalogs/layout-object-types.csv): a tab control and its
/// panels are both `tab panel`, slide controls and panels `slide panel`.
pub fn object_type(object: &LayoutObject) -> &'static str {
    match object.kind() {
        ObjectKind::Field => "field",
        ObjectKind::Text => "text",
        ObjectKind::Graphic => "graphic",
        ObjectKind::Line => "line",
        ObjectKind::Rectangle => "rectangle",
        ObjectKind::RoundedRectangle => "rounded rectangle",
        ObjectKind::Oval => "oval",
        ObjectKind::Group if object.script_step().is_some() => "button group",
        ObjectKind::Group => "group",
        ObjectKind::Portal => "portal",
        ObjectKind::Button => "button",
        ObjectKind::TabControl | ObjectKind::TabPanel => "tab panel",
        // The style's object type tells a chart (85) and a web viewer (86)
        // from a plug-in object (layout-model.md 5.3).
        ObjectKind::External => match object.style.as_ref().map(|s| s.object_type_id) {
            Some(85) => "chart",
            Some(86) => "web viewer",
            _ => "external",
        },
        ObjectKind::PopoverButton => "popover button",
        ObjectKind::Popover => "popover",
        ObjectKind::SlideControl | ObjectKind::SlidePanel => "slide panel",
        ObjectKind::ButtonBar => "button bar",
        ObjectKind::Unknown(_) => "unknown",
    }
}

/// The part an object is in: the one its top edge falls in.
fn part_of(layout: &Layout, top: f64) -> Option<PartKind> {
    layout
        .parts
        .iter()
        .find(|p| top >= p.top && top < p.bottom())
        .map(|p| p.kind())
}

/// An object's rectangle in layout coordinates, a portal's objects in its
/// first row (inside the portal's drawn left and top borders), as
/// `FieldBounds` places them.
fn layout_rect(layout: &Layout, object: &LayoutObject) -> Option<Rect> {
    let mut rect = layout.absolute_bounds(object.id)?;
    let mut parent = object.parent;
    let mut depth = 0;
    while let Some(id) = parent {
        let container = layout.objects.get(id)?;
        if container.kind() == ObjectKind::Portal {
            let style = layout.object_style(container, 1, 1);
            let drawn = |side: usize| match style.border_style[side] {
                Some(s) if s != 1 && s != 2 => style.border_width[side].unwrap_or(0.0),
                _ => 0.0,
            };
            rect.left += drawn(3);
            rect.right += drawn(3);
            rect.top += drawn(0);
            rect.bottom += drawn(0);
            break;
        }
        depth += 1;
        if depth > 64 {
            break;
        }
        parent = container.parent;
    }
    Some(rect)
}

/// The rectangle of repetition `repetition` of a repeating field (`None`
/// past its repetitions shown): the object's rectangle split evenly down
/// or across [I].
fn repetition_rect(object: &LayoutObject, rect: Rect, repetition: u32) -> Option<Rect> {
    let Some(field) = object.field_obj.as_ref() else {
        return (repetition <= 1).then_some(rect);
    };
    let shown = field.num_of_reps.unsigned_abs().max(1);
    if repetition == 0 {
        return Some(rect);
    }
    if repetition > shown {
        return None;
    }
    let n = f64::from(shown);
    let i = f64::from(repetition - 1);
    let mut r = rect.clone();
    if field.num_of_reps < 0 {
        let w = rect.width() / n;
        r.left = rect.left + w * i;
        r.right = r.left + w;
    } else {
        let h = rect.height() / n;
        r.top = rect.top + h * i;
        r.bottom = r.top + h;
    }
    Some(r)
}

/// A coordinate as these attributes write it: whole points without a
/// decimal point, fractions as they are (layout coordinates are stored as
/// doubles rounded to 1e-6, layout-model.md 3).
fn number_text(n: f64) -> String {
    let n = (n * 1e6).round() / 1e6;
    if n == n.trunc() {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The UUID in FileMaker's form, `B0A97EC3-E710-45FD-B0A3-0EF1D7257532`.
pub fn uuid_text(bytes: &[u8; 16]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Anchor bits of an object's flags (layout-model.md 5.7).
const ANCHOR_LEFT: u64 = 0x1000_0000;
const ANCHOR_TOP: u64 = 0x2000_0000;
const ANCHOR_RIGHT: u64 = 0x4000_0000;
const ANCHOR_BOTTOM: u64 = 0x8000_0000;

/// One axis of an anchored object, `start..end`, when its container grows
/// by `grow`: anchored to both edges it stretches, to the far edge alone
/// it moves, to the near edge alone it stays, and to neither it keeps its
/// place in the middle [I].
fn anchored_axis(start: f64, end: f64, near: bool, far: bool, grow: f64) -> (f64, f64) {
    match (near, far) {
        (true, true) => (start, end + grow),
        (false, true) => (start + grow, end + grow),
        (true, false) => (start, end),
        (false, false) => (start + grow / 2.0, end + grow / 2.0),
    }
}

/// An object's rectangle (layout coordinates) once the window gives the
/// layout `grow` more points across and down, with the rectangle it had:
/// a root object follows its anchors in the window, a contained one in its
/// container as the container moved and grew (an enclosing container that
/// does not stretch on an axis keeps its contents' place on it). A
/// portal's rows do not stretch: the portal shows more of them.
fn anchored(layout: &Layout, id: u64, grow: (f64, f64), depth: usize) -> Option<(Rect, Rect)> {
    let object = layout.objects.get(id)?;
    let old = layout.absolute_bounds(id)?;
    let (dw, dh, dx, dy) = match object.parent {
        Some(parent) if depth < 64 => {
            let (new, was) = anchored(layout, parent, grow, depth + 1)?;
            let rows = layout
                .objects
                .get(parent)
                .is_some_and(|c| c.kind() == ObjectKind::Portal);
            (
                new.width() - was.width(),
                if rows {
                    0.0
                } else {
                    new.height() - was.height()
                },
                new.left - was.left,
                new.top - was.top,
            )
        }
        Some(_) => (0.0, 0.0, 0.0, 0.0),
        None => (grow.0, grow.1, 0.0, 0.0),
    };
    let flags = object.flags;
    let (left, right) = anchored_axis(
        old.left + dx,
        old.right + dx,
        flags & ANCHOR_LEFT != 0,
        flags & ANCHOR_RIGHT != 0,
        dw,
    );
    let (top, bottom) = anchored_axis(
        old.top + dy,
        old.bottom + dy,
        flags & ANCHOR_TOP != 0,
        flags & ANCHOR_BOTTOM != 0,
        dh,
    );
    let mut new = old.clone();
    (new.left, new.top, new.right, new.bottom) = (left, top, right, bottom);
    Some((new, old))
}

/// The parts a Browse window shows, in order, with where each starts below
/// the content area's top and its height there; title parts and
/// sub-summaries are not shown in Browse mode, the footer and the bottom
/// navigation sit at the bottom of the window [I], and the navigation parts
/// are not zoomed.
struct Placed {
    kind: PartKind,
    top: f64,
    height: f64,
    screen_top: f64,
    zoom: f64,
}

fn placed_parts(layout: &Layout, view: &ScreenView, grow_down: f64) -> Vec<Placed> {
    let mut parts: Vec<&om_layout::Part> = layout.parts.iter().collect();
    parts.sort_by(|a, b| {
        a.top
            .partial_cmp(&b.top)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let zoom = view.zoom;
    let mut placed = Vec::new();
    let mut cursor = 0.0;
    let mut bottom = view.size.1;
    for part in parts.iter().rev() {
        let kind = part.kind();
        let z = if kind == PartKind::BottomNavigation {
            1.0
        } else {
            zoom
        };
        if matches!(kind, PartKind::Footer | PartKind::BottomNavigation) {
            bottom -= part.height * z;
            placed.push(Placed {
                kind,
                top: part.top,
                height: part.height,
                screen_top: bottom,
                zoom: z,
            });
        }
    }
    for part in parts {
        let kind = part.kind();
        let z = if kind == PartKind::TopNavigation {
            1.0
        } else {
            zoom
        };
        match kind {
            PartKind::TitleHeader
            | PartKind::TitleFooter
            | PartKind::LeadingSubSummary
            | PartKind::TrailingSubSummary
            | PartKind::Footer
            | PartKind::BottomNavigation
            | PartKind::Other(_) => {}
            PartKind::Body => {
                let (row, grow) = match view.view {
                    View::Form => (0.0, grow_down),
                    _ => (view.row as f64, 0.0),
                };
                placed.push(Placed {
                    kind,
                    top: part.top,
                    height: part.height + grow,
                    screen_top: cursor + row * part.height * z,
                    zoom: z,
                });
                cursor += (part.height + grow) * z;
            }
            _ => {
                placed.push(Placed {
                    kind,
                    top: part.top,
                    height: part.height,
                    screen_top: cursor,
                    zoom: z,
                });
                cursor += part.height * z;
            }
        }
    }
    placed
}

/// Where an object's rectangle `rect` (layout coordinates, as
/// [`layout_rect`] places it) shows on the desktop, in whole points;
/// `None` where the window does not draw it (Preview, Table view, a part
/// Browse mode hides).
fn screen_rect(
    layout: &Layout,
    object: &LayoutObject,
    rect: Rect,
    view: &ScreenView,
) -> Option<Rect> {
    if view.preview || view.view == View::Table {
        return None;
    }
    let own = layout.absolute_bounds(object.id)?;
    let part_kind = part_of(layout, own.top);
    let kind = part_kind.unwrap_or(PartKind::Body);
    let fixed = matches!(kind, PartKind::TopNavigation | PartKind::BottomNavigation);
    let zoom = if fixed { 1.0 } else { view.zoom };
    let width = layout.def.as_ref().map_or(0.0, |d| d.base_width);
    let height: f64 = layout
        .parts
        .iter()
        .filter(|p| {
            !matches!(
                p.kind(),
                PartKind::TitleHeader
                    | PartKind::TitleFooter
                    | PartKind::LeadingSubSummary
                    | PartKind::TrailingSubSummary
                    | PartKind::Other(_)
            )
        })
        .map(|p| {
            if matches!(
                p.kind(),
                PartKind::TopNavigation | PartKind::BottomNavigation
            ) {
                p.height
            } else {
                p.height * view.zoom
            }
        })
        .sum();
    // The room beyond the layout, in layout points; a list's rows do not
    // stretch.
    let across = (view.size.0 - width * zoom).max(0.0) / zoom;
    let down = match view.view {
        View::Form if !fixed => (view.size.1 - height).max(0.0) / view.zoom,
        _ => 0.0,
    };
    let (moved, was) = anchored(layout, object.id, (across, down), 0)?;
    // An object outside every part (a layout without parts) is placed as
    // laid out.
    let part = match part_kind {
        Some(kind) => placed_parts(layout, view, down)
            .into_iter()
            .find(|p| p.kind == kind && own.top >= p.top && own.top < p.top + p.height.max(1.0))?,
        None => Placed {
            kind,
            top: 0.0,
            height: f64::MAX,
            screen_top: 0.0,
            zoom,
        },
    };
    let y = |v: f64| view.origin.1 + part.screen_top + (v - part.top) * part.zoom;
    let x = |v: f64| view.origin.0 + v * zoom;
    let mut out = rect.clone();
    out.left = points(x(rect.left + moved.left - was.left)) as f64;
    out.right = points(x(rect.right + moved.right - was.right)) as f64;
    out.top = points(y(rect.top + moved.top - was.top)) as f64;
    out.bottom = points(y(rect.bottom + moved.bottom - was.bottom)) as f64;
    Some(out)
}

/// Whether `object` is inside a portal.
fn in_portal(layout: &Layout, object: &LayoutObject) -> bool {
    let mut parent = object.parent;
    let mut depth = 0;
    while let Some(id) = parent {
        let Some(container) = layout.objects.get(id) else {
            return false;
        };
        if container.kind() == ObjectKind::Portal {
            return true;
        }
        depth += 1;
        if depth > 64 {
            return false;
        }
        parent = container.parent;
    }
    false
}

/// The first object showing field `field` of occurrence `table` (of the
/// layout's own occurrence when `table` is empty) on layout `layout_name`:
/// the active field's object (`Get ( ActiveLayoutObjectName )`).
pub fn focused_object<'s>(
    schema: &'s CalcSchema,
    layout_name: &str,
    table: &str,
    field: &str,
) -> Option<&'s LayoutObject> {
    if field.is_empty() {
        return None;
    }
    let entry = schema.layout_named(layout_name)?;
    let layout = entry.layout.as_deref()?;
    objects_in_order(layout).into_iter().find(|object| {
        let Some(spec) = object.field_spec() else {
            return false;
        };
        let (Some(occurrence), Some(id)) = (
            om_layout::occurrence_id(spec.table_key),
            om_hbam::serial::raw_of_id(spec.field_key),
        ) else {
            return false;
        };
        let names = schema.occurrence(occurrence).map(|o| o.name.as_str());
        let table_matches = if table.is_empty() {
            entry.occurrence == Some(occurrence)
        } else {
            names == Some(table)
        };
        table_matches
            && schema
                .field(occurrence, id)
                .is_some_and(|d| d.name == field)
    })
}

/// The first field object of `layout`'s tab order, where New Record and
/// Enter Find Mode put the focus (FileMaker Server's script engine, s33:
/// Title on Contacts' Contact Details, First Name on Contact List, Task on
/// Tasks' Task Details); `None` when no field is in the tab order.
pub fn first_in_tab_order(layout: &Layout) -> Option<&LayoutObject> {
    layout
        .objects
        .objects
        .iter()
        .filter(|o| o.tab_order > 0 && o.kind() == ObjectKind::Field && o.field_spec().is_some())
        .min_by_key(|o| o.tab_order)
}

/// `GetLayoutObjectOwnerInfo ( uuid )`: FileMaker Pro 26.0.3 and FileMaker
/// Server answer the not-found document for every object probed, in any
/// case and without hyphens (calc-functions.md 6.3 O5), CR line ends and
/// one tab per level.
pub fn owner_info_not_found() -> String {
    "{\r\t\"APIVers\" : 1,\r\t\"Object\" : \r\t{\r\t\t\"Index\" : 4294967295,\r\t\t\"Name\" : \"\",\r\t\t\"Owners\" : \r\t\t{\r\t\t\t\"Layout\" : \r\t\t\t{\r\t\t\t\t\"Name\" : \"\",\r\t\t\t\t\"UUID\" : \"\"\r\t\t\t}\r\t\t}\r\t}\r}".into()
}

/// `GetLayoutObjectAttribute ( name ; attribute ; repetition ; row )` on
/// the window's layout: empty text for an object or attribute that does
/// not exist. Names and attribute names match in any case; repetition and
/// row 0 act as 1 (the whole field for `bounds`).
pub fn layout_object_attribute(
    window: &ObjectWindow<'_>,
    schema: &CalcSchema,
    name: &str,
    attribute: &str,
    repetition: u32,
    row: u32,
    state: &mut dyn ObjectState,
) -> Value {
    let layout = window.layout;
    let Some(object) = named_object(layout, name) else {
        return Value::empty_text();
    };
    let attribute = attribute.to_ascii_lowercase();
    let flag = |b: bool| Value::text(if b { "1" } else { "0" });
    // Body objects of a list without records are not drawn, nor is
    // anything in Table view.
    let screen = window.screen.as_ref();
    let in_body = layout
        .absolute_bounds(object.id)
        .map(|r| part_of(layout, r.top) == Some(PartKind::Body));
    let drawn = in_body.is_some_and(|body| window.body_shown || !body)
        && screen.map_or(true, |s| s.view != View::Table);
    let rect = || -> Option<Rect> {
        if !drawn {
            return None;
        }
        // On a desktop a list's body object has no portal row (O5).
        if let Some(s) = screen {
            if s.view == View::List
                && row > 0
                && in_body == Some(true)
                && !in_portal(layout, object)
            {
                return None;
            }
        }
        let mut r = layout_rect(layout, object)?;
        let row_height = portal_row_height(layout, object);
        let rows = f64::from(row.max(1) - 1);
        r.top += row_height * rows;
        r.bottom += row_height * rows;
        let r = repetition_rect(object, r, repetition)?;
        match screen {
            Some(view) => screen_rect(layout, object, r, view),
            None => Some(r),
        }
    };
    let geometry = |f: &dyn Fn(&Rect) -> String| rect().map_or_else(String::new, |r| f(&r));
    let text = match attribute.as_str() {
        "objecttype" => object_type(object).to_owned(),
        "hasfocus" => return flag(window.focus == Some(object.id)),
        "containsfocus" => {
            return flag(
                window
                    .focus
                    .is_some_and(|f| f == object.id || is_within(layout, f, object.id)),
            )
        }
        "bounds" => geometry(&|r| {
            format!(
                "{} {} {} {} {}",
                number_text(r.left),
                number_text(r.top),
                number_text(r.right),
                number_text(r.bottom),
                object.rotation
            )
        }),
        "left" => geometry(&|r| number_text(r.left)),
        "right" => geometry(&|r| number_text(r.right)),
        "top" => geometry(&|r| number_text(r.top)),
        "bottom" => geometry(&|r| number_text(r.bottom)),
        "width" => geometry(&|r| number_text(r.width())),
        "height" => geometry(&|r| number_text(r.height())),
        "rotation" => object.rotation.to_string(),
        "startpoint" => geometry(&|r| format!("{} {}", number_text(r.left), number_text(r.top))),
        "endpoint" => geometry(&|r| format!("{} {}", number_text(r.right), number_text(r.bottom))),
        "source" => source(schema, object),
        "content" if !drawn => String::new(),
        "content" => match object.kind() {
            ObjectKind::Field => state.content(object, repetition.max(1), row.max(1)),
            // A page number or count is only known while printing: `?`,
            // with no error (O3, O4).
            ObjectKind::Text if has_page_symbol(object.text.as_deref().unwrap_or_default()) => {
                "?".into()
            }
            ObjectKind::Button | ObjectKind::PopoverButton | ObjectKind::Text => {
                object.text.clone().unwrap_or_default()
            }
            _ => String::new(),
        },
        "enclosingobject" => object
            .parent
            .and_then(|p| layout.objects.get(p))
            .and_then(|p| p.name.clone())
            .unwrap_or_default(),
        "containedobjects" => {
            let mut names = Vec::new();
            for child in &object.children {
                collect_names(layout, *child, &mut names, 0);
            }
            names.join("\r")
        }
        "isfronttabpanel" | "isfrontpanel" => {
            let front = matches!(object.kind(), ObjectKind::TabPanel | ObjectKind::SlidePanel)
                && object
                    .parent
                    .and_then(|p| layout.objects.get(p))
                    .is_some_and(|control| front_panel(control) == Some(object.id));
            return flag(front);
        }
        "isobjecthidden" => return flag(state.hidden(object)),
        // FileMaker Server answers empty; FileMaker Pro 0 unless the object
        // has focus in the active window (with focus in a background
        // application 0, O5).
        "isactive" => match screen {
            Some(s) => return flag(s.active && window.focus == Some(object.id)),
            None => String::new(),
        },
        "uuid" => object.uuid.as_ref().map(uuid_text).unwrap_or_default(),
        _ => String::new(),
    };
    Value::text(text)
}

/// Whether layout text holds `{{PageNumber}}` or `{{PageCount}}`, by name
/// or by its selector's number (`{{1015}}`, `{{1125}}`; calc-values.md
/// "Layout calculations").
fn has_page_symbol(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["{{pagenumber}}", "{{pagecount}}", "{{1015}}", "{{1125}}"]
        .iter()
        .any(|symbol| lower.contains(symbol))
}

/// A portal row's height: the portal's height over its rows; 0 outside a
/// portal.
fn portal_row_height(layout: &Layout, object: &LayoutObject) -> f64 {
    let mut parent = object.parent;
    let mut depth = 0;
    while let Some(id) = parent {
        let Some(container) = layout.objects.get(id) else {
            return 0.0;
        };
        if let Some(portal) = container.portal.as_ref() {
            let rows = portal.num_rows.max(1.0);
            return container.bounds.as_ref().map_or(0.0, |b| b.height() / rows);
        }
        depth += 1;
        if depth > 64 {
            return 0.0;
        }
        parent = container.parent;
    }
    0.0
}

/// Whether object `inner` is inside `outer`.
fn is_within(layout: &Layout, inner: u64, outer: u64) -> bool {
    let mut at = layout.objects.get(inner).and_then(|o| o.parent);
    let mut depth = 0;
    while let Some(id) = at {
        if id == outer {
            return true;
        }
        depth += 1;
        if depth > 64 {
            return false;
        }
        at = layout.objects.get(id).and_then(|o| o.parent);
    }
    false
}

/// The front panel of a tab or slide control: the current panel, else the
/// default, else the first.
fn front_panel(control: &LayoutObject) -> Option<u64> {
    let tab = control.tab.as_ref();
    tab.map(|t| t.current_panel_key)
        .filter(|k| *k != 0)
        .or_else(|| tab.map(|t| t.default_panel_key).filter(|k| *k != 0))
        .or_else(|| control.children.first().copied())
}

fn collect_names(layout: &Layout, id: u64, out: &mut Vec<String>, depth: usize) {
    let Some(object) = layout.objects.get(id) else {
        return;
    };
    if depth > 64 {
        return;
    }
    if let Some(name) = &object.name {
        out.push(name.clone());
    }
    for child in &object.children {
        collect_names(layout, *child, out, depth + 1);
    }
}

/// `source`: a field's `Occurrence::Field`, a portal's occurrence, a text
/// object's text with its merge fields as written; empty for the rest (a
/// web viewer's address is a calculation the host evaluates, FileMaker
/// Server answers empty).
fn source(schema: &CalcSchema, object: &LayoutObject) -> String {
    match object.kind() {
        ObjectKind::Field => object
            .field_spec()
            .and_then(|spec| {
                let occurrence = om_layout::occurrence_id(spec.table_key)?;
                let field = om_hbam::serial::raw_of_id(spec.field_key)?;
                let def = schema.field(occurrence, field)?;
                Some(format!(
                    "{}::{}",
                    schema.occurrence(occurrence)?.name,
                    def.name
                ))
            })
            .unwrap_or_default(),
        ObjectKind::Portal => object
            .portal
            .as_ref()
            .and_then(|p| om_layout::occurrence_id(p.table_alias_key))
            .and_then(|o| schema.occurrence(o))
            .map(|o| o.name.clone())
            .unwrap_or_default(),
        ObjectKind::Text => object.text.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// `GetLayoutObjectAttribute ( name ; attribute { ; repetition ; row } )`
/// for a host that evaluates through `host`: on the session window's layout
/// of `schema`, the record of `context` (a field object shows its field
/// there, a related one the portal row's record), with the session's active
/// field having focus, in screen points when the session has a desktop
/// window ([`ScreenView::of`]). A list or table view without records draws
/// no body.
pub fn answer(
    host: &mut dyn CalcHost,
    schema: &CalcSchema,
    session: &SessionInfo,
    args: &[Value],
    context: &Context,
) -> Value {
    let cx = Ctx::new(host.locale(), host.today());
    let text = |i: usize| {
        args.get(i)
            .map(|v| v.to_text(&cx).into_string())
            .unwrap_or_default()
    };
    let number = |i: usize| {
        args.get(i)
            .and_then(|v| v.to_number(&cx).to_i64())
            .map_or(0, |n| u32::try_from(n.max(0)).unwrap_or(u32::MAX))
    };
    let (name, attribute, repetition, row) = (text(0), text(1), number(2), number(3));
    let Some(entry) = schema.layout_named(&session.layout_name) else {
        return Value::empty_text();
    };
    let Some(layout) = entry.layout.as_deref() else {
        return Value::empty_text();
    };
    let focus = focused_object(
        schema,
        &session.layout_name,
        &session.active_field_table_name,
        &session.active_field_name,
    )
    .map(|o| o.id);
    let screen = ScreenView::of(session);
    let list = match &screen {
        Some(view) => view.view != View::Form,
        None => layout.def.as_ref().is_some_and(|d| d.flags & 0x4200 != 0),
    };
    let window = ObjectWindow {
        layout,
        screen,
        focus,
        body_shown: !list || session.found_count.unwrap_or(0) > 0,
    };
    let mut state = HostState { host, context };
    layout_object_attribute(
        &window, schema, &name, &attribute, repetition, row, &mut state,
    )
}

/// Object contents read through a calculation host on the context's record.
struct HostState<'h, 'c> {
    host: &'h mut dyn CalcHost,
    context: &'c Context,
}

impl ObjectState for HostState<'_, '_> {
    fn content(&mut self, object: &LayoutObject, repetition: u32, row: u32) -> String {
        let (Some(occurrence), Some(record)) = (self.context.occurrence, self.context.record)
        else {
            return String::new();
        };
        let Some(spec) = object.field_spec() else {
            return String::new();
        };
        let (Some(field_occurrence), Some(field)) = (
            om_layout::occurrence_id(spec.table_key),
            om_hbam::serial::raw_of_id(spec.field_key),
        ) else {
            return String::new();
        };
        let target = if field_occurrence == occurrence {
            Some(record)
        } else {
            self.host
                .related_record(
                    occurrence,
                    record,
                    field_occurrence,
                    row.max(1) as usize - 1,
                )
                .ok()
                .flatten()
        };
        let Some(target) = target else {
            return String::new();
        };
        let value = super::read_field(self.host, field_occurrence, target, field, repetition);
        om_calc::eval::display(&value, &*self.host)
    }

    fn hidden(&mut self, object: &LayoutObject) -> bool {
        match &object.calcs.hide {
            Some(hide) => super::layout::holds(self.host, hide, self.context),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use om_layout::{Layout, LayoutObject, ObjectStore, Rect, TabObject};

    use super::*;

    fn rect(left: f64, top: f64, right: f64, bottom: f64) -> Option<Rect> {
        Some(Rect {
            left,
            top,
            right,
            bottom,
            ..Rect::default()
        })
    }

    /// A tab control (1) at 10, 20 with panels 2 (front) and 3, panel 2
    /// holding text 4 "Label"; button 5 "Go" with a label and a UUID.
    fn sample() -> Layout {
        let objects = vec![
            LayoutObject {
                id: 1,
                object_type: 11,
                name: Some("Tabs".into()),
                bounds: rect(10.0, 20.0, 110.0, 220.0),
                children: vec![2, 3],
                tab: Some(TabObject {
                    default_panel_key: 2,
                    ..TabObject::default()
                }),
                ..LayoutObject::default()
            },
            LayoutObject {
                id: 2,
                object_type: 12,
                name: Some("One".into()),
                parent: Some(1),
                bounds: rect(0.0, 0.0, 100.0, 200.0),
                children: vec![4],
                ..LayoutObject::default()
            },
            LayoutObject {
                id: 3,
                object_type: 12,
                name: Some("Two".into()),
                parent: Some(1),
                bounds: rect(0.0, 0.0, 100.0, 200.0),
                ..LayoutObject::default()
            },
            LayoutObject {
                id: 4,
                object_type: 2,
                name: Some("Label".into()),
                parent: Some(2),
                bounds: rect(5.0, 6.5, 50.0, 26.5),
                text: Some("<<Name>>".into()),
                ..LayoutObject::default()
            },
            LayoutObject {
                id: 5,
                object_type: 10,
                name: Some("Go".into()),
                bounds: rect(300.0, 10.0, 400.0, 40.0),
                rotation: 90,
                text: Some("Contacts".into()),
                uuid: Some([
                    0xb0, 0xa9, 0x7e, 0xc3, 0xe7, 0x10, 0x45, 0xfd, 0xb0, 0xa3, 0x0e, 0xf1, 0xd7,
                    0x25, 0x75, 0x32,
                ]),
                ..LayoutObject::default()
            },
        ];
        Layout {
            objects: ObjectStore {
                root: vec![1, 5],
                objects,
                ..ObjectStore::default()
            },
            ..Layout::default()
        }
    }

    fn attribute(layout: &Layout, origin: (f64, f64), name: &str, attribute: &str) -> String {
        let schema = CalcSchema::new("f", om_calc::Locale::en_us());
        let window = ObjectWindow {
            layout,
            screen: (origin != (0.0, 0.0)).then_some(ScreenView {
                origin,
                size: (0.0, 0.0),
                zoom: 1.0,
                view: View::Form,
                preview: false,
                row: 0,
                active: false,
            }),
            focus: Some(4),
            body_shown: true,
        };
        match layout_object_attribute(&window, &schema, name, attribute, 0, 0, &mut NoRecord) {
            Value::Text(t) => t.as_str().to_owned(),
            other => panic!("{other:?}"),
        }
    }

    /// Bar 6 "Nav" with segments 7 "Entry" and 8 "Details"; text 9 is also
    /// named "Elsewhere".
    fn bar_layout(flags: u32, key: u64, calc: bool) -> Layout {
        let segment = |id, name: &str| LayoutObject {
            id,
            object_type: 10,
            name: Some(name.into()),
            parent: Some(6),
            ..LayoutObject::default()
        };
        let mut bar = LayoutObject {
            id: 6,
            object_type: 18,
            name: Some("Nav".into()),
            children: vec![7, 8],
            segmented_bar: Some(om_layout::SegmentedBarObject {
                flags,
                active_segment_key: key,
                ..Default::default()
            }),
            ..LayoutObject::default()
        };
        if calc {
            bar.calcs.active_segment = Some(om_formula::Formula {
                context: None,
                formatted: None,
                polish: om_formula::Expr::Empty,
            });
        }
        Layout {
            objects: ObjectStore {
                root: vec![6, 9],
                objects: vec![
                    bar,
                    segment(7, "Entry"),
                    segment(8, "Details"),
                    LayoutObject {
                        id: 9,
                        object_type: 2,
                        name: Some("Elsewhere".into()),
                        ..LayoutObject::default()
                    },
                ],
                ..ObjectStore::default()
            },
            ..Layout::default()
        }
    }

    #[test]
    fn button_bars_choose_their_active_segment_as_stored() {
        let choice = |l: &Layout| match segment_choice(l.objects.get(6).unwrap()) {
            SegmentChoice::Fixed(key) => format!("fixed {key:?}"),
            SegmentChoice::Calculated(_) => "calculated".into(),
            SegmentChoice::Undetermined(why) => why.into(),
        };
        // The templates' navigation bars and None (flags 0).
        assert_eq!(choice(&bar_layout(0, 8, false)), "fixed Some(8)");
        assert_eq!(choice(&bar_layout(0, 0, false)), "fixed None");
        // "Specify" (flags 2, as the corpus's calculated bar stores it).
        assert_eq!(choice(&bar_layout(2, 0, true)), "calculated");
        // What the stored bar does not settle is refused.
        assert_eq!(
            choice(&bar_layout(0, 9, false)),
            "its active segment is not one of its segments"
        );
        assert_eq!(
            choice(&bar_layout(0, 0, true)),
            "it stores an active segment calculation it is not set to use"
        );
        assert_eq!(
            choice(&bar_layout(2, 0, false)),
            "its active segment calculation is not stored"
        );
        let mut undecodable = bar_layout(2, 0, false);
        undecodable.objects.objects[0]
            .calcs
            .undecodable
            .push(om_layout::calcs::ACTIVE_SEGMENT);
        assert_eq!(
            choice(&undecodable),
            "its active segment calculation does not decode"
        );
    }

    #[test]
    fn a_calculated_name_is_the_segment_get_layout_object_attribute_finds() {
        let l = bar_layout(2, 0, true);
        let bar = l.objects.get(6).unwrap();
        // Any case, as GetLayoutObjectAttribute; the same object it describes.
        assert_eq!(named_segment(&l, bar, "details"), Some(8));
        assert_eq!(
            attribute(&l, (0.0, 0.0), "details", "enclosingObject"),
            "Nav"
        );
        assert_eq!(named_segment(&l, bar, "Entry"), Some(7));
        // Not a segment of this bar, no object, or empty: none is active.
        assert_eq!(named_segment(&l, bar, "Elsewhere"), None);
        assert_eq!(named_segment(&l, bar, "Nav"), None);
        assert_eq!(named_segment(&l, bar, "0"), None);
        assert_eq!(named_segment(&l, bar, ""), None);
    }

    #[test]
    fn attributes_describe_the_named_object() {
        let l = sample();
        let a = |name, attr| attribute(&l, (0.0, 0.0), name, attr);
        assert_eq!(a("go", "objectType"), "button");
        assert_eq!(a("Tabs", "OBJECTTYPE"), "tab panel");
        assert_eq!(a("Go", "bounds"), "300 10 400 40 90");
        assert_eq!(a("Go", "width"), "100");
        assert_eq!(a("Go", "startPoint"), "300 10");
        assert_eq!(a("Go", "endPoint"), "400 40");
        assert_eq!(a("Go", "content"), "Contacts");
        assert_eq!(a("Go", "UUID"), "B0A97EC3-E710-45FD-B0A3-0EF1D7257532");
        // Children are placed from their containers.
        assert_eq!(a("Label", "bounds"), "15 26.5 60 46.5 0");
        assert_eq!(a("Label", "source"), "<<Name>>");
        assert_eq!(a("Label", "enclosingObject"), "One");
        assert_eq!(a("Tabs", "containedObjects"), "One\rLabel\rTwo");
        assert_eq!(a("One", "isFrontPanel"), "1");
        assert_eq!(a("Two", "isFrontTabPanel"), "0");
        assert_eq!(a("Label", "hasFocus"), "1");
        assert_eq!(a("One", "containsFocus"), "1");
        assert_eq!(a("Two", "containsFocus"), "0");
        assert_eq!(a("Go", "isActive"), "");
        assert_eq!(a("Go", "nosuch"), "");
        assert_eq!(a("nosuch", "bounds"), "");
        // A desktop gives screen coordinates: the layout's origin added.
        assert_eq!(attribute(&l, (40.0, 100.0), "Go", "left"), "340");
    }
}
