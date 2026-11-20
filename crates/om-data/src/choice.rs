//! Choosing a field's value from its value list (issue #178;
//! layout-model.md 5.4): the items a field control offers, whether the user
//! may choose in it, and the edit choosing an item makes. The desktop's
//! option sets, pop-up menus and drop-down lists read their items here, and
//! `om-author`'s
//! `list_choices` and `choose_value_list_item` commands are these
//! operations (AGENTS.md rule 3).
//!
//! The items come from the calculation host's
//! [`FileCalc::value_list_entries`], with a list's second-field labels and
//! related values. The provider's field values for sorts and Insert from
//! Index (docs/value-lists-missing-index.md) are a separate path, which
//! applies neither.

use om_calc::eval::Context;
use om_layout::{FieldEntry, FieldSpec, LayoutObject};

use crate::host::{CalcSchema, FileCalc, ValueListEntry};
use crate::meta::{ValueList, ValueSource};

/// The most items a control offers. Past it the list is an error, never
/// its first items, as for the provider's field values
/// (docs/value-lists-missing-index.md).
pub const MAX_CHOICES: usize = 100_000;

/// The display type of a drop-down list (layout-model.md 5.4).
pub const DROP_DOWN_LIST: u32 = 1;

/// The display type of a pop-up menu (layout-model.md 5.4).
pub const POP_UP_MENU: u32 = 2;

/// A field object's flag: its menu ends with “Other…”, which asks for a
/// value to enter (layout-model.md 5.4, field object flags).
pub const OTHER_ITEM: u32 = 0x1;

/// A field object's flag: its menu ends with “Edit…”, which opens its
/// value list for editing (layout-model.md 5.4, field object flags).
pub const EDIT_ITEM: u32 = 0x8;

/// FileMaker errors a choice answers with.
mod code {
    /// Command is unavailable.
    pub const UNAVAILABLE: i32 = 3;
    /// Record is missing.
    pub const RECORD_MISSING: i32 = 101;
    /// Field cannot be modified.
    pub const FIELD_LOCKED: i32 = 201;
    /// Value in field is not a member of the value list.
    pub const NOT_IN_LIST: i32 = 506;
    /// Insufficient privileges.
    pub const INSUFFICIENT_PRIVILEGES: i32 = 9;
}

/// A field control on a layout, and the record whose field a choice in it
/// changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChoiceTarget {
    /// The layout's id.
    pub layout: u64,
    /// The field object's key on the layout.
    pub object: u64,
    /// The record's id.
    pub record: u64,
}

/// Why a control refuses a choice before anything changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A control in a portal row: related records are not edited here.
    Portal,
    /// A field of another occurrence than the layout's (a related field).
    Related,
    /// A control showing several repetitions, or another repetition than
    /// the first.
    Repetitions,
    /// The layout does not let the user enter the field in Browse mode
    /// (layout-model.md 5.4, field entry).
    Entry,
    /// This connection may not change the field.
    ReadOnly,
}

impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Refusal::Portal => "OpenMaker does not support value-list controls in portals.",
            Refusal::Related => {
                "OpenMaker supports value-list controls of the layout's own table only."
            }
            Refusal::Repetitions => {
                "OpenMaker does not support value-list controls showing several repetitions."
            }
            Refusal::Entry => "The layout does not allow changing this field in Browse mode.",
            Refusal::ReadOnly => "This field cannot be changed on this connection.",
        }
    }
}

/// What a caller lacks for an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// A desktop window: the field's focus, the record's draft and the
    /// script triggers a choice runs. `om-author` has none.
    NoWindow,
    /// The calculation host the items are read through.
    NoCalcHost,
}

impl Unavailable {
    /// The name `om-author` reports.
    pub fn name(self) -> &'static str {
        match self {
            Unavailable::NoWindow => "no_window",
            Unavailable::NoCalcHost => "no_calc_host",
        }
    }
}

/// Why a control's items cannot be listed, or a value cannot be chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChoiceError {
    /// The caller lacks what the operation needs (error 3).
    Unavailable(Unavailable),
    /// The layout or its field object does not exist (error 3).
    NoControl,
    /// The object is not a field control with a value list this file
    /// defines.
    NoValueList,
    /// Choosing an item replaces the value in a pop-up menu or a drop-down
    /// list only; sets change their values their own way
    /// (layout-model.md 5.4).
    NotAMenuOrList,
    /// A list OpenMaker cannot read: another file's (type 2) or a
    /// calculated one (type 3; file-metadata.md 6.2). Error 3.
    ListUnsupported(&'static str),
    /// The data the items come from is still being read: the desktop
    /// waits, and nothing is offered meanwhile. Not an error to report.
    Pending,
    /// More than [`MAX_CHOICES`] items (error 3).
    TooLarge,
    /// The control refuses a choice (error 201).
    Refused(Refusal),
    /// The control and record are not the ones the choice was made on.
    /// Nothing changes, on that record or another (error 101).
    TargetGone,
    /// The value is not one of the list's items (error 506).
    NotAnItem,
    /// The control offers no such command (error 3).
    NoCommand(MenuCommand),
    /// Reading the items failed with this FileMaker error.
    Failed(i32),
    /// A single checkbox's field holds a value other than its list's first
    /// and second values ([`single_checkbox`]): it is not replaced
    /// (error 3).
    OtherValues,
}

impl ChoiceError {
    /// The FileMaker error, or `None` while the items are being read.
    pub fn code(&self) -> Option<i32> {
        Some(match self {
            ChoiceError::Pending => return None,
            ChoiceError::Unavailable(_)
            | ChoiceError::NoControl
            | ChoiceError::NoValueList
            | ChoiceError::NotAMenuOrList
            | ChoiceError::ListUnsupported(_)
            | ChoiceError::TooLarge
            | ChoiceError::NoCommand(_)
            | ChoiceError::OtherValues => code::UNAVAILABLE,
            ChoiceError::Refused(_) => code::FIELD_LOCKED,
            ChoiceError::TargetGone => code::RECORD_MISSING,
            ChoiceError::NotAnItem => code::NOT_IN_LIST,
            ChoiceError::Failed(code) => *code,
        })
    }

    pub fn message(&self) -> String {
        match self {
            ChoiceError::Unavailable(Unavailable::NoWindow) => {
                "Choosing a value needs a desktop window: its field focus, the record's draft and \
                 its script triggers."
                    .into()
            }
            ChoiceError::Unavailable(Unavailable::NoCalcHost) => {
                "The value list's items cannot be read: no calculation host for this file.".into()
            }
            ChoiceError::NoControl => "The layout has no such field control.".into(),
            ChoiceError::NoValueList => "This field control has no value list of this file.".into(),
            ChoiceError::NotAMenuOrList => {
                "Only a pop-up menu or a drop-down list chooses one item this way.".into()
            }
            ChoiceError::ListUnsupported(what) => format!("OpenMaker cannot read {what}."),
            ChoiceError::Pending => "The value list's items are being read.".into(),
            ChoiceError::TooLarge => format!(
                "The value list has more than {MAX_CHOICES} items, the most a control offers here."
            ),
            ChoiceError::Refused(refusal) => refusal.message().into(),
            ChoiceError::TargetGone => "The record or the control is no longer there.".into(),
            ChoiceError::NotAnItem => "The value is not an item of the value list.".into(),
            ChoiceError::OtherValues => {
                "The field holds a value other than its checkbox's two. OpenMaker does not \
                 replace it."
                    .into()
            }
            ChoiceError::NoCommand(command) => {
                format!("This field control offers no “{}” item.", command.label())
            }
            ChoiceError::Failed(code) => {
                format!("The value list's items could not be read (error {code}).")
            }
        }
    }
}

/// A control's items, in `ValueListItems` order (file-metadata.md 6.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices {
    pub items: Vec<ValueListEntry>,
    /// The items are custom values, where `-` alone is a divider
    /// (file-metadata.md 6.1).
    pub custom: bool,
}

/// A drop-down list's auto-complete (#870): the first of `choices`' items,
/// in the list's order, whose value begins with `typed`, ignoring case, and
/// is longer; none for an empty `typed`. FileMaker Pro 26.0.3 shows "Acme"
/// for a typed "A" (layout-model.md 5.4); that case is ignored, and that
/// the list's first match wins, are inferred.
pub fn completion<'c>(choices: &'c Choices, typed: &str) -> Option<&'c ValueListEntry> {
    let typed: Vec<char> = typed.chars().collect();
    if typed.is_empty() {
        return None;
    }
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    choices.items.iter().find(|item| {
        !choices.is_divider(item) && {
            let mut chars = item.value.chars();
            typed
                .iter()
                .all(|&t| chars.next().is_some_and(|c| same(c, t)))
                && chars.next().is_some()
        }
    })
}

impl Choices {
    /// Whether `item` is a divider: a menu draws it as a line and offers
    /// no choice (layout-model.md 5.4, inferred).
    pub fn is_divider(&self, item: &ValueListEntry) -> bool {
        self.custom && item.value == "-"
    }
}

/// The edit a choice makes: the field's whole new value, on `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceEdit {
    pub target: ChoiceTarget,
    pub value: String,
}

/// The value list field control `object` shows: its `valueListKey`, read
/// as a serial catalog key like a field spec's field key
/// (layout-model.md 5.4).
pub fn value_list<'s>(
    schema: &'s CalcSchema,
    object: &LayoutObject,
) -> Result<&'s ValueList, ChoiceError> {
    let key = object
        .field_obj
        .as_ref()
        .map(|field| field.value_list_key)
        .filter(|key| *key != 0)
        .ok_or(ChoiceError::NoValueList)?;
    let id = FieldSpec {
        field_key: key,
        ..FieldSpec::default()
    }
    .field_id()
    .ok_or(ChoiceError::NoValueList)?;
    let list = schema
        .value_lists()
        .iter()
        .find(|list| list.id == id)
        .ok_or(ChoiceError::NoValueList)?;
    // A list the account's custom set gives no access offers nothing
    // (session-identity.md 2.2).
    if !schema.shows_value_list(list.id) {
        return Err(ChoiceError::Failed(code::INSUFFICIENT_PRIVILEGES));
    }
    Ok(list)
}

/// Whether OpenMaker reads and changes `object`'s value list here, a
/// control of a layout whose occurrence key is `layout_alias`
/// (`table_alias_key`), shown in a portal row or not: refused in a portal,
/// for a related field and for a control showing several repetitions,
/// rather than guessing which record or repetition a choice belongs to.
pub fn supported(
    object: &LayoutObject,
    layout_alias: u64,
    in_portal: bool,
) -> Result<(), ChoiceError> {
    let field = object.field_obj.as_ref().ok_or(ChoiceError::NoControl)?;
    if in_portal {
        return Err(ChoiceError::Refused(Refusal::Portal));
    }
    let spec = object
        .field_spec()
        .filter(|spec| spec.table_key == layout_alias)
        .ok_or(ChoiceError::Refused(Refusal::Related))?;
    if spec.repetition > 1 || field.num_of_reps.unsigned_abs() > 1 {
        return Err(ChoiceError::Refused(Refusal::Repetitions));
    }
    Ok(())
}

/// Whether the user may choose in `object` ([`supported`]), by a
/// connection that may change its field or not, where the layout lets the
/// field be entered in Browse mode. Checked before anything changes.
pub fn availability(
    object: &LayoutObject,
    layout_alias: u64,
    in_portal: bool,
    may_change: bool,
) -> Result<(), ChoiceError> {
    supported(object, layout_alias, in_portal)?;
    let field = object.field_obj.as_ref().ok_or(ChoiceError::NoControl)?;
    if field.field_entry(false) != FieldEntry::Edit {
        return Err(ChoiceError::Refused(Refusal::Entry));
    }
    if !may_change {
        return Err(ChoiceError::Refused(Refusal::ReadOnly));
    }
    Ok(())
}

/// Whether the user may choose in `object` where it shows a related
/// record's field (a portal row's, or a related field's first record) that
/// the related table's own rules let the user change (`may_change`): as
/// [`availability`], without its refusal of a portal or related field.
/// The desktop writes the choice to the related record, committed with
/// the record the layout shows.
pub fn related_availability(object: &LayoutObject, may_change: bool) -> Result<(), ChoiceError> {
    let field = object.field_obj.as_ref().ok_or(ChoiceError::NoControl)?;
    let repetition = object.field_spec().map_or(1, |spec| spec.repetition);
    if repetition > 1 || field.num_of_reps.unsigned_abs() > 1 {
        return Err(ChoiceError::Refused(Refusal::Repetitions));
    }
    if field.field_entry(false) != FieldEntry::Edit {
        return Err(ChoiceError::Refused(Refusal::Entry));
    }
    if !may_change {
        return Err(ChoiceError::Refused(Refusal::ReadOnly));
    }
    Ok(())
}

/// The context `list`'s items are read in for record `record` of
/// occurrence `occurrence`. Related values start from the current record
/// of the list's start occurrence: a portal row's own record (`portal`:
/// its occurrence and record) when it is in that table.
pub fn context(
    schema: &CalcSchema,
    list: &ValueList,
    occurrence: u64,
    record: u64,
    portal: Option<(u64, u64)>,
) -> Context {
    let start = match &list.source {
        ValueSource::Field { start, .. } => *start,
        _ => None,
    };
    let table = |occurrence| schema.table_of(occurrence).map(|table| table.id);
    match portal {
        Some((row_occurrence, row_record))
            if start.is_some_and(|start| table(start) == table(row_occurrence)) =>
        {
            Context::record(row_occurrence, row_record)
        }
        _ => Context::record(occurrence, record),
    }
}

/// Every item `list` offers in `context`, or an error: never an empty or
/// partial list in place of one OpenMaker cannot read.
pub fn choices(
    file: &FileCalc<'_>,
    list: &ValueList,
    context: &Context,
) -> Result<Choices, ChoiceError> {
    match &list.source {
        ValueSource::External { .. } => {
            return Err(ChoiceError::ListUnsupported("another file's value list"))
        }
        ValueSource::Calculation(_) => {
            return Err(ChoiceError::ListUnsupported("a calculated value list"))
        }
        ValueSource::Custom(_) | ValueSource::Field { .. } => {}
    }
    let items = file
        .value_list_entries(list, context, MAX_CHOICES)
        .map_err(|error| ChoiceError::Failed(error.code()))?
        .ok_or(ChoiceError::TooLarge)?;
    Ok(Choices {
        items,
        custom: matches!(list.source, ValueSource::Custom(_)),
    })
}

/// The edit choosing the item that stores `value` in pop-up menu or
/// drop-down list `object` makes, for a choice made on `made_on` and
/// applied on `at`: the field's whole value becomes the item's stored value
/// (layout-model.md 5.4; for a drop-down list inferred, also when the field
/// holds typed text). A choice applies only where it was made: a record or
/// control that changed meanwhile is [`ChoiceError::TargetGone`].
pub fn choose(
    object: &LayoutObject,
    made_on: ChoiceTarget,
    at: ChoiceTarget,
    choices: &Choices,
    value: &str,
) -> Result<ChoiceEdit, ChoiceError> {
    if object
        .field_obj
        .as_ref()
        .is_none_or(|field| !matches!(field.display_type, DROP_DOWN_LIST | POP_UP_MENU))
    {
        return Err(ChoiceError::NotAMenuOrList);
    }
    if made_on != at {
        return Err(ChoiceError::TargetGone);
    }
    let item = choices
        .items
        .iter()
        .find(|item| item.value == value && !choices.is_divider(item))
        .ok_or(ChoiceError::NotAnItem)?;
    Ok(ChoiceEdit {
        target: at,
        value: item.value.clone(),
    })
}

/// The value a click gives a checkbox set shown as one checkbox without a
/// label ("Display single checkbox without value", FileMaker Pro 26.1;
/// layout-model.md 5.4.1), holding `value`. Checking it stores the list's
/// first value; clearing it stores the second, or empties the field when
/// the list has one value (Claris, FileMaker Pro 26.1.1 release notes).
/// So it toggles a field holding exactly the first value, the second, or
/// nothing. A field holding anything else (another value, or other lines
/// with the first) is refused, [`ChoiceError::OtherValues`], its data
/// kept: what FileMaker Pro writes over such a value is not documented or
/// measured. `Ok(None)`: the list has no item, or the value stays.
pub fn single_checkbox(choices: &Choices, value: &str) -> Result<Option<String>, ChoiceError> {
    let mut items = choices
        .items
        .iter()
        .filter(|item| !choices.is_divider(item) && !item.value.is_empty());
    let Some(first) = items.next() else {
        return Ok(None);
    };
    let second = items.next().map_or("", |item| item.value.as_str());
    let changed = if value == first.value {
        second
    } else if value.is_empty() || value == second {
        first.value.as_str()
    } else {
        return Err(ChoiceError::OtherValues);
    };
    Ok((changed != value).then(|| changed.to_owned()))
}

/// Whether `value` checks the single checkbox of `choices`
/// ([`single_checkbox`]): it is the list's first value, or holds it among
/// its lines, as a set's item is checked. That the lines count is
/// inferred.
pub fn single_checked(choices: &Choices, value: &str) -> bool {
    choices
        .items
        .iter()
        .find(|item| !choices.is_divider(item) && !item.value.is_empty())
        .is_some_and(|first| {
            value == first.value || value.split(['\r', '\n']).any(|line| line == first.value)
        })
}

/// Find mode's criterion after a click on a single checkbox holding
/// `criterion`: the list's first value, or no criterion when the box is
/// checked [I: Find mode's use of the checkbox is unmeasured]. `None`: the
/// list has no item, or the criterion stays.
pub fn single_checkbox_criterion(choices: &Choices, criterion: &str) -> Option<String> {
    let first = choices
        .items
        .iter()
        .find(|item| !choices.is_divider(item) && !item.value.is_empty())?;
    let changed = match single_checked(choices, criterion) {
        true => String::new(),
        false => first.value.clone(),
    };
    (changed != criterion).then_some(changed)
}

/// An item a menu offers after its value list's items: a command, never a
/// value. It is not one of [`choices`]' items, so [`choose`], a value
/// list's completion and a find criterion never meet it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuCommand {
    /// “Other…”: a dialog asks for the value to enter.
    Other,
    /// “Edit…”: a dialog edits the value list's custom values.
    Edit,
}

impl MenuCommand {
    pub fn label(self) -> &'static str {
        match self {
            MenuCommand::Other => "Other…",
            MenuCommand::Edit => "Edit…",
        }
    }
}

/// The commands field control `object` offers after its items, in the
/// order FileMaker Pro 26.0.3 shows them (#869): “Other…” last of a pop-up
/// menu's list, “Edit…” directly after it, with no divider of their own (a
/// divider before them is the list's last stored `-`). A drop-down list
/// offers neither: Pro showed no “Edit…” in Assets' Type list with its
/// Edit flag set, empty or holding values (#869). Checkbox and radio
/// button sets offer neither here: what Pro shows for them is unmeasured.
pub fn menu_commands(object: &LayoutObject) -> Vec<MenuCommand> {
    let Some(field) = object.field_obj.as_ref() else {
        return Vec::new();
    };
    let pop_up = field.display_type == POP_UP_MENU;
    let other = pop_up && field.flags & OTHER_ITEM != 0;
    let edit = pop_up && field.flags & EDIT_ITEM != 0;
    [(other, MenuCommand::Other), (edit, MenuCommand::Edit)]
        .into_iter()
        .filter_map(|(offered, command)| offered.then_some(command))
        .collect()
}

/// Whether “Other…” shows checked with the field holding `value`: a value
/// no item of `choices` stores, as FileMaker Pro 26.0.3 checks it after
/// Other… entered one (#869). An empty field checks nothing.
pub fn other_checked(choices: &Choices, value: &str) -> bool {
    !value.is_empty()
        && !choices
            .items
            .iter()
            .any(|item| item.value == value && !choices.is_divider(item))
}

/// The edit “Other…” makes in pop-up menu `object`, chosen on `made_on`
/// and applied on `at`: `text`, as entered in its dialog, becomes the
/// field's whole value, as typing it would (the record's draft, saved with
/// the record, which validates it). FileMaker Pro 26.0.3 keeps the text
/// and does not add it to the list (#869). A choice applies only where it
/// was made, as [`choose`]'s.
pub fn other_value(
    object: &LayoutObject,
    made_on: ChoiceTarget,
    at: ChoiceTarget,
    text: &str,
) -> Result<ChoiceEdit, ChoiceError> {
    if !menu_commands(object).contains(&MenuCommand::Other) {
        return Err(ChoiceError::NoCommand(MenuCommand::Other));
    }
    if made_on != at {
        return Err(ChoiceError::TargetGone);
    }
    Ok(ChoiceEdit {
        target: at,
        value: text.to_owned(),
    })
}

/// “Edit…” in pop-up menu `object`: the value list it opens in Edit
/// Value List, by the id the catalog readers use (#869). FileMaker Pro
/// 26.0.3 commits the record first; the list's values are then read
/// (`DataProvider::custom_value_list`) and written
/// ([`crate::value_lists::plan_values`]), a list of a field's values
/// being refused there.
pub fn edit_value_list(object: &LayoutObject) -> Result<u64, ChoiceError> {
    if !menu_commands(object).contains(&MenuCommand::Edit) {
        return Err(ChoiceError::NoCommand(MenuCommand::Edit));
    }
    let key = object
        .field_obj
        .as_ref()
        .map(|field| field.value_list_key)
        .filter(|key| *key != 0)
        .ok_or(ChoiceError::NoValueList)?;
    FieldSpec {
        field_key: key,
        ..FieldSpec::default()
    }
    .field_id()
    .ok_or(ChoiceError::NoValueList)
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_layout::FieldObject;

    fn control(display_type: u32, flags: u32) -> LayoutObject {
        LayoutObject {
            field_obj: Some(FieldObject {
                display_type,
                flags,
                ..FieldObject::default()
            }),
            ..LayoutObject::default()
        }
    }

    fn item(value: &str) -> ValueListEntry {
        ValueListEntry {
            value: value.into(),
            label: value.into(),
        }
    }

    #[test]
    fn a_single_checkbox_stores_the_first_value_and_clears_to_the_second() {
        let two = Choices {
            items: ["Yes", "No"].map(item).to_vec(),
            custom: true,
        };
        assert_eq!(single_checkbox(&two, ""), Ok(Some("Yes".into())));
        assert_eq!(single_checkbox(&two, "No"), Ok(Some("Yes".into())));
        assert_eq!(single_checkbox(&two, "Yes"), Ok(Some("No".into())));
        assert!(single_checked(&two, "Yes") && !single_checked(&two, "No"));
        // Other data is kept, not replaced: other lines with the first
        // value (drawn checked), or a value that is neither.
        for other in ["Yes\rNotes", "Notes"] {
            assert_eq!(single_checkbox(&two, other), Err(ChoiceError::OtherValues));
        }
        assert!(single_checked(&two, "Yes\rNotes") && !single_checked(&two, "Notes"));
        // One value: clearing empties the field.
        let one = Choices {
            items: vec![item("Done")],
            custom: true,
        };
        assert_eq!(single_checkbox(&one, "Done"), Ok(Some(String::new())));
        assert_eq!(single_checkbox(&one, ""), Ok(Some("Done".into())));
        // A first item of several lines (a second field's list) is checked
        // by itself, and cleared.
        let lines = Choices {
            items: ["A\rB", "C"].map(item).to_vec(),
            custom: false,
        };
        assert!(single_checked(&lines, "A\rB"));
        assert_eq!(single_checkbox(&lines, "A\rB"), Ok(Some("C".into())));
        // A divider is no value; no item, no change.
        let divided = Choices {
            items: ["-", "On", "-", "Off"].map(item).to_vec(),
            custom: true,
        };
        assert_eq!(single_checkbox(&divided, "On"), Ok(Some("Off".into())));
        let none = Choices {
            items: Vec::new(),
            custom: true,
        };
        assert_eq!(single_checkbox(&none, "x"), Ok(None));
        // Find mode: the first value, or no criterion.
        assert_eq!(single_checkbox_criterion(&two, ""), Some("Yes".into()));
        assert_eq!(single_checkbox_criterion(&two, "Yes"), Some(String::new()));
        assert_eq!(single_checkbox_criterion(&none, ""), None);
    }

    #[test]
    fn other_and_edit_follow_the_values_and_are_never_values() {
        use MenuCommand::{Edit, Other};
        // The starters' flags: Contacts' Title (Other), Content
        // Management's Type (Other and Edit), Assets' Type list (Edit).
        assert_eq!(menu_commands(&control(POP_UP_MENU, 0x1)), [Other]);
        assert_eq!(menu_commands(&control(POP_UP_MENU, 0x9)), [Other, Edit]);
        assert!(menu_commands(&control(DROP_DOWN_LIST, 0x9)).is_empty());
        assert!(menu_commands(&control(3, 0x9)).is_empty());
        let title = Choices {
            items: ["Mr.", "Mrs.", "-", "Prof.", "-"].map(item).to_vec(),
            custom: true,
        };
        assert!(other_checked(&title, "Dr"));
        assert!(!other_checked(&title, "Mr."));
        assert!(!other_checked(&title, ""));
        assert!(other_checked(&title, "-"), "a divider stores no value");
        // A command's label is no item to choose.
        let at = ChoiceTarget {
            layout: 1,
            object: 2,
            record: 3,
        };
        let menu = control(POP_UP_MENU, 0x9);
        assert_eq!(
            choose(&menu, at, at, &title, "Other…"),
            Err(ChoiceError::NotAnItem)
        );
        assert_eq!(
            other_value(&menu, at, at, "Dr").map(|edit| edit.value),
            Ok("Dr".into())
        );
        let elsewhere = ChoiceTarget { record: 4, ..at };
        assert_eq!(
            other_value(&menu, at, elsewhere, "Dr"),
            Err(ChoiceError::TargetGone)
        );
        assert_eq!(
            other_value(&control(POP_UP_MENU, 0x8), at, at, "Dr"),
            Err(ChoiceError::NoCommand(Other))
        );
        assert_eq!(
            edit_value_list(&menu),
            Err(ChoiceError::NoValueList),
            "a menu without a list opens none"
        );
        let mut listed = control(POP_UP_MENU, 0x8);
        listed.field_obj.as_mut().unwrap().value_list_key = 1;
        assert_eq!(edit_value_list(&listed), Ok(1));
        assert_eq!(
            edit_value_list(&control(POP_UP_MENU, 0x1)),
            Err(ChoiceError::NoCommand(Edit))
        );
    }
}
