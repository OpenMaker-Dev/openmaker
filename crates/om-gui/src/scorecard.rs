//! The alpha scorecard (ROADMAP.md, "Alpha gate"; docs/development.md):
//! what still blocks a user of each given `.fmp12` file, measured with the
//! desktop client's own decisions. Each file is copied and opened as File >
//! Open does (`DataProvider::open_local`, the account the file suggests and
//! an empty password); nothing is drawn and no step runs.
//!
//! - Edit: each base table through the record planner the provider plans
//!   writes with, and each field object through Browse's edit gates
//!   ([`crate::document::browse_edits`]; option sets'
//!   `om_data::choice::availability`, or `related_availability` in a
//!   portal row or of a related field; a related field's
//!   `RecordCapabilities::can_edit_related`), and in a portal's creation
//!   row as `paint_new_row` decides. The gates that need a live window
//!   (`Evaluator::related_record`, `portal_allows_creation`,
//!   `new_related_field`) are mirrored here, assuming a related record.
//! - Scripts: each enabled step of the file's scripts and buttons, through
//!   the script runtime against [`Probe`], a host with the capabilities the
//!   desktop host leaves to the runtime's default (error 3), and those it
//!   has for some steps only, which refuse the steps the desktop host
//!   refuses by its own decision (`OPENED_MENU_ITEMS`, `bar_flag`).
//! - Output: each layout through the report builder Preview and Save
//!   Records as PDF share, for one record of empty values.
//! - Browse: each layout's objects by kind, and those Browse does not draw
//!   or open as FileMaker does, by what `paint_object` does with them; a
//!   drop-down calendar opens where its field is a date the user may
//!   change (#562, #571).
//! - Build (criterion 7, the desktop route): on another copy, scripts,
//!   layouts, buttons, object frames and field options made through the
//!   operations the Script Workspace, Layout mode and Manage Database call,
//!   each closed and reopened ([`build`]).
//! - Trust: on another copy, records written in every base table as
//!   Browse writes them, through its layout or a portal, then the file
//!   closed and reopened ([`trust`]); with `OM_SCORECARD_TRUST` set to a
//!   folder, the written copies are left there for FileMaker's consistency
//!   check.
//! - Calculations: each function the file's fields, custom functions,
//!   script and button steps and layout objects call, evaluated alone with
//!   empty arguments by the calculation host Browse uses (`FileHost`); one
//!   that answers "not supported" has no implementation.
//!
//! What it leaves out: nothing is compared with FileMaker ("FileMaker lets
//! the user change it" is read from the object's Browse entry option, the
//! field's class and auto-entry). Browse's gate is checked on the first
//! record page. A step that reaches a capability the desktop host has may
//! still refuse particular options, modes or targets with error 3; those
//! refusals are not measured. Reports are measured without values: text
//! overflow, sub-summary groups and value-dependent refusals are not.
//! Functions are checked on the desktop's Browse host, not on FileMaker
//! Pro's own platform support (a mobile-only function counts as missing).
//!
//! ```text
//! OM_SCORECARD_FILES=a.fmp12:b.fmp12 OM_SCORECARD_OUT=<folder> \
//!   [OM_SCORECARD_TRUST=<folder>] \
//!   cargo test -p om-gui --lib scorecard::alpha_scorecard -- --ignored --exact --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use om_data::calc::ValueType;
use om_data::host::{CalcSchema, FieldClass};
use om_data::rules::RecordPlan;
use om_data::runtime::{
    Accounts, Bar, Dialogs, FieldRef, Fields, Files, FoundSets, HostError, HostResult, Navigation,
    Network, Platform, Records, ScriptHost, StepCall, Visibility, Windows,
};
use om_data::{DataProvider, LocalFile, LocalOpen, RecordCapabilities, RenderLayout, ScriptStep};
use om_layout::{FieldEntry, Layout, LayoutObject, ObjectKind};
use serde::Serialize;

use crate::report::{self, ReportError};

mod build;
mod entry;
mod hosted;
mod md_steps;
pub(crate) mod server;
mod trust;

#[derive(Serialize, Default)]
struct FileCard {
    file: String,
    open: Open,
    tables: Vec<TableCard>,
    layouts: Vec<LayoutCard>,
    /// Scripts that are scripts (not folders or separators).
    scripts: usize,
    steps: Vec<StepCard>,
    functions: Vec<FunctionCard>,
    trust: trust::TrustCard,
    build: build::BuildCard,
}

#[derive(Serialize)]
struct FunctionCard {
    name: String,
    implemented: bool,
    /// The error the function gave alone with empty arguments, when it has
    /// no implementation (1225 "not supported").
    error: Option<i32>,
    /// Where it is called: `field T::F`, `custom function F`, `script S`,
    /// `layout L`.
    used_in: BTreeSet<String>,
}

#[derive(Serialize, Default)]
struct Open {
    opened: bool,
    account: Option<String>,
    read_only: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct TableCard {
    name: String,
    writable: bool,
    /// Every field the record planner refuses, with its reason, in the
    /// order it refuses them; a refusal naming no field ends the list.
    refusals: Vec<Refusal>,
}

#[derive(Serialize)]
struct Refusal {
    field: Option<String>,
    reason: String,
}

#[derive(Serialize)]
struct LayoutCard {
    id: u32,
    name: String,
    error: Option<String>,
    /// `LayoutDef.orientation` (0 portrait, 2 landscape).
    orientation: Option<u32>,
    /// Objects by kind (field objects by control style).
    objects: BTreeMap<String, usize>,
    /// Objects Browse does not draw or open as FileMaker does, by why.
    browse: BTreeMap<&'static str, usize>,
    fields: Vec<FieldCard>,
    report: ReportCard,
}

#[derive(Serialize)]
struct FieldCard {
    object: u64,
    field: String,
    control: &'static str,
    /// FileMaker Pro lets the user type into or choose in it in Browse.
    filemaker_edits: bool,
    /// OpenMaker's Browse does.
    editable: bool,
    /// Why not, when either refuses.
    class: Option<&'static str>,
    detail: Option<String>,
    /// For a drop-down calendar: why Browse does not open its calendar.
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_refusal: Option<String>,
    /// For a field in a portal: the portal's creation row.
    creation_row: Option<CreationRow>,
}

#[derive(Serialize)]
struct CreationRow {
    /// The portal's table occurrence.
    portal: String,
    /// The relationship from the layout's occurrence allows creation, so
    /// the portal shows a creation row.
    allowed: bool,
    /// The field takes typing in it.
    editable: bool,
    /// Why not, when the row is shown.
    why: Option<&'static str>,
}

#[derive(Serialize)]
struct ReportCard {
    accepted: bool,
    /// The layout's paper was refused: the other refusals were measured on
    /// the default paper.
    default_paper: bool,
    /// The layout prints in columns: what follows the refusal was measured
    /// as if it printed in one.
    one_column: bool,
    refusals: Vec<String>,
}

#[derive(Serialize)]
struct StepCard {
    id: u16,
    name: String,
    /// `implemented`, `error3` (OpenMaker), `filemaker` (FileMaker Pro has
    /// no handler either) or `unknown`.
    status: &'static str,
    why: Option<String>,
    /// Scripts with the step, by name.
    scripts: BTreeSet<String>,
    /// Buttons whose single step it is.
    buttons: usize,
    /// Enabled steps of this id in scripts.
    uses: usize,
}

#[test]
#[ignore = "requires .fmp12 files: OM_SCORECARD_FILES (a path list) and OM_SCORECARD_OUT (a folder)"]
fn alpha_scorecard() {
    let files: Vec<PathBuf> = std::env::split_paths(
        &std::env::var_os("OM_SCORECARD_FILES").expect("set OM_SCORECARD_FILES"),
    )
    .collect();
    let out = PathBuf::from(std::env::var_os("OM_SCORECARD_OUT").expect("set OM_SCORECARD_OUT"));
    // Where the Trust measure leaves the files it wrote, named for their
    // folders (`Assets.fmp12`), for FileMaker's consistency check.
    let keep = std::env::var_os("OM_SCORECARD_TRUST").map(PathBuf::from);
    let desktop = Desktop::read();
    let temp = tempfile::tempdir().expect("temporary folder");
    let cards: Vec<FileCard> = files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let dir = temp.path().join(i.to_string());
            let mut card = file_card(path, &dir, &desktop);
            let name = format!("{}.fmp12", label(path).split('/').next().unwrap_or("file"));
            card.trust = trust::card(path, &dir.join("trust"), keep.as_deref(), &name);
            card.build = build::card(path, &dir.join("build"));
            card
        })
        .collect();
    let created = build::create_new(&temp.path().join("create-new"), keep.as_deref());
    std::fs::create_dir_all(&out).expect("output folder");
    let json = serde_json::to_string_pretty(&(&cards, &created)).expect("JSON");
    std::fs::write(out.join("scorecard.json"), json).expect("write scorecard.json");
    let summary = with_create_new(summary(&cards), &created);
    std::fs::write(out.join("scorecard.md"), &summary).expect("write scorecard.md");
    println!("{summary}");
}

/// `summary` with File > Create New's row under the Build total, and its
/// detail.
fn with_create_new(summary: String, created: &build::CreateNewCard) -> String {
    let row = format!(
        "| 7 Build | File > Create New: a blank file made by the command, a field placed and two records added, read back after reopening | {} |",
        if created.done { "yes" } else { "no" }
    );
    let mut lines: Vec<String> = Vec::new();
    for line in summary.lines() {
        lines.push(line.to_owned());
        if line.starts_with("| 7 Build |") {
            lines.push(row.clone());
        }
    }
    format!(
        "{}\n\n## Build: File > Create New\n\n{}\n",
        lines.join("\n"),
        created.detail
    )
}

/// A file's last two path components: the starter solutions are all
/// `en.fmp12`, in folders named for them.
fn label(path: &Path) -> String {
    let mut parts: Vec<_> = path
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.reverse();
    parts.join("/")
}

fn file_card(path: &Path, dir: &Path, desktop: &Desktop) -> FileCard {
    let mut card = FileCard {
        file: label(path),
        ..FileCard::default()
    };
    // The client may write to the file it opens: it opens a copy.
    let copy = dir.join(path.file_name().unwrap_or_default());
    if let Err(error) = std::fs::create_dir_all(dir).and_then(|()| std::fs::copy(path, &copy)) {
        card.open.error = Some(format!("The file could not be copied: {error}"));
        return card;
    }
    let mut provider: Box<dyn DataProvider> = match om_provider::NativeProvider::from_env() {
        Ok(provider) => Box::new(provider),
        Err(error) => {
            card.open.error = Some(error.message);
            return card;
        }
    };
    match open(&mut *provider, &copy) {
        Ok((account, read_only)) => {
            card.open = Open {
                opened: true,
                account: Some(account),
                read_only,
                error: None,
            }
        }
        Err(error) => {
            card.open.error = Some(error);
            return card;
        }
    }
    measure_opened(&mut card, &mut *provider, Some(path), desktop);
    provider.close();
    card
}

/// Every measure but Trust and Build of the file opened by `provider`,
/// locally or over the wire, with only reading calls. Its table
/// definitions are read from `path`, the file or a copy of it on disk;
/// without one, Edit has no tables and no auto-entry prohibitions.
fn measure_opened(
    card: &mut FileCard,
    provider: &mut dyn DataProvider,
    path: Option<&Path>,
    desktop: &Desktop,
) {
    let schema = match provider.calc_schema() {
        Ok(Some(schema)) => schema,
        Ok(None) => Arc::new(CalcSchema::new("", om_data::calc::Locale::en_us())),
        Err(error) => {
            card.open.error = Some(format!("No calculation schema: {}", error.message));
            return;
        }
    };
    let (tables, prohibited) = match path {
        Some(path) => tables(path, &schema).unwrap_or_else(|error| {
            card.open.error = Some(error);
            Default::default()
        }),
        None => Default::default(),
    };
    let read_only: BTreeSet<&str> = tables
        .iter()
        .filter(|t| !t.writable)
        .map(|t| t.name.as_str())
        .collect();
    let mut buttons: Vec<(ScriptStep, String)> = Vec::new();
    let mut uses = Uses::default();
    // Separators and folders of the layout menu are no layouts to use.
    let structural: BTreeSet<u32> = provider
        .layout_menu()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.flags & (0x01 | 0x02 | 0x10) != 0)
        .map(|entry| entry.id)
        .collect();
    match provider.layouts() {
        Ok(layouts) => {
            for info in layouts.into_iter().filter(|l| !structural.contains(&l.id)) {
                let layout = layout_card(
                    provider,
                    info,
                    &schema,
                    &read_only,
                    &prohibited,
                    &mut buttons,
                    &mut uses,
                );
                card.layouts.push(layout);
            }
        }
        Err(error) => card.open.error = Some(format!("No layouts: {}", error.message)),
    }
    card.tables = tables;
    match provider.scripts() {
        Ok(catalog) => {
            let scripts: Vec<_> = catalog
                .scripts
                .iter()
                .filter(|s| s.kind == om_data::ScriptKind::Script)
                .collect();
            card.scripts = scripts.len();
            card.steps = steps(&scripts, &buttons, desktop);
            for script in &scripts {
                for step in script.steps.iter().filter(|s| s.is_enabled()) {
                    uses.step(step, &format!("script {}", script.name));
                }
            }
        }
        Err(error) => card.open.error = Some(format!("No scripts: {}", error.message)),
    }
    for (step, layout) in &buttons {
        uses.step(step, &format!("layout {layout}"));
    }
    for table in schema.tables() {
        for def in table.fields.values() {
            let place = format!("field {}::{}", table.name, def.name);
            for formula in [&def.formula, &def.auto_enter, &def.validation]
                .into_iter()
                .flatten()
            {
                uses.walk(&formula.polish, &place);
            }
        }
    }
    for function in schema.custom_functions() {
        uses.walk(
            &function.body,
            &format!("custom function {}", function.name),
        );
    }
    card.functions = uses.cards(&schema);
}

/// Open `path` as File > Open does: the client asks for an account and
/// suggests the one the file names; the scorecard answers with it and an
/// empty password, the starter solutions' default account.
fn open(provider: &mut dyn DataProvider, path: &Path) -> Result<(String, Option<String>), String> {
    let mut file = LocalFile {
        path: path.to_owned(),
        credentials: None,
    };
    let account = match provider.open_local(&file).map_err(|e| e.message)? {
        LocalOpen::Opened { read_only } => return Ok((String::new(), read_only)),
        LocalOpen::NeedsLogin { account } => account,
    };
    file.credentials = Some((account.clone(), Default::default()));
    match provider
        .open_local(&file)
        .map_err(|e| format!("{} (account “{account}”, empty password)", e.message))?
    {
        LocalOpen::Opened { read_only } => Ok((account, read_only)),
        LocalOpen::NeedsLogin { .. } => Err(format!("Account “{account}” was refused")),
    }
}

/// Fields whose auto-entry prohibits modification, by (table, field).
type Prohibited = BTreeSet<(u64, u64)>;

/// Each base table of the file through the record planner the provider
/// plans creation and edits with (`table_plan`): its definitions read
/// from the file, in the open file's calculation schema.
fn tables(path: &Path, schema: &CalcSchema) -> Result<(Vec<TableCard>, Prohibited), String> {
    let file = om_hbam::HbamFile::open(path).map_err(|e| e.to_string())?;
    let tree = file.key_tree().map_err(|e| e.to_string())?;
    let catalog = om_hbam::Catalog::from_tree(&tree).map_err(|e| e.to_string())?;
    let mut cards = Vec::new();
    let mut prohibited = Prohibited::new();
    for table in &catalog.tables {
        let mut table = table.clone();
        table.fields = om_hbam::catalog::fields(&tree, table.id).map_err(|e| e.to_string())?;
        prohibited.extend(
            table
                .fields
                .iter()
                .filter(|f| {
                    f.decoded_options()
                        .is_some_and(|o| o.auto_entry_editing_prohibited())
                })
                .map(|f| (table.id, f.id)),
        );
        let name = table.name.clone();
        let refusals = plan_refusals(table, &tree, schema);
        cards.push(TableCard {
            name,
            writable: refusals.is_empty(),
            refusals,
        });
    }
    Ok((cards, prohibited))
}

/// The planner stops at the first field it refuses and names it ("Field
/// Name: reason"); that field is left out and the table planned again, so
/// every refused field is listed. A field that depends on one left out is
/// then refused as referencing an unavailable field.
fn plan_refusals(
    mut table: om_hbam::Table,
    tree: &om_hbam::KeyTree<'_>,
    schema: &CalcSchema,
) -> Vec<Refusal> {
    let mut refusals = Vec::new();
    for _ in 0..=table.fields.len() {
        let Err(error) = RecordPlan::from_file_schema(&table, tree, schema) else {
            break;
        };
        let named = |name: &str| {
            error
                .message
                .strip_prefix("Field ")
                .and_then(|rest| rest.strip_prefix(name))
                .filter(|rest| rest.starts_with(':') || rest.starts_with(' '))
                .map(|rest| rest.trim_start_matches(':').trim().to_owned())
        };
        let field = table
            .fields
            .iter()
            .filter_map(|f| Some((f.id, f.name.clone(), named(&f.name)?)))
            .max_by_key(|(_, name, _)| name.len());
        match field {
            Some((id, name, reason)) => {
                refusals.push(Refusal {
                    field: Some(name),
                    reason,
                });
                table.fields.retain(|f| f.id != id);
            }
            None => {
                refusals.push(Refusal {
                    field: None,
                    reason: error.message,
                });
                break;
            }
        }
    }
    refusals
}

fn layout_card(
    provider: &mut dyn DataProvider,
    info: om_data::LayoutInfo,
    schema: &CalcSchema,
    read_only: &BTreeSet<&str>,
    prohibited: &Prohibited,
    buttons: &mut Vec<(ScriptStep, String)>,
    uses: &mut Uses,
) -> LayoutCard {
    let mut card = LayoutCard {
        id: info.id,
        name: info.name,
        error: None,
        orientation: None,
        objects: BTreeMap::new(),
        browse: BTreeMap::new(),
        fields: Vec::new(),
        report: ReportCard {
            accepted: false,
            default_paper: false,
            one_column: false,
            refusals: Vec::new(),
        },
    };
    let render = match provider.render_layout(info.id) {
        Ok(Some(render)) => render,
        Ok(None) => {
            card.error = Some("The provider has no layout to render".into());
            return card;
        }
        Err(error) => {
            card.error = Some(error.message);
            return card;
        }
    };
    let capabilities = provider
        .record_capabilities(info.id)
        .unwrap_or_else(|error| {
            card.error = Some(format!("Record capabilities: {}", error.message));
            RecordCapabilities::default()
        });
    let fields = match provider.records(info.id, 0, 1) {
        Ok(page) => page.fields,
        Err(error) => {
            card.error = Some(format!("Records: {}", error.message));
            Vec::new()
        }
    };
    let layout = &render.layout;
    card.orientation = layout.def.as_ref().map(|d| d.orientation);
    for object in &layout.objects.objects {
        *card.objects.entry(kind_name(object)).or_default() += 1;
        let step = match (&object.button, &object.group) {
            (Some(button), _) => button.script_step.as_ref(),
            (None, Some(group)) => group.script_step.as_ref(),
            (None, None) => None,
        };
        // A button without an action stores step 0: nothing runs.
        if let Some(step) = step
            .and_then(ScriptStep::from_layout)
            .filter(|s| s.id() != 0)
        {
            buttons.push((step, card.name.clone()));
        }
        if let Some(gap) = browse_gap(object) {
            *card.browse.entry(gap).or_default() += 1;
        }
        let place = format!("layout {}", card.name);
        let calcs = &object.calcs;
        for formula in [
            &calcs.label,
            &calcs.tooltip,
            &calcs.placeholder,
            &calcs.hide,
            &calcs.script_parameter,
            &calcs.active_segment,
            &calcs.web_address,
            &calcs.portal_filter,
        ]
        .into_iter()
        .flatten()
        .chain(calcs.conditions.iter().filter_map(|c| c.formula.as_ref()))
        {
            uses.walk(&formula.polish, &place);
        }
        if object.kind() == ObjectKind::Field {
            if let Some(field) = field_card(
                &render,
                object,
                &fields,
                &capabilities,
                schema,
                read_only,
                prohibited,
            ) {
                let gap = calendar_gap(object, &field, schema);
                if let Some(gap) = gap.filter(|_| browse_gap(object).is_none()) {
                    *card.browse.entry(gap).or_default() += 1;
                }
                card.fields.push(field);
            }
        }
    }
    card.report = report_card(layout, schema);
    card
}

/// An object's kind; a field object's by its control style
/// (layout-model.md 5.4).
fn kind_name(object: &LayoutObject) -> String {
    match (object.kind(), &object.field_obj) {
        (ObjectKind::Field, Some(field)) => format!("Field: {}", control(field.display_type)),
        (kind, _) => format!("{kind:?}"),
    }
}

fn control(display_type: u32) -> &'static str {
    match display_type {
        1 => "drop-down list",
        2 => "pop-up menu",
        3 => "checkbox set",
        4 => "radio button set",
        6 => "drop-down calendar",
        7 => "concealed edit box",
        _ => "edit box",
    }
}

/// Whether FileMaker and OpenMaker let the user change the field object
/// in Browse, and why not.
fn field_card(
    render: &RenderLayout,
    object: &LayoutObject,
    fields: &[om_data::Field],
    capabilities: &RecordCapabilities,
    schema: &CalcSchema,
    read_only: &BTreeSet<&str>,
    prohibited: &Prohibited,
) -> Option<FieldCard> {
    let field_obj = object.field_obj.as_ref()?;
    let spec = object.field_spec()?;
    let layout = &render.layout;
    let alias = layout.def.as_ref().map_or(0, |d| d.table_alias_key);
    let occurrence = om_layout::occurrence_id(spec.table_key);
    let def = occurrence
        .zip(spec.field_id())
        .and_then(|(o, f)| schema.field(o, f));
    let table = occurrence.and_then(|o| schema.table_of(o));
    let name = match (occurrence.and_then(|o| schema.occurrence(o)), def) {
        (Some(o), Some(def)) => format!("{}::{}", o.name, def.name),
        _ => format!("(unresolved field of object {})", object.id),
    };
    let in_portal = crate::document::portals::enclosing(layout, object.id)
        .and_then(|id| layout.objects.get(id))
        .and_then(|p| p.portal.as_ref());
    let portal = in_portal.is_some();
    let portal_occurrence = in_portal.and_then(|p| om_layout::occurrence_id(p.table_alias_key));
    let layout_occurrence = om_layout::occurrence_id(alias);
    // The gates `paint_object` and `option_set_refusal` apply.
    let may_change = crate::document::browse_edits(render, fields, capabilities, object.id);
    let option_set = matches!(field_obj.display_type, 2..=4);
    // `drop_down::offers_list`: a drop-down list.
    let list_control = field_obj.display_type == 1;
    let container = def.is_some_and(|d| d.value_type == ValueType::Container);
    let related_spec = spec.table_key != alias;
    // `paint_object`'s `related` target (#538, #571) through
    // `Evaluator::related_record`: a field of a related occurrence, in a
    // portal row only of the portal's own occurrence, no container, one
    // repetition. It needs a related record: the scorecard assumes there is
    // one.
    let related_target = occurrence.zip(spec.field_id()).filter(|&(o, _)| {
        !container
            && Some(o) != layout_occurrence
            && spec.repetition <= 1
            && (!portal || portal_occurrence == Some(o))
    });
    let related_edits = related_target.is_some_and(|(o, f)| capabilities.can_edit_related(o, f));
    // Whether the user may choose here: `related_refusal` for a related
    // record's field, `option_set_refusal` otherwise. An option set takes
    // input only so; a drop-down calendar's field takes typing either way,
    // but its calendar opens only so (#562, #571). The list's items are
    // not read (there are no records here): a list that is empty or cannot
    // be read, which both refusals also refuse, is not caught, in saved
    // rows or the creation row.
    let choice = || {
        match related_target {
            Some((o, f)) => {
                om_data::choice::related_availability(object, capabilities.can_edit_related(o, f))
            }
            None => om_data::choice::availability(object, alias, portal, may_change),
        }
        .map_err(|e| e.message())
    };
    // A container of the portal's occurrence in a portal row, saved or the
    // creation row (#584, `select_related_container`): a click selects it
    // and the Insert menu fills it.
    let row_container = container
        && portal
        && occurrence.is_some()
        && occurrence == portal_occurrence
        && spec.repetition <= 1
        && occurrence
            .zip(spec.field_id())
            .is_some_and(|(o, f)| capabilities.can_edit_related(o, f));
    let gate = if option_set {
        choice()
    } else if row_container {
        Ok(())
    } else if container && !portal && !related_spec {
        // A container takes no typing: Browse selects a bound one outside
        // a portal (`select_container`) and the Insert menu fills it when
        // the record may be updated (`container_insert` in app.rs).
        if render.field_bindings.contains_key(&object.id) && capabilities.update {
            Ok(())
        } else {
            Err(String::new())
        }
    } else if (!portal && may_change) || related_edits {
        Ok(())
    } else {
        Err(String::new())
    };
    // The portal's creation row (#543, `paint_new_row`): shown when the
    // relationship from the layout's occurrence allows creation
    // (`Evaluator::portal_allows_creation`); a field of the portal's
    // occurrence (`Evaluator::new_related_field`) Browse may edit takes
    // input there as in a saved row: typing, its value-list control where
    // `related_refusal` lets the user choose (#591), or a file (#584).
    let creation_row = in_portal.map(|_| {
        let allowed = layout_occurrence
            .zip(portal_occurrence)
            .is_some_and(|(own, other)| {
                other != own
                    && schema.table_of(other).is_some()
                    && schema.relationships().iter().any(|r| {
                        (r.left == own && r.right == other && r.options & 0x10 != 0)
                            || (r.right == own && r.left == other && r.options & 0x20 != 0)
                    })
            });
        let editable = allowed
            && spec.repetition <= 1
            && occurrence.is_some_and(|o| Some(o) == portal_occurrence && def.is_some())
            && occurrence
                .zip(spec.field_id())
                .is_some_and(|(o, f)| capabilities.can_edit_related(o, f))
            && (!option_set || choice().is_ok());
        let why = (allowed && !editable).then(|| {
            if occurrence != portal_occurrence {
                "field of another occurrence than the portal's"
            } else if spec.repetition > 1 {
                "repetition"
            } else if table.is_some_and(|t| read_only.contains(t.name.as_str())) {
                "table read-only"
            } else if option_set {
                "value-list control refused"
            } else {
                "other"
            }
        });
        CreationRow {
            portal: portal_occurrence
                .and_then(|o| schema.occurrence(o))
                .map_or_else(String::new, |o| o.name.clone()),
            allowed,
            editable,
            why,
        }
    });
    let editable = gate.is_ok();
    // What FileMaker itself does not let the user change.
    let filemaker = if field_obj.field_entry(false) != FieldEntry::Edit {
        Some("field entry off in Browse")
    } else {
        match def.map(|d| d.class) {
            Some(FieldClass::Calculation) => Some("calculation"),
            Some(FieldClass::Summary) => Some("summary"),
            _ if table
                .zip(def)
                .is_some_and(|(t, d)| prohibited.contains(&(t.id, d.id))) =>
            {
                Some("auto-entry prohibits modification")
            }
            _ => None,
        }
    };
    let blocker = || -> &'static str {
        let Some(def) = def else {
            return "other";
        };
        let related = portal || related_spec;
        if portal && portal_occurrence != occurrence {
            "portal field of another occurrence than the portal's"
        } else if table.is_some_and(|t| read_only.contains(t.name.as_str())) {
            "table read-only"
        } else if related && (option_set || list_control) {
            "value-list control in a portal or of a related field"
        } else if container && related {
            "container of a related field outside a portal row of its occurrence"
        } else if container {
            "container"
        } else if def.global {
            "global"
        } else if def.repetitions > 1 || spec.repetition > 1 || field_obj.num_of_reps.abs() > 1 {
            "repetition"
        } else if matches!(
            def.value_type,
            ValueType::Date | ValueType::Time | ValueType::Timestamp
        ) {
            "field type (date, time, timestamp)"
        } else if option_set {
            "control style"
        } else if portal {
            "portal field"
        } else if related_spec {
            "related field"
        } else {
            "other"
        }
    };
    let class = match (filemaker, editable) {
        (Some(class), _) => Some(class),
        (None, true) => None,
        (None, false) => Some(blocker()),
    };
    Some(FieldCard {
        object: object.id,
        field: name,
        control: control(field_obj.display_type),
        filemaker_edits: filemaker.is_none(),
        editable,
        class,
        detail: gate.err().filter(|e| !e.is_empty()),
        calendar_refusal: (field_obj.display_type == 6)
            .then(choice)
            .and_then(Result::err),
        creation_row,
    })
}

/// The report builder's verdict on `layout` for Preview and Save Records
/// as PDF, without data: one record whose fields are empty, the layout's
/// own paper, no sort groups. Every object's refusal is listed; then the
/// first refusal of planning and drawing the page, if it is another, with
/// every conditional format holding. When
/// the layout's paper is refused, the rest is measured on the default
/// paper (US Letter, portrait), and a layout that prints in columns as if
/// it printed in one, so that what remains shows too. Each field, text and
/// button object's content is checked as the builder checks it while
/// drawing, whether or not planning gets that far.
fn report_card(layout: &Layout, schema: &CalcSchema) -> ReportCard {
    let mut refusals: Vec<String> = Vec::new();
    let mut refuse = |error: ReportError| {
        if !refusals.contains(&error.message) {
            refusals.push(error.message);
        }
    };
    let occurrence = layout
        .def
        .as_ref()
        .and_then(|d| om_layout::occurrence_id(d.table_alias_key))
        .unwrap_or(0);
    if let Err(error) = report::report_breaks(layout, schema, occurrence) {
        refuse(error);
    }
    let mut default_paper = false;
    let paper = om_layout::pagination::PageSize::for_layout(layout).unwrap_or_else(|error| {
        refuse(ReportError::unsupported(error));
        default_paper = true;
        om_layout::pagination::PageSize {
            margins: om_layout::paper::printer_margins(),
            ..Default::default()
        }
    });
    let (object_refusals, placed) = report::report_object_refusals(layout, &paper);
    for error in object_refusals {
        refuse(error);
    }
    // What `pdf_field` checks before it reads a value; no value is read.
    let content = |object: &LayoutObject, group: Option<om_layout::BreakField>| {
        report::object_text(object, occurrence, schema, |field| {
            let def = report::field_def(schema, field, object)?;
            report::group_summary(def, field, occurrence, group)?;
            Ok(String::new())
        })
    };
    // Only objects the report prints: not a navigation part's, nor what a
    // refused object holds.
    for object in placed.iter().filter_map(|id| layout.objects.get(*id)) {
        if matches!(
            object.kind(),
            ObjectKind::Field | ObjectKind::Text | ObjectKind::Button
        ) {
            if let Err(error) = content(object, None) {
                refuse(error);
            }
        }
    }
    // Without data no condition is decided: every conditional format
    // holds, so a style a condition would bring that reports refuse shows.
    struct Content<F>(F);
    impl<F> report::ReportContent for Content<F>
    where
        F: Fn(&LayoutObject, Option<om_layout::BreakField>) -> Result<String, ReportError>,
    {
        fn text(
            &mut self,
            object: &LayoutObject,
            context: report::ContentContext,
        ) -> Result<String, ReportError> {
            (self.0)(object, context.group)
        }

        fn conditions(
            &mut self,
            object: &LayoutObject,
            _: report::ContentContext,
        ) -> Result<Option<Vec<usize>>, ReportError> {
            Ok(Some((0..object.calcs.conditions.len()).collect()))
        }

        // No related records: the rows print their backgrounds only.
        fn portal_records(
            &mut self,
            _: &LayoutObject,
            _: report::ContentContext,
        ) -> Result<Option<Vec<u64>>, ReportError> {
            Ok(Some(Vec::new()))
        }
    }
    let render = |layout: &Layout| {
        report::render_pdf_with(layout, 1, paper.clone(), &[], &mut Content(&content)).err()
    };
    let mut one_column = false;
    if let Some(error) = render(layout) {
        // When printing in columns is what was refused, what follows is
        // measured in one column.
        let columns = om_layout::pagination::PRINT_IN_COLUMNS;
        if let Some(def) = layout.def.as_ref().filter(|d| d.flags & columns != 0) {
            let mut single = layout.clone();
            single.def = Some(om_layout::LayoutDef {
                flags: def.flags & !columns,
                ..def.clone()
            });
            let next = render(&single);
            if next.as_ref().map(|e| &e.message) != Some(&error.message) {
                one_column = true;
                if let Some(next) = next {
                    refuse(next);
                }
            }
        }
        refuse(error);
    }
    ReportCard {
        accepted: refusals.is_empty(),
        default_paper,
        one_column,
        refusals,
    }
}

/// Every enabled step of `scripts` and `buttons`, by id, with what the
/// desktop does with it.
fn steps(
    scripts: &[&om_data::Script],
    buttons: &[(ScriptStep, String)],
    desktop: &Desktop,
) -> Vec<StepCard> {
    let mut cards: BTreeMap<u16, StepCard> = BTreeMap::new();
    let card = |cards: &mut BTreeMap<u16, StepCard>, step: &ScriptStep, button: bool| {
        let id = step.id();
        let entry = cards.entry(id).or_insert_with(|| StepCard {
            id,
            name: om_data::script_step_name(id).unwrap_or("?").to_owned(),
            status: "implemented",
            why: None,
            scripts: BTreeSet::new(),
            buttons: 0,
            uses: 0,
        });
        // Each stored form of the step: any refused one refuses the step.
        if entry.status == "implemented" {
            let (status, why) = desktop.status(step, button);
            entry.status = status;
            entry.why = why;
        }
    };
    for script in scripts {
        for step in script.steps.iter().filter(|s| s.is_enabled()) {
            card(&mut cards, step, false);
            let entry = cards.get_mut(&step.id()).expect("the step's card");
            entry.scripts.insert(script.name.clone());
            entry.uses += 1;
        }
    }
    for (step, _) in buttons.iter().filter(|(s, _)| s.is_enabled()) {
        card(&mut cards, step, true);
        cards.get_mut(&step.id()).expect("the step's card").buttons += 1;
    }
    cards.into_values().collect()
}

/// Whether drop-down calendar `object` does not open as FileMaker's does:
/// Browse opens one on a date field (`calendar::is_calendar`) where
/// `option_set_refusal` or, in a portal row or of a related field,
/// `related_refusal` lets the user choose (#562, #571; the field card's
/// `calendar_refusal`). One FileMaker itself does not let the user change
/// opens in neither.
fn calendar_gap(
    object: &LayoutObject,
    field: &FieldCard,
    schema: &CalcSchema,
) -> Option<&'static str> {
    if object.field_obj.as_ref()?.display_type != 6 {
        return None;
    }
    let spec = object.field_spec()?;
    let date = om_layout::occurrence_id(spec.table_key)
        .zip(spec.field_id())
        .and_then(|(o, f)| schema.field(o, f))
        .is_some_and(|d| d.value_type == ValueType::Date);
    if !date {
        Some("drop-down calendar of a field other than a date: does not open")
    } else if field.filemaker_edits && field.calendar_refusal.is_some() {
        Some("drop-down calendar Browse does not open (read-only field, repetitions, or related-record rules)")
    } else {
        None
    }
}

/// What Browse does not draw or open of `object` as FileMaker does, read
/// from `paint_object` (document.rs): it paints no rotation, no chart's or
/// plug-in object's content (only its styling) and no unknown kind; a
/// web viewer's page needs the `web-viewer` feature and shows in Form view on the current record only
/// (#278). Containers draw their picture or a file icon (`container.rs`);
/// whether FileMaker would show a media player or an interactive PDF
/// there instead depends on an object option OpenMaker does not read.
fn browse_gap(object: &LayoutObject) -> Option<&'static str> {
    match object.kind() {
        _ if object.rotation != 0 => Some("rotated object: drawn unrotated"),
        ObjectKind::External if object.web_viewer().is_some() => {
            if cfg!(feature = "web-viewer") {
                Some("web viewer: page in Form view on the current record only")
            } else {
                Some("web viewer: a note (client built without the web-viewer feature)")
            }
        }
        ObjectKind::External => Some("chart or plug-in object: only its styling drawn"),
        ObjectKind::Unknown(_) => Some("unknown object kind: not drawn"),
        _ => None,
    }
}

/// A function a formula calls: a function, a `Get` selector, a built-in
/// plug-in function (Design, mobile), or a form with a node of its own
/// (`Case`, `Choose`, `Let`, `While`, `Evaluate`, `GetField`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Callee {
    Function(u16),
    Get(u8),
    Plugin([u8; 4], u16),
    Form(&'static str),
}

/// The functions the file's formulas call: for each, its name, the call
/// alone with empty arguments, and where it is called.
#[derive(Default)]
struct Uses(BTreeMap<Callee, (String, om_data::formula::Expr, BTreeSet<String>)>);

impl Uses {
    fn add(&mut self, callee: Callee, name: String, call: om_data::formula::Expr, place: &str) {
        self.0
            .entry(callee)
            .or_insert_with(|| (name, call, BTreeSet::new()))
            .2
            .insert(place.to_owned());
    }

    /// Every slot calculation of a step (a button's single step included).
    fn step(&mut self, step: &ScriptStep, place: &str) {
        let slots: Vec<u16> = step
            .data
            .iter()
            .flat_map(|data| data.slots().map(|(slot, _)| slot))
            .collect();
        for slot in slots {
            let formula = step
                .calculation(slot)
                .and_then(|calc| calc.polish)
                .and_then(|polish| om_data::formula::Formula::decode(None, None, &polish).ok());
            if let Some(formula) = formula {
                self.walk(&formula.polish, place);
            }
        }
    }

    /// Record every call in `expr`, its arguments' included.
    fn walk(&mut self, expr: &om_data::formula::Expr, place: &str) {
        use om_data::formula::catalog::{self, Kind};
        use om_data::formula::{Expr, Text};
        let empty = |n: usize| {
            vec![
                Expr::Text(Text {
                    value: String::new(),
                    style: None,
                });
                n
            ]
        };
        let text = || {
            Expr::Text(Text {
                value: String::new(),
                style: None,
            })
        };
        // Walked without recursion: a formula's depth comes from the file.
        let mut pending = vec![expr];
        while let Some(expr) = pending.pop() {
            match expr {
                Expr::Call { op, args } => {
                    if let Some(info) = op.info().filter(|i| i.kind == Kind::Function) {
                        let call = Expr::Call {
                            op: *op,
                            args: empty(args.len()),
                        };
                        self.add(Callee::Function(op.0), info.name.to_owned(), call, place);
                    }
                    pending.extend(args);
                }
                Expr::Get(selector) => {
                    let name = catalog::get_selector_name(*selector).unwrap_or("?");
                    let call = Expr::Get(*selector);
                    self.add(
                        Callee::Get(*selector),
                        format!("Get ( {name} )"),
                        call,
                        place,
                    );
                }
                Expr::PluginCall { function, args } => {
                    let name = catalog::builtin(*function).map_or_else(
                        || format!("plug-in function {:?}", function.plugin),
                        |b| b.name.to_owned(),
                    );
                    let call = Expr::PluginCall {
                        function: *function,
                        args: empty(args.len()),
                    };
                    let callee = Callee::Plugin(function.plugin, function.function);
                    self.add(callee, name, call, place);
                    pending.extend(args);
                }
                Expr::List(items) => pending.extend(items),
                Expr::Case(pairs) => {
                    let call = Expr::Case(vec![(text(), text())]);
                    self.add(Callee::Form("Case"), "Case".into(), call, place);
                    pending.extend(pairs.iter().flat_map(|(a, b)| [a, b]));
                }
                Expr::Choose { test, results } => {
                    let call = Expr::Choose {
                        test: Box::new(text()),
                        results: vec![text()],
                    };
                    self.add(Callee::Form("Choose"), "Choose".into(), call, place);
                    pending.push(test);
                    pending.extend(results);
                }
                Expr::Let { bindings, body } => {
                    let call = Expr::Let {
                        bindings: Vec::new(),
                        body: Box::new(text()),
                    };
                    self.add(Callee::Form("Let"), "Let".into(), call, place);
                    pending.extend(bindings);
                    pending.push(body);
                }
                Expr::While {
                    initial,
                    condition,
                    logic,
                    result,
                } => {
                    let call = Expr::While {
                        initial: Vec::new(),
                        condition: Box::new(text()),
                        logic: Vec::new(),
                        result: Box::new(text()),
                    };
                    self.add(Callee::Form("While"), "While".into(), call, place);
                    pending.extend(initial);
                    pending.extend(logic);
                    pending.push(condition);
                    pending.push(result);
                }
                Expr::Evaluate { args, .. } => {
                    let call = Expr::Evaluate {
                        args: empty(args.len()),
                        need_field_ref: 0,
                    };
                    self.add(Callee::Form("Evaluate"), "Evaluate".into(), call, place);
                    pending.extend(args);
                }
                Expr::GetField { arg, .. } => {
                    let call = Expr::GetField {
                        arg: Box::new(text()),
                        need_field_ref: 0,
                    };
                    self.add(Callee::Form("GetField"), "GetField".into(), call, place);
                    pending.push(arg);
                }
                Expr::CustomCall { args, .. } => pending.extend(args),
                _ => {}
            }
        }
    }

    /// Each call alone, with empty arguments, through the calculation host
    /// Browse uses, over no records: "not supported" or an unknown function
    /// means no implementation; any other answer, an error included, means
    /// one.
    fn cards(self, schema: &CalcSchema) -> Vec<FunctionCard> {
        use om_data::calc::CalcError;
        let source = om_data::host::MemorySource::default();
        let session = om_data::host::SessionInfo::default();
        self.0
            .into_values()
            .map(|(name, call, used_in)| {
                let file =
                    om_data::host::FileCalc::new(schema, &source, om_data::host::session::today(0));
                let (mut locals, mut globals) = Default::default();
                let mut host =
                    om_data::host::FileHost::new(file, &session, &mut locals, &mut globals);
                // `Self` reads the value its object or field supplies.
                let context = om_data::calc::eval::Context {
                    self_value: Some(om_data::calc::Value::empty_text()),
                    ..Default::default()
                };
                let evaluated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    om_data::calc::eval::evaluate(&call, &mut host, &context)
                }));
                let error = match evaluated {
                    Ok(evaluation) => evaluation
                        .error
                        .filter(|e| {
                            *e == CalcError::NOT_SUPPORTED || *e == CalcError::FUNCTION_NOT_FOUND
                        })
                        .map(|e| e.code()),
                    // A panic is a fault to file, not a missing function.
                    Err(_) => Some(-1),
                };
                FunctionCard {
                    name,
                    implemented: error.is_none(),
                    error,
                    used_in,
                }
            })
            .collect()
    }
}

/// The script host capabilities the desktop host (`WorkerHost`) leaves to
/// the runtime's default, error 3 "Command is unavailable". [`Desktop::read`]
/// checks this list against the sources it was built from.
const PROBED: [&str; 16] = [
    "undo_redo",
    "save_a_copy_as",
    "set_multi_user",
    "set_use_system_formats",
    "get_folder_path",
    "execute_data_api",
    "add_account",
    "delete_account",
    "reset_account_password",
    "enable_account",
    "change_password",
    "re_login",
    "dial_phone",
    "install_menu_set",
    "allow_formatting_bar",
    "install_plugin_file",
];

/// What the desktop host does not answer: the step ids its `other_step`
/// answers, and the client its runtime stands for.
struct Desktop {
    other_steps: BTreeSet<u16>,
    client: om_data::scripts::Client,
}

impl Desktop {
    /// Read the desktop host's capabilities from the sources this test was
    /// built from: the script host traits' methods whose default is
    /// `HostError::Unsupported`, less those `WorkerHost` implements, must
    /// be [`PROBED`], so that [`Probe`] records exactly them.
    fn read() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |path: &Path| {
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        };
        let traits = read(&root.join("../om-scripts/src/runtime/host.rs"));
        let mut implemented = BTreeSet::new();
        let mut other_steps = BTreeSet::new();
        for path in rust_files(&root.join("src")) {
            let text = read(&path);
            for block in impl_blocks(&text, " for WorkerHost") {
                implemented.extend(methods(block).into_iter().map(|(name, _)| name));
                for (name, body) in methods(block) {
                    if name == "other_step" {
                        other_steps.extend(body.lines().filter_map(|line| {
                            line.trim().split_once(" =>")?.0.parse::<u16>().ok()
                        }));
                    }
                }
            }
        }
        let defaults: BTreeSet<String> = impl_blocks(&traits, "pub trait ")
            .into_iter()
            .flat_map(methods)
            .filter(|(_, body)| body.contains("Err(HostError::Unsupported)"))
            .map(|(name, _)| name)
            .collect();
        assert!(
            implemented.len() > 50 && defaults.len() > 50,
            "the host sources were not read"
        );
        let missing: BTreeSet<String> = defaults.difference(&implemented).cloned().collect();
        let probed: BTreeSet<String> = PROBED.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            missing, probed,
            "the desktop host's unimplemented capabilities changed: update PROBED and Probe"
        );
        assert!(
            !other_steps.is_empty(),
            "WorkerHost::other_step was not read"
        );
        Desktop {
            other_steps,
            client: crate::scripting::host::options().client,
        }
    }

    /// What the desktop does with `step`: it runs alone in the desktop's
    /// runtime against [`Probe`], as a button's single step or as a script
    /// of one step; a step that reaches a capability the desktop host lacks
    /// fails with error 3.
    fn status(&self, step: &ScriptStep, button: bool) -> (&'static str, Option<String>) {
        let id = step.id();
        if !om_data::scripts::is_valid_step_id(id)
            || om_data::scripts::OBSOLETE_STEP_IDS.contains(&id)
        {
            return ("unknown", Some("not a step FileMaker Pro 26 runs".into()));
        }
        if om_data::scripts::PENDING_STEP_IDS.contains(&id) {
            return (
                "error3",
                Some("a FileMaker Pro 26.1.1 step OpenMaker cannot run yet".into()),
            );
        }
        if !om_data::scripts::is_supported(id, self.client) {
            return (
                "filemaker",
                Some("FileMaker Pro has no handler for it either (error 3)".into()),
            );
        }
        // The stored step, and the step with no options: a calculation the
        // probe cannot evaluate must not end the step before its host call.
        let mut bare = step.clone();
        bare.data = None;
        bare.flags = 0;
        for step in [step.clone(), bare] {
            let mut probe = Probe::default();
            let mut options = crate::scripting::host::options();
            options.step_budget = Some(1000);
            let mut runtime = om_data::runtime::Runtime::new(options);
            let started = if button {
                runtime.perform_step(&mut probe, step, None)
            } else {
                probe.script = Some(Arc::new(om_data::Script {
                    id: 1,
                    name: "Probe".into(),
                    steps: vec![step],
                    ..om_data::Script::default()
                }));
                runtime.perform_script(&mut probe, om_data::runtime::ScriptCall::new(0, 1))
            };
            if started.is_err() {
                continue;
            }
            for _ in 0..100 {
                if runtime.run(&mut probe) != om_data::runtime::RunState::Yielded {
                    break;
                }
            }
            if let Some(why) = probe.reached.first() {
                return ("error3", Some(why.clone()));
            }
            if probe.other.contains(&id) && !self.other_steps.contains(&id) {
                return (
                    "error3",
                    Some("the desktop host answers no such step (`other_step`)".into()),
                );
            }
        }
        ("implemented", None)
    }
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files
}

/// The bodies of top-level items whose first line contains `marker`, up
/// to their closing brace at the start of a line (rustfmt's layout).
fn impl_blocks<'s>(text: &'s str, marker: &str) -> Vec<&'s str> {
    let mut blocks = Vec::new();
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if start.is_none()
            && !line.starts_with(' ')
            && line.contains(marker)
            && line.trim_end().ends_with('{')
        {
            start = Some(offset + line.len());
        } else if let Some(from) = start.filter(|_| line.trim_end() == "}") {
            blocks.push(&text[from..offset]);
            start = None;
        }
        offset += line.len();
    }
    blocks
}

/// The methods of an impl or trait body, with their bodies: each `fn` at
/// the block's indentation up to its closing brace there (a one-line body
/// is its own line).
fn methods(block: &str) -> Vec<(String, &str)> {
    let mut methods = Vec::new();
    let mut current: Option<(String, usize)> = None;
    let mut offset = 0;
    for line in block.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("    fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if line.trim_end().ends_with('}') {
                methods.push((name, &block[offset..offset + line.len()]));
            } else {
                current = Some((name, offset));
            }
        } else if line.trim_end() == "    }" {
            if let Some((name, from)) = current.take() {
                methods.push((name, &block[from..offset + line.len()]));
            }
        }
        offset += line.len();
    }
    methods
}

/// A script host with only the capabilities in [`PROBED`], each recording
/// that a step reached it; `other_step`; and `open_menu_item` and
/// `show_hide`, which the desktop host has for some steps only: they refuse
/// the steps it refuses, by its own decision. Everything else is the
/// runtime's default.
struct Probe {
    calc: om_data::calc::eval::NullHost,
    /// The script a script step is probed in.
    script: Option<Arc<om_data::Script>>,
    /// Why a step reached a capability the desktop host lacks for it.
    reached: Vec<String>,
    other: Vec<u16>,
}

impl Default for Probe {
    fn default() -> Self {
        Probe {
            calc: om_data::calc::eval::NullHost::new(
                om_data::calc::Locale::en_us(),
                om_data::host::session::today(0),
            ),
            script: None,
            reached: Vec::new(),
            other: Vec::new(),
        }
    }
}

impl Probe {
    fn reach(&mut self, method: &'static str) -> HostResult {
        self.reached
            .push(format!("the desktop host has no `{method}` capability"));
        Err(HostError::Unsupported)
    }

    /// A capability the desktop host has for other steps than this one.
    fn refuse(&mut self, method: &'static str) -> HostResult {
        self.reached
            .push(format!("the desktop host's `{method}` refuses this step"));
        Err(HostError::Unsupported)
    }
}

impl om_data::calc::eval::CalcHost for Probe {
    fn locale(&self) -> &om_data::calc::Locale {
        self.calc.locale()
    }
    fn today(&self) -> om_data::calc::Date {
        self.calc.today()
    }
    fn variables(
        &mut self,
        scope: om_data::calc::eval::VarScope,
    ) -> &mut om_data::calc::eval::Variables {
        self.calc.variables(scope)
    }
}

impl ScriptHost for Probe {
    fn script(&mut self, _file: u64, id: u64) -> Option<Arc<om_data::Script>> {
        self.script.clone().filter(|s| s.id == id)
    }
    fn other_step(&mut self, call: &StepCall<'_>) -> HostResult<Option<om_data::calc::Value>> {
        self.other.push(call.id);
        Err(HostError::Unsupported)
    }
}
impl Navigation for Probe {}
impl Records for Probe {}
impl Fields for Probe {
    fn undo_redo(&mut self, _choice: u16) -> HostResult {
        self.reach("undo_redo")
    }
}
impl FoundSets for Probe {}
impl Windows for Probe {
    fn show_hide(&mut self, bar: Bar, _visibility: Visibility, _lock: bool) -> HostResult {
        let mut window = crate::scripting::WindowState::default();
        match crate::scripting::host::bar_flag(&mut window, bar) {
            Some(_) => Ok(()),
            None => self.refuse("show_hide"),
        }
    }
}
impl Dialogs for Probe {
    fn open_menu_item(&mut self, step: u16) -> HostResult {
        if crate::scripting::host::OPENED_MENU_ITEMS.contains(&step) {
            Ok(())
        } else {
            self.refuse("open_menu_item")
        }
    }
}
impl Files for Probe {
    fn save_a_copy_as(&mut self, _paths: &[String], _kind: u16, _create: bool) -> HostResult {
        self.reach("save_a_copy_as")
    }
    fn set_multi_user(&mut self, _choice: u16) -> HostResult {
        self.reach("set_multi_user")
    }
    fn set_use_system_formats(&mut self, _on: bool) -> HostResult {
        self.reach("set_use_system_formats")
    }
    fn get_folder_path(
        &mut self,
        _title: &str,
        _default: &str,
        _allow_create: bool,
    ) -> HostResult<om_data::calc::Value> {
        self.reach("get_folder_path")?;
        unreachable!()
    }
}
impl Network for Probe {
    fn execute_data_api(&mut self, _request: &str) -> HostResult<om_data::calc::Value> {
        self.reach("execute_data_api")?;
        unreachable!()
    }
}
impl Accounts for Probe {
    fn add_account(
        &mut self,
        _name: &str,
        _password: &str,
        _privilege_set: &[u8; 14],
        _expire: bool,
    ) -> HostResult {
        self.reach("add_account")
    }
    fn delete_account(&mut self, _name: &str) -> HostResult {
        self.reach("delete_account")
    }
    fn reset_account_password(
        &mut self,
        _name: &str,
        _password: &str,
        _expire: bool,
    ) -> HostResult {
        self.reach("reset_account_password")
    }
    fn enable_account(&mut self, _name: &str, _active: bool) -> HostResult {
        self.reach("enable_account")
    }
    fn change_password(&mut self, _old: &str, _new: &str, _with_dialog: bool) -> HostResult {
        self.reach("change_password")
    }
    fn re_login(&mut self, _account: &str, _password: &str, _with_dialog: bool) -> HostResult {
        self.reach("re_login")
    }
}
impl Platform for Probe {
    fn dial_phone(&mut self, _number: &str, _with_dialog: bool) -> HostResult {
        self.reach("dial_phone")
    }
    fn install_menu_set(&mut self, _menu_set: Option<u64>, _file_default: bool) -> HostResult {
        self.reach("install_menu_set")
    }
    fn allow_formatting_bar(&mut self, _on: bool) -> HostResult {
        self.reach("allow_formatting_bar")
    }
    fn install_plugin_file(&mut self, _field: Option<FieldRef>) -> HostResult {
        self.reach("install_plugin_file")
    }
}

/// Group messages that differ only by object ids and other decimal
/// numbers; hexadecimal option bits (`0x...`) stay.
fn normalize(message: &str) -> String {
    message
        .split(' ')
        .map(|word| {
            if word.starts_with("0x") {
                return word.to_owned();
            }
            let mut out = String::with_capacity(word.len());
            for c in word.chars() {
                if !c.is_ascii_digit() {
                    out.push(c);
                } else if !out.ends_with('N') {
                    out.push('N');
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reasons ranked by how many things (tables, layouts, scripts) each
/// blocks, most first.
fn ranked(reasons: BTreeMap<String, BTreeSet<String>>) -> Vec<(String, BTreeSet<String>)> {
    let mut ranked: Vec<_> = reasons.into_iter().collect();
    ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    ranked
}

fn summary(cards: &[FileCard]) -> String {
    use std::fmt::Write;
    let mut md = String::new();
    let opened = cards.iter().filter(|c| c.open.opened).count();
    let _ = writeln!(md, "## Totals\n");
    let _ = writeln!(md, "| Criterion | Measure | Result |\n|---|---|---|");
    let _ = writeln!(
        md,
        "| 1 Open | files that open with the account they suggest, empty password | {opened} of {} |",
        cards.len()
    );
    // Criterion 4.
    let tables: Vec<_> = cards
        .iter()
        .flat_map(|c| c.tables.iter().map(move |t| (c, t)))
        .collect();
    let writable = tables.iter().filter(|(_, t)| t.writable).count();
    let _ = writeln!(
        md,
        "| 4 Edit | base tables the record planner accepts | {writable} of {} |",
        tables.len()
    );
    let objects: Vec<_> = cards
        .iter()
        .flat_map(|c| c.layouts.iter().map(move |l| (c, l)))
        .flat_map(|(c, l)| l.fields.iter().map(move |f| (c, l, f)))
        .collect();
    let editable_in_fm: Vec<_> = objects
        .iter()
        .filter(|(_, _, f)| f.filemaker_edits)
        .collect();
    let editable = editable_in_fm.iter().filter(|(_, _, f)| f.editable).count();
    let _ = writeln!(
        md,
        "| 4 Edit | field objects FileMaker lets the user change that Browse lets the user change | {editable} of {} ({} more FileMaker does not let the user change) |",
        editable_in_fm.len(),
        objects.len() - editable_in_fm.len()
    );
    // Criterion 5.
    let steps: Vec<_> = cards
        .iter()
        .flat_map(|c| c.steps.iter().map(move |s| (c, s)))
        .collect();
    let mut by_id: BTreeMap<u16, (&StepCard, usize, BTreeSet<String>)> = BTreeMap::new();
    for (card, step) in &steps {
        let entry = by_id.entry(step.id).or_insert((step, 0, BTreeSet::new()));
        if step.status != "implemented" {
            entry.0 = step;
        }
        entry.1 += step.buttons;
        entry
            .2
            .extend(step.scripts.iter().map(|s| format!("{}: {s}", card.file)));
    }
    let failing = |status: &str| {
        by_id
            .values()
            .filter(|(s, _, _)| s.status == status)
            .count()
    };
    let scripts: usize = cards.iter().map(|c| c.scripts).sum();
    let blocked_scripts: BTreeSet<&String> = by_id
        .values()
        .filter(|(s, _, _)| s.status == "error3" || s.status == "unknown")
        .flat_map(|(_, _, scripts)| scripts)
        .collect();
    let _ = writeln!(
        md,
        "| 5 Script | distinct steps used: implemented / error 3 in OpenMaker / not in FileMaker Pro either | {} / {} / {} of {} |",
        failing("implemented"),
        failing("error3") + failing("unknown"),
        failing("filemaker"),
        by_id.len()
    );
    let _ = writeln!(
        md,
        "| 5 Script | scripts with a step that returns error 3 in OpenMaker only | {} of {scripts} |",
        blocked_scripts.len()
    );
    // Criterion 6.
    let layouts: Vec<_> = cards
        .iter()
        .flat_map(|c| c.layouts.iter().map(move |l| (c, l)))
        .collect();
    let accepted = layouts.iter().filter(|(_, l)| l.report.accepted).count();
    let _ = writeln!(
        md,
        "| 6 Output | layouts the report builder accepts for Preview and Save Records as PDF | {accepted} of {} |",
        layouts.len()
    );

    // Criterion 2.
    let all_objects: usize = layouts.iter().flat_map(|(_, l)| l.objects.values()).sum();
    let gaps: usize = layouts.iter().flat_map(|(_, l)| l.browse.values()).sum();
    let gap_layouts = layouts.iter().filter(|(_, l)| !l.browse.is_empty()).count();
    let _ = writeln!(
        md,
        "| 2 Browse | objects Browse does not draw or open as FileMaker does | {gaps} of {all_objects}, on {gap_layouts} of {} layouts |",
        layouts.len()
    );
    // Calculations.
    let mut functions: BTreeMap<&str, (bool, BTreeSet<String>)> = BTreeMap::new();
    for card in cards {
        for function in &card.functions {
            let entry = functions
                .entry(&function.name)
                .or_insert((true, BTreeSet::new()));
            entry.0 &= function.implemented;
            entry.1.extend(
                function
                    .used_in
                    .iter()
                    .map(|place| format!("{}: {place}", card.file)),
            );
        }
    }
    let missing_functions: Vec<_> = functions.iter().filter(|(_, (ok, _))| !ok).collect();
    let _ = writeln!(
        md,
        "| Calculations | functions, `Get` selectors and Design functions called, with no implementation | {} of {} |",
        missing_functions.len(),
        functions.len()
    );

    // Criterion 7 through the desktop's developer tools: each step, by
    // the files where it held after reopening.
    let mut build: Vec<(&str, &str, usize, Vec<String>)> = Vec::new();
    for card in cards {
        for step in &card.build.steps {
            let at = match build.iter().position(|b| b.0 == step.step) {
                Some(at) => at,
                None => {
                    build.push((step.step, step.made_by, 0, Vec::new()));
                    build.len() - 1
                }
            };
            if step.done {
                build[at].2 += 1;
            } else {
                build[at].3.push(format!("{}: {}", card.file, step.detail));
            }
        }
    }
    let built = cards
        .iter()
        .filter(|c| c.build.error.is_none() && c.build.steps.iter().all(|s| s.done))
        .count();
    let _ = writeln!(
        md,
        "| 7 Build | files where every step of building through the desktop's developer tools holds after reopening | {built} of {} |",
        cards.len()
    );
    let build_section = {
        let mut out = String::from(
            "\n## Build: the desktop's developer tools, each step closed and reopened\n\n| Step | Made by | Files | First failure |\n|---|---|---:|---|\n",
        );
        for (step, made_by, done, failures) in &build {
            let _ = writeln!(
                out,
                "| {step} | {made_by} | {done} of {} | {} |",
                cards.len(),
                failures
                    .first()
                    .map_or("", String::as_str)
                    .replace('|', "\\|")
            );
        }
        for card in cards.iter().filter(|c| c.build.error.is_some()) {
            let _ = writeln!(
                out,
                "| (stopped) | | | {}: {} |",
                card.file,
                card.build.error.as_deref().unwrap_or("")
            );
        }
        out
    };
    let trust: Vec<_> = cards
        .iter()
        .flat_map(|c| c.trust.tables.iter().map(move |t| (c, t)))
        .collect();
    let stopped: Vec<_> = cards.iter().filter(|c| c.trust.error.is_some()).collect();
    let _ = writeln!(
        md,
        "| 8 Trust | base tables whose records, written as Browse writes them (its layout or a portal), survive close and reopen as written | {} of {}{} |",
        trust.iter().filter(|(_, t)| t.passed).count(),
        tables.len(),
        if stopped.is_empty() {
            String::new()
        } else {
            format!(" ({} files stopped short)", stopped.len())
        }
    );

    let _ = writeln!(
        md,
        "\n## Trust: tables whose records did not survive as written\n"
    );
    let _ = writeln!(
        md,
        "| Table | Reached through | What failed |\n|---|---|---|"
    );
    for (card, table) in trust.iter().filter(|(_, t)| !t.passed) {
        let _ = writeln!(
            md,
            "| {}: {} | {} | {} |",
            card.file,
            table.table,
            table.route,
            table.error.as_deref().unwrap_or("").replace('|', "\\|")
        );
    }
    for card in &stopped {
        let _ = writeln!(
            md,
            "| {} | - | stopped: {} |",
            card.file,
            card.trust
                .error
                .as_deref()
                .unwrap_or("")
                .replace('|', "\\|")
        );
    }
    for card in cards.iter().filter(|c| c.trust.reopen_wait_ms.is_some()) {
        let _ = writeln!(
            md,
            "\n{}: reopening waited {} ms for the closed file to be released.",
            card.file,
            card.trust.reopen_wait_ms.unwrap_or_default()
        );
    }
    let written: Vec<_> = cards
        .iter()
        .filter_map(|c| c.trust.written.as_ref())
        .collect();
    if let Some(first) = written.first() {
        let _ = writeln!(
            md,
            "\nThe {} written files are in `{}`.",
            written.len(),
            first.parent().unwrap_or(first).display()
        );
    }

    md.push_str(&build_section);

    let _ = writeln!(md, "\n## Edit: why base tables are read-only\n");
    let mut reasons: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (card, table) in &tables {
        for refusal in &table.refusals {
            reasons
                .entry(refusal.reason.clone())
                .or_default()
                .insert(format!("{}: {}", card.file, table.name));
        }
    }
    let _ = writeln!(md, "| Reason | Tables | Which |\n|---|---:|---|");
    for (reason, which) in ranked(reasons) {
        let _ = writeln!(md, "| {reason} | {} | {} |", which.len(), list(&which, 6));
    }

    let _ = writeln!(
        md,
        "\n## Edit: why Browse does not let the user change a field object FileMaker does\n"
    );
    let mut classes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (card, layout, field) in &editable_in_fm {
        if let Some(class) = field.class.filter(|_| !field.editable) {
            classes
                .entry(class.into())
                .or_default()
                .insert(format!("{}: {}", card.file, layout.name));
            *counts.entry(class.into()).or_default() += 1;
        }
    }
    let _ = writeln!(md, "| Class | Layouts | Objects |\n|---|---:|---:|");
    for (class, which) in ranked(classes) {
        let _ = writeln!(md, "| {class} | {} | {} |", which.len(), counts[&class]);
    }

    let _ = writeln!(md, "\n## Script: steps that return error 3 in OpenMaker\n");
    let _ = writeln!(
        md,
        "| Step | Scripts | Buttons | Why |\n|---|---:|---:|---|"
    );
    let mut missing: Vec<_> = by_id
        .values()
        .filter(|(s, _, _)| s.status == "error3" || s.status == "unknown")
        .collect();
    missing.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then(b.1.cmp(&a.1)));
    for (step, buttons, scripts) in missing {
        let _ = writeln!(
            md,
            "| {} ({}) | {} | {buttons} | {} |",
            step.name,
            step.id,
            scripts.len(),
            step.why.as_deref().unwrap_or("")
        );
    }

    let _ = writeln!(md, "\n## Output: why the report builder refuses a layout\n");
    let mut reasons: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (card, layout) in &layouts {
        for refusal in &layout.report.refusals {
            reasons
                .entry(normalize(refusal))
                .or_default()
                .insert(format!("{}: {}", card.file, layout.name));
        }
    }
    let _ = writeln!(md, "| Refusal | Layouts |\n|---|---:|");
    for (reason, which) in ranked(reasons) {
        let _ = writeln!(md, "| {reason} | {} |", which.len());
    }

    let _ = writeln!(
        md,
        "\n## Edit: portals (rows, and the creation row where the relationship allows creation)\n"
    );
    let _ = writeln!(
        md,
        "| Layout | Portal | Fields editable in rows | Why not | Creation row | Fields editable in it | Why not |\n|---|---|---:|---|---|---:|---|"
    );
    for (card, layout) in &layouts {
        let mut portals: BTreeMap<&str, Vec<&FieldCard>> = BTreeMap::new();
        for field in &layout.fields {
            if let Some(row) = &field.creation_row {
                portals.entry(&row.portal).or_default().push(field);
            }
        }
        for (portal, fields) in portals {
            let edited: Vec<_> = fields.iter().filter(|f| f.filemaker_edits).collect();
            let allowed = fields
                .iter()
                .any(|f| f.creation_row.as_ref().is_some_and(|r| r.allowed));
            let mut why: BTreeMap<&str, usize> = BTreeMap::new();
            for field in edited.iter().filter(|f| !f.editable) {
                *why.entry(field.class.unwrap_or("other")).or_default() += 1;
            }
            let mut why_new: BTreeMap<&str, usize> = BTreeMap::new();
            for field in &edited {
                if let Some(why) = field.creation_row.as_ref().and_then(|r| r.why) {
                    *why_new.entry(why).or_default() += 1;
                }
            }
            let list = |why: BTreeMap<&str, usize>| {
                let why: Vec<String> = why.iter().map(|(c, n)| format!("{c} ({n})")).collect();
                why.join(", ")
            };
            let _ = writeln!(
                md,
                "| {}: {} | {portal} | {} of {} | {} | {} | {} | {} |",
                card.file,
                layout.name,
                edited.iter().filter(|f| f.editable).count(),
                edited.len(),
                list(why),
                if allowed { "shown" } else { "none" },
                if allowed {
                    format!(
                        "{} of {}",
                        edited
                            .iter()
                            .filter(|f| f.creation_row.as_ref().is_some_and(|r| r.editable))
                            .count(),
                        edited.len()
                    )
                } else {
                    "-".to_owned()
                },
                list(why_new),
            );
        }
    }

    let _ = writeln!(
        md,
        "\n## Browse: what Browse does not draw or open as FileMaker does\n"
    );
    let mut gaps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut gap_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (card, layout) in &layouts {
        for (gap, n) in &layout.browse {
            gaps.entry(gap.to_string())
                .or_default()
                .insert(format!("{}: {}", card.file, layout.name));
            *gap_counts.entry(gap.to_string()).or_default() += n;
        }
    }
    let _ = writeln!(md, "| What | Layouts | Objects |\n|---|---:|---:|");
    for (gap, which) in ranked(gaps) {
        let _ = writeln!(md, "| {gap} | {} | {} |", which.len(), gap_counts[&gap]);
    }

    let _ = writeln!(md, "\n## Calculations: functions with no implementation\n");
    let _ = writeln!(
        md,
        "| Function | Formulas' places | Where |\n|---|---:|---|"
    );
    let mut missing_functions: Vec<_> = missing_functions
        .into_iter()
        .map(|(name, (_, places))| (name.to_string(), places.clone()))
        .collect();
    missing_functions.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    for (name, places) in missing_functions {
        let _ = writeln!(md, "| {name} | {} | {} |", places.len(), list(&places, 4));
    }

    let _ = writeln!(md, "\n## Per file\n");
    let _ = writeln!(
        md,
        "| File | Opens | Writable tables | Field objects editable (of those FileMaker edits) | Steps error 3 | Layouts accepted for output |\n|---|---|---:|---:|---:|---:|"
    );
    for card in cards {
        let fields: Vec<_> = card
            .layouts
            .iter()
            .flat_map(|l| &l.fields)
            .filter(|f| f.filemaker_edits)
            .collect();
        let _ = writeln!(
            md,
            "| {} | {} | {} of {} | {} of {} | {} of {} | {} of {} |",
            card.file,
            match (&card.open.opened, &card.open.error) {
                (true, _) => "yes".to_owned(),
                (false, Some(error)) => format!("no: {error}"),
                (false, None) => "no".to_owned(),
            },
            card.tables.iter().filter(|t| t.writable).count(),
            card.tables.len(),
            fields.iter().filter(|f| f.editable).count(),
            fields.len(),
            card.steps
                .iter()
                .filter(|s| s.status == "error3" || s.status == "unknown")
                .count(),
            card.steps.len(),
            card.layouts.iter().filter(|l| l.report.accepted).count(),
            card.layouts.len(),
        );
    }
    md
}

/// Up to `n` names, then how many more.
fn list(names: &BTreeSet<String>, n: usize) -> String {
    let mut shown: Vec<&str> = names.iter().take(n).map(String::as_str).collect();
    let more = names.len().saturating_sub(n);
    let more = format!("and {more} more");
    if names.len() > n {
        shown.push(&more);
    }
    shown.join(", ")
}
