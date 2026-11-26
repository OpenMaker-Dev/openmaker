//! Trust (criterion 8, its OpenMaker half): whether records written
//! through the provider calls the desktop window makes survive closing and
//! reopening the file as written. On a fresh copy of each file, every base
//! table is reached as Browse reaches it:
//!
//! - through a layout of its own: New Record (`create_record` with no
//!   values), an edit committed (`update_record`), Duplicate Record
//!   (`create_record` with the record's fields Browse may change) and
//!   Delete Record (`delete_record`);
//! - otherwise through a portal showing it on its parent's layout: typing
//!   into the creation row (`write_record` of a new related record with the
//!   relationship's match fields from the parent record, as the window's
//!   commit writes it), an edit committed in the row (`write_record`), and
//!   a portal row deleted (`delete_related_record`). Duplicate Record
//!   duplicates the layout's record, not a portal row's, so this route has
//!   no duplicate.
//!
//! The field edited is one the layout or portal shows in a control the
//! user types into (not an option set), with a different value at each
//! stage. First a baseline record in every table, created and edited;
//! then in every table a record created and edited, and a record deleted
//! (the original after duplicating it, or a second portal row); then the
//! file is closed and opened again. A table passes when every call
//! succeeds and, after reopening, it holds exactly the records it held
//! before (unchanged) and those left, each with every stored value as read
//! before closing (`calc_data`, all fields), the edited field as entered. A panic is caught and reported with its
//! table. The written copies are left where the caller asks, for
//! FileMaker's consistency check and FileMaker Pro, which this does not
//! run.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use om_data::calc::{Ctx, Date, ValueType};
use om_data::host::{CalcNeed, CalcSchema, FieldClass, FieldDef, RecordValues, TableDef};
use om_data::meta::{relation_option, JoinOp};
use om_data::{DataProvider, FieldChanges, RecordCapabilities, ScriptWrite};
use om_layout::ObjectKind;
use serde::Serialize;

#[derive(Serialize, Default)]
pub(super) struct TrustCard {
    pub tables: Vec<TrustTable>,
    /// The written copy, where it was left.
    pub written: Option<PathBuf>,
    /// Why the measure stopped short, if it did.
    pub error: Option<String>,
    /// How long reopening waited for the closed file to be released, when
    /// it had to: the local host keeps a file in use until its last task
    /// ends (`om-provider`'s `local.rs`).
    pub reopen_wait_ms: Option<u64>,
}

#[derive(Serialize)]
pub(super) struct TrustTable {
    pub table: String,
    /// How Browse reaches it: its layout, or a portal on a layout.
    pub route: String,
    pub passed: bool,
    /// The first error, panic or difference.
    pub error: Option<String>,
}

enum Route {
    /// The table's own layout.
    Layout,
    /// A portal on the layout of `parent` (an index into the targets);
    /// each match field pairs the parent's field with this table's.
    Portal {
        parent: usize,
        matches: Vec<(u64, u64)>,
    },
}

struct Target {
    table: String,
    table_id: u64,
    /// The layout written through: the table's own, or the parent's.
    layout: u32,
    route_name: String,
    route: Route,
    /// The field edited: its id, name and type.
    field: Option<(u64, String, ValueType)>,
    capabilities: RecordCapabilities,
    baseline: Option<i64>,
    /// The record left from step 2, and the record deleted.
    kept: Option<i64>,
    deleted: Option<i64>,
    /// Each record's stored values (their debug form): those the table
    /// held before anything was written, and all, read before closing.
    existing: BTreeMap<u64, String>,
    before: BTreeMap<u64, String>,
    error: Option<String>,
}

impl Target {
    fn new(table: &TableDef, layout: u32, route_name: String, route: Route) -> Target {
        Target {
            table: table.name.clone(),
            table_id: table.id,
            layout,
            route_name,
            route,
            field: None,
            capabilities: RecordCapabilities::default(),
            baseline: None,
            kept: None,
            deleted: None,
            existing: BTreeMap::new(),
            before: BTreeMap::new(),
            error: None,
        }
    }

    fn portal(&self) -> bool {
        matches!(self.route, Route::Portal { .. })
    }
}

/// The stages the field is written in: the baseline record, a new
/// record, its edit, a second portal row.
const BASELINE: usize = 0;
const CREATED: usize = 1;
const EDITED: usize = 2;
const SECOND: usize = 3;
const STAGES: [&str; 4] = ["baseline", "created", "edited", "second"];

/// What is entered into a field of type `kind` of `table` at `stage`:
/// different at each stage, so that an edit that is lost shows.
fn entered(kind: ValueType, table: &str, stage: usize) -> String {
    let n = stage as u32;
    match kind {
        ValueType::Number => format!("{}", 41 + n),
        ValueType::Date => format!("11/{}/2026", 13 + n),
        ValueType::Time => format!("10:3{n}:00"),
        ValueType::Timestamp => format!("11/{}/2026 10:3{n}:00", 13 + n),
        _ => format!("Trust {} {table}", STAGES[stage]),
    }
}

/// A field object the user types into in Browse: no checkbox set, radio
/// button set or pop-up menu (which offer their list's values instead),
/// and entry allowed.
fn typing_control(object: &om_layout::LayoutObject) -> bool {
    object.field_obj.as_ref().is_some_and(|f| {
        !matches!(f.display_type, 2..=4) && f.field_entry(false) == om_layout::FieldEntry::Edit
    })
}

/// A field the user may type into: stored data, not global, container or
/// repeating.
fn typed_field(def: &FieldDef) -> bool {
    def.class == FieldClass::Data
        && !def.global
        && def.value_type != ValueType::Container
        && def.repetitions <= 1
}

/// A text field among `fields`, else the first.
fn pick(fields: Vec<(u64, String, ValueType)>) -> Option<(u64, String, ValueType)> {
    let text = fields.iter().position(|f| f.2 == ValueType::Text);
    fields.into_iter().nth(text.unwrap_or(0))
}

fn panic_text(panic: Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "a panic".into())
}

/// `f`'s result, a panic in it as an error.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f))
        .map_err(|p| format!("panicked: {}", panic_text(p)))
        .and_then(|r| r)
}

/// Every record of `table` with its stored values, as the window's
/// calculations read them.
fn records(
    provider: &mut dyn DataProvider,
    table: u64,
) -> Result<BTreeMap<u64, RecordValues>, String> {
    let data = provider
        .calc_data(&[CalcNeed::Records { table }])
        .map_err(|e| e.message)?;
    let ids: Vec<u64> = data
        .tables
        .get(&table)
        .map(|ids| ids.to_vec())
        .unwrap_or_default();
    let needs: Vec<CalcNeed> = ids
        .iter()
        .map(|&record| CalcNeed::Record { table, record })
        .collect();
    let data = provider.calc_data(&needs).map_err(|e| e.message)?;
    ids.into_iter()
        .map(|id| {
            let values = data
                .records
                .get(&(table, id))
                .cloned()
                .flatten()
                .ok_or_else(|| format!("record {id} does not read"))?;
            Ok((id, (*values).clone()))
        })
        .collect()
}

fn text(schema: &CalcSchema, values: &RecordValues, field: u64) -> String {
    let cx = Ctx::new(
        &schema.locale,
        Date::from_ymd(2026, 11, 13).expect("a date"),
    );
    values
        .get(field, 1)
        .map(|v| v.to_text(&cx).as_str().to_owned())
        .unwrap_or_default()
}

/// The table's own layout, the field edited there, and what Browse may do.
fn layout_target(
    provider: &mut dyn DataProvider,
    table: &TableDef,
    (layout, occurrence): (u32, u64),
    name: &str,
) -> Target {
    let mut t = Target::new(table, layout, format!("layout {name}"), Route::Layout);
    let read = guarded(|| {
        let capabilities = provider
            .record_capabilities(layout)
            .map_err(|e| e.message)?;
        // The table's fields the layout shows outside portals in a control
        // the user types into.
        let render = provider
            .render_layout(layout)
            .map_err(|e| e.message)?
            .ok_or("the layout does not render")?;
        let shown: BTreeSet<u64> = render
            .layout
            .objects
            .objects
            .iter()
            .filter(|o| {
                typing_control(o)
                    && crate::document::portals::enclosing(&render.layout, o.id).is_none()
            })
            .filter_map(|o| {
                let spec = o.field_spec()?;
                (om_layout::occurrence_id(spec.table_key)? == occurrence && spec.repetition <= 1)
                    .then_some(spec.field_id()?)
            })
            .collect();
        let page = provider.records(layout, 0, 1).map_err(|e| e.message)?;
        let fields = page
            .fields
            .iter()
            .filter(|f| capabilities.can_edit(f))
            .filter_map(|f| {
                let def = table.fields.values().find(|d| d.name == f.name)?;
                (typed_field(def) && shown.contains(&def.id))
                    .then(|| (def.id, f.name.clone(), def.value_type))
            })
            .collect();
        if !capabilities.create || !capabilities.delete {
            return Err("the provider does not let Browse create and delete records".into());
        }
        let field = pick(fields).ok_or("no field on the layout takes typing")?;
        Ok((capabilities, Some(field)))
    });
    match read {
        Ok((capabilities, field)) => {
            t.capabilities = capabilities;
            t.field = field;
        }
        Err(e) => t.error = Some(format!("reading the layout: {e}")),
    }
    t
}

/// A portal of `table` on a layout of one of `targets`' tables, through a
/// direct relationship that allows creating its records: its route and
/// the portal's field of the table the user may type into.
fn portal_target(
    provider: &mut dyn DataProvider,
    schema: &CalcSchema,
    targets: &[Target],
    table: &TableDef,
) -> Option<Target> {
    for entry in schema.layouts() {
        let Some(own) = entry.occurrence else {
            continue;
        };
        let Some(parent) = schema.table_of(own).and_then(|t| {
            targets
                .iter()
                .position(|p| p.table_id == t.id && !p.portal())
        }) else {
            continue;
        };
        let Ok(Some(render)) = provider.render_layout(entry.id) else {
            continue;
        };
        let layout = &render.layout;
        let capabilities = provider.record_capabilities(entry.id).unwrap_or_default();
        for portal in layout
            .objects
            .objects
            .iter()
            .filter(|o| o.kind() == ObjectKind::Portal)
        {
            let Some(occurrence) = portal
                .portal
                .as_ref()
                .and_then(|p| om_layout::occurrence_id(p.table_alias_key))
            else {
                continue;
            };
            if schema.table_of(occurrence).map(|t| t.id) != Some(table.id) {
                continue;
            }
            let Some(path) = schema.path(own, occurrence) else {
                continue;
            };
            let [hop] = path.as_slice() else {
                continue;
            };
            let creates = schema.relationships().iter().any(|r| {
                (r.left == hop.from
                    && r.right == hop.to
                    && r.options & relation_option::CREATE_LEFT_TO_RIGHT != 0)
                    || (r.right == hop.from
                        && r.left == hop.to
                        && r.options & relation_option::CREATE_RIGHT_TO_LEFT != 0)
            });
            if !creates {
                continue;
            }
            let matches: Vec<(u64, u64)> = hop
                .predicates
                .iter()
                .filter(|p| p.op == JoinOp::Equal)
                .map(|p| (p.left, p.right))
                .collect();
            let fields = layout
                .objects
                .objects
                .iter()
                .filter(|o| {
                    typing_control(o)
                        && crate::document::portals::enclosing(layout, o.id) == Some(portal.id)
                })
                .filter_map(|o| {
                    let spec = o.field_spec()?;
                    if om_layout::occurrence_id(spec.table_key)? != occurrence {
                        return None;
                    }
                    let def = table.fields.get(&spec.field_id()?)?;
                    (typed_field(def)
                        && !matches.iter().any(|m| m.1 == def.id)
                        && capabilities.can_edit_related(occurrence, def.id))
                    .then(|| (def.id, def.name.clone(), def.value_type))
                })
                .collect();
            let mut t = Target::new(
                table,
                entry.id,
                format!("portal on layout {}", entry.name),
                Route::Portal { parent, matches },
            );
            t.field = pick(fields);
            if t.field.is_none() {
                t.error = Some("no field of the portal takes typing".into());
            }
            return Some(t);
        }
    }
    None
}

/// How Browse reaches each base table: tables with a layout of their own
/// first, as portals need their parents' records.
fn targets(
    provider: &mut dyn DataProvider,
    schema: &CalcSchema,
    card: &mut TrustCard,
) -> Vec<Target> {
    let mut out = Vec::new();
    let mut unreached = Vec::new();
    for table in schema.tables() {
        let layout = schema.layouts().iter().find(|l| {
            l.occurrence
                .and_then(|o| schema.table_of(o))
                .is_some_and(|t| t.id == table.id)
        });
        match layout {
            Some(l) => out.push(layout_target(
                provider,
                table,
                (l.id, l.occurrence.unwrap_or_default()),
                &l.name,
            )),
            None => unreached.push(table),
        }
    }
    for table in unreached {
        match portal_target(provider, schema, &out, table) {
            Some(t) => out.push(t),
            None => card.tables.push(TrustTable {
                table: table.name.clone(),
                route: "-".into(),
                passed: false,
                error: Some(
                    "no layout or creating portal shows the table: Browse cannot reach it".into(),
                ),
            }),
        }
    }
    out
}

/// A new record with its field set as at `stage`, as Browse makes one: New
/// Record then the edit committed, or typing into the creation row.
fn create(
    provider: &mut dyn DataProvider,
    schema: &CalcSchema,
    targets: &[Target],
    t: &Target,
    stage: usize,
) -> Result<i64, String> {
    match &t.route {
        Route::Layout => {
            let record = provider
                .create_record(t.layout, FieldChanges::new())
                .map_err(|e| format!("New Record: {}", e.message))?;
            edit(provider, t, record.id, stage)?;
            Ok(record.id)
        }
        Route::Portal { parent, matches } => {
            let parent = &targets[*parent];
            let key = parent.baseline.ok_or("the parent record was not made")?;
            let values = records(provider, parent.table_id)?
                .remove(&(key as u64))
                .ok_or("the parent record does not read")?;
            let mut cells: BTreeMap<u64, Option<String>> = matches
                .iter()
                .map(|&(left, right)| {
                    let key = self::text(schema, &values, left);
                    (right, (!key.is_empty()).then_some(key))
                })
                .collect();
            let mut typed = BTreeSet::new();
            if let Some((id, _, kind)) = &t.field {
                cells.insert(*id, Some(entered(*kind, &t.table, stage)));
                typed.insert(*id);
            }
            let record = provider
                .write_record(&ScriptWrite {
                    layout: t.layout,
                    table: Some(t.table_id),
                    record: None,
                    cells,
                    fields: FieldChanges::new(),
                    typed,
                    skip_validation: false,
                    privilege_set: None,
                    record_number: None,
                    prepared: None,
                    single_field_step: false,
                })
                .map_err(|e| format!("typing into the creation row: {}", e.message))?;
            Ok(record.id)
        }
    }
}

/// The field of `record` set as at `stage` and committed.
fn edit(
    provider: &mut dyn DataProvider,
    t: &Target,
    record: i64,
    stage: usize,
) -> Result<(), String> {
    let (id, name, kind) = t.field.as_ref().ok_or("no field to edit")?;
    let entered = entered(*kind, &t.table, stage);
    let result = if t.portal() {
        provider.write_record(&ScriptWrite {
            layout: t.layout,
            table: Some(t.table_id),
            record: Some(record),
            cells: BTreeMap::from([(*id, Some(entered))]),
            fields: FieldChanges::new(),
            typed: BTreeSet::from([*id]),
            skip_validation: false,
            privilege_set: None,
            record_number: None,
            prepared: None,
            single_field_step: false,
        })
    } else {
        provider.update_record(
            t.layout,
            record,
            FieldChanges::from([(name.clone(), Some(entered))]),
        )
    };
    result
        .map(|_| ())
        .map_err(|e| format!("committing an edit of {name}: {}", e.message))
}

/// Duplicate Record of `record`: its fields Browse may change.
fn duplicate(provider: &mut dyn DataProvider, t: &Target, record: i64) -> Result<i64, String> {
    let page = provider
        .records(t.layout, 0, 100_000)
        .map_err(|e| e.message)?;
    let row = page
        .records
        .iter()
        .find(|r| r.id == record)
        .ok_or("the record to duplicate is not on the layout")?;
    let values: FieldChanges = page
        .fields
        .iter()
        .zip(&row.values)
        .filter(|(field, _)| t.capabilities.can_edit(field))
        .map(|(field, value)| (field.name.clone(), value.clone()))
        .collect();
    Ok(provider
        .create_record(t.layout, values)
        .map_err(|e| format!("Duplicate Record: {}", e.message))?
        .id)
}

/// Step 2 for one table: the record created and edited, and the one
/// deleted.
fn work(
    provider: &mut dyn DataProvider,
    schema: &CalcSchema,
    targets: &[Target],
    t: &Target,
) -> Result<(i64, i64), String> {
    let made = create(provider, schema, targets, t, CREATED)?;
    edit(provider, t, made, EDITED)?;
    if t.portal() {
        // A second row, deleted as a portal row is.
        let second = create(provider, schema, targets, t, SECOND)?;
        provider
            .delete_related_record(t.table_id, second)
            .map_err(|e| format!("deleting a portal row: {}", e.message))?;
        Ok((made, second))
    } else {
        let copy = duplicate(provider, t, made)?;
        provider
            .delete_record(t.layout, made)
            .map_err(|e| format!("Delete Record: {}", e.message))?;
        Ok((copy, made))
    }
}

/// Before closing: the edited fields read as entered, and a portal's
/// records match its parent record (so they show in its portal); every
/// record's stored values kept to compare.
fn read_before(
    provider: &mut dyn DataProvider,
    schema: &CalcSchema,
    targets: &[Target],
    t: &Target,
) -> Result<BTreeMap<u64, String>, String> {
    let records = records(provider, t.table_id)?;
    for (id, values) in &t.existing {
        let now = records.get(id).map(|v| format!("{:?}", v.values));
        if now.as_ref() != Some(values) {
            return Err(format!(
                "record {id}, there before anything was written, is gone or changed"
            ));
        }
    }
    if let Route::Portal { parent, matches } = &t.route {
        let parent = &targets[*parent];
        let key = parent.baseline.ok_or("the parent record was not made")? as u64;
        let parent_values = self::records(provider, parent.table_id)?
            .remove(&key)
            .ok_or("the parent record is gone")?;
        for record in [t.baseline, t.kept].into_iter().flatten() {
            let values = records
                .get(&(record as u64))
                .ok_or_else(|| format!("record {record} is gone"))?;
            for &(left, right) in matches {
                let (want, got) = (
                    text(schema, &parent_values, left),
                    text(schema, values, right),
                );
                if want.is_empty() || want != got {
                    return Err(format!(
                        "record {record}'s match field reads “{got}”, the parent's key “{want}”"
                    ));
                }
            }
        }
    }
    if let Some((id, name, kind)) = &t.field {
        for (record, stage) in [(t.baseline, BASELINE), (t.kept, EDITED)] {
            let Some(record) = record else { continue };
            let values = records
                .get(&(record as u64))
                .ok_or_else(|| format!("record {record} is gone"))?;
            let shown = text(schema, values, *id);
            let entered = entered(*kind, &t.table, stage);
            if shown != entered {
                return Err(format!(
                    "record {record}'s {name} reads “{shown}”, not “{entered}”"
                ));
            }
        }
    }
    Ok(records
        .into_iter()
        .map(|(id, values)| (id, format!("{:?}", values.values)))
        .collect())
}

/// After reopening: exactly the records left, each as before.
fn read_after(provider: &mut dyn DataProvider, t: &Target) -> Result<(), String> {
    let after: BTreeMap<u64, String> = records(provider, t.table_id)?
        .into_iter()
        .map(|(id, values)| (id, format!("{:?}", values.values)))
        .collect();
    let expected: BTreeSet<u64> = t
        .existing
        .keys()
        .copied()
        .chain(
            [t.baseline, t.kept]
                .into_iter()
                .flatten()
                .map(|id| id as u64),
        )
        .collect();
    let ids: BTreeSet<u64> = after.keys().copied().collect();
    if ids != expected {
        return Err(format!(
            "records {ids:?}, expected {expected:?} (record {:?} deleted)",
            t.deleted
        ));
    }
    match after
        .iter()
        .find(|(id, values)| t.before.get(id) != Some(values))
    {
        Some((id, _)) => Err(format!("record {id}'s stored values changed")),
        None => Ok(()),
    }
}

/// The measure on a copy of `original` in `dir`; the written copy left in
/// `keep` as `name`.
pub(super) fn card(original: &Path, dir: &Path, keep: Option<&Path>, name: &str) -> TrustCard {
    let mut card = TrustCard::default();
    let copy = dir.join("trust.fmp12");
    if let Err(e) = std::fs::create_dir_all(dir).and_then(|()| std::fs::copy(original, &copy)) {
        card.error = Some(format!("The file could not be copied: {e}"));
        return card;
    }
    if let Err(e) = guarded(|| measure(&copy, &mut card)) {
        card.error = Some(e);
    }
    if let Some(keep) = keep {
        let to = keep.join(name);
        match std::fs::create_dir_all(keep).and_then(|()| copy_released(&copy, &to)) {
            Ok(()) => card.written = Some(to),
            Err(e) => card.error = Some(format!("The written copy could not be kept: {e}")),
        }
    }
    card
}

/// Copy the file at `from` once its host has let go of it, holding the
/// lock the host takes (an exclusive `flock` on the file at the path;
/// `om-server`'s `filelock.rs`) so that what is copied is the file as last
/// saved, never one being written. Waits up to 30 s.
pub(super) fn copy_released(from: &Path, to: &Path) -> std::io::Result<()> {
    let started = std::time::Instant::now();
    loop {
        let file = std::fs::File::open(from)?;
        match file.try_lock() {
            Ok(()) => {
                #[cfg(unix)]
                {
                    // A save renames a new file into place: lock what is at
                    // the path now.
                    use std::os::unix::fs::MetadataExt;
                    let (held, there) = (file.metadata()?, std::fs::metadata(from)?);
                    if (held.dev(), held.ino()) != (there.dev(), there.ino()) {
                        continue;
                    }
                }
                std::fs::copy(from, to)?;
                return Ok(());
            }
            Err(std::fs::TryLockError::WouldBlock)
                if started.elapsed() < std::time::Duration::from_secs(30) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(std::io::Error::other(
                    "the file was still in use 30 s after it was closed",
                ))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

fn open(copy: &Path) -> Result<Box<dyn DataProvider>, String> {
    let mut provider: Box<dyn DataProvider> = om_provider::NativeProvider::from_env()
        .map(|p| Box::new(p) as Box<dyn DataProvider>)
        .map_err(|e| e.message)?;
    super::open(&mut *provider, copy)?;
    Ok(provider)
}

fn measure(copy: &Path, card: &mut TrustCard) -> Result<(), String> {
    let mut provider = open(copy).map_err(|e| format!("Opening: {e}"))?;
    let schema = provider
        .calc_schema()
        .map_err(|e| e.message)?
        .ok_or("no calculation schema")?;
    let mut targets = targets(&mut *provider, &schema, card);
    // The records each table holds before anything is written, which are
    // to survive unchanged.
    for t in &mut targets {
        if t.error.is_some() {
            continue;
        }
        match guarded(|| records(&mut *provider, t.table_id)) {
            Ok(records) => {
                t.existing = records
                    .into_iter()
                    .map(|(id, values)| (id, format!("{:?}", values.values)))
                    .collect()
            }
            Err(e) => t.error = Some(format!("reading the records there: {e}")),
        }
    }
    for i in 0..targets.len() {
        if targets[i].error.is_some() {
            continue;
        }
        match guarded(|| create(&mut *provider, &schema, &targets, &targets[i], BASELINE)) {
            Ok(id) => targets[i].baseline = Some(id),
            Err(e) => targets[i].error = Some(format!("the baseline record: {e}")),
        }
    }
    for i in 0..targets.len() {
        if targets[i].error.is_some() {
            continue;
        }
        match guarded(|| work(&mut *provider, &schema, &targets, &targets[i])) {
            Ok((kept, deleted)) => {
                targets[i].kept = Some(kept);
                targets[i].deleted = Some(deleted);
            }
            Err(e) => targets[i].error = Some(format!("creating, editing and deleting: {e}")),
        }
    }
    for i in 0..targets.len() {
        if targets[i].error.is_some() {
            continue;
        }
        match guarded(|| read_before(&mut *provider, &schema, &targets, &targets[i])) {
            Ok(before) => targets[i].before = before,
            Err(e) => targets[i].error = Some(format!("reading back before closing: {e}")),
        }
    }
    provider.close();
    drop(provider);

    // Reopened as soon as the closed file is released, within 30 s.
    let started = std::time::Instant::now();
    let mut provider = loop {
        match open(copy) {
            Ok(provider) => break provider,
            Err(e)
                if e.contains("already open")
                    && started.elapsed() < std::time::Duration::from_secs(30) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("Reopening: {e}")),
        }
    };
    let waited = started.elapsed().as_millis() as u64;
    card.reopen_wait_ms = (waited >= 100).then_some(waited);
    for t in &mut targets {
        if t.error.is_some() {
            continue;
        }
        if let Err(e) = guarded(|| read_after(&mut *provider, t)) {
            t.error = Some(format!("after reopening: {e}"));
        }
    }
    provider.close();
    for t in targets {
        card.tables.push(TrustTable {
            passed: t.error.is_none(),
            table: t.table,
            route: t.route_name,
            error: t.error,
        });
    }
    Ok(())
}
