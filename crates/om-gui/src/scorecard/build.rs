//! Building a solution through the desktop's developer tools (criterion 7,
//! the desktop route): on a fresh copy of each file, each step through the
//! provider operation the Script Workspace, Layout mode or Manage Database
//! calls, then the file closed and opened again and the step checked there:
//!
//! - Script Workspace (#611, #619): New Script, rename it, add a step and
//!   save (`save_script_catalog`, `save_script`); duplicate, move and
//!   delete a script (the catalog saved as the Scripts pane commits it);
//! - Layout mode (#615, #614, #626, #629): New Layout / Report
//!   (`create_layout`); on it, a button that performs the new script and
//!   one with a single step (`place_layout_button`); a stored object of the
//!   file's first layout moved and resized (`set_layout_object_frames`);
//! - Manage Database, Options for Field (#601, #618): a new text field
//!   given a calculated value, and another a lookup through a relationship
//!   of its table (`apply_schema_change`, `commit_schema_changes`);
//! - later (`later_steps`): Duplicate Layout and Delete Layout of the copy
//!   (#671, #677); a text and a field with its label on the new layout
//!   (#666); a stored object of the first layout deleted (#641); Duplicate
//!   field (#646); a lookup from the first table, a parent matching on its
//!   auto-entered UUID key (#678); global storage on the first plain text
//!   field no other definition uses (#635);
//! - Layout mode's setup (`setup_steps`): Layout Setup's name, views and
//!   General options on the new layout (`set_layout_setup`; #714, #720,
//!   #817), its width (`set_layout_width`; #766), the first drop-down list
//!   or pop-up menu with a value list, or date field, of the file's layouts
//!   shown with another control (the Inspector's Control Style,
//!   `set_field_control`; #827),
//!   and a part's page options
//!   (Part Setup, `set_part_options`; #847, #855).
//! - File > Create New (#712, #724; [`create_new`], once a run): a blank
//!   file made by the command's operation, a text field added and placed
//!   on its layout, two records with a value in it, read back after
//!   reopening.
//!
//! The Script Workspace's own catalog edits (`catalog_pane`) are repeated
//! here as they make the catalog: a new member gets the next id, a copy a
//! new id, " Copy" and a fresh UUID. What the window does around them
//! (dialogs, selection, the canvas) is not exercised, and nothing checks
//! that FileMaker Pro opens the result.

use std::path::Path;

use om_data::field_options::{AutoCalculation, AutoEnterValue, Lookup, NoMatch, OptionFormula};
use om_data::layout_catalog::LayoutRequest;
use om_data::layout_objects::{LayoutObjectFrames, ObjectFrame};
use om_data::layout_placement::{ButtonAction, ButtonPlacement, LayoutButtonPlacement};
use om_data::scripts::display::{parse_steps, NoStepNames, Origin, Pasted};
use om_data::{
    DataProvider, DatabaseSchema, FieldKind, SchemaChange, SchemaFieldKind, Script, ScriptCatalog,
    ScriptKind, ScriptStep,
};
use om_layout::Rect;
use serde::Serialize;

#[derive(Serialize, Default)]
pub(super) struct BuildCard {
    pub steps: Vec<BuildStep>,
    /// Why the measure stopped short, if it did.
    pub error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct BuildStep {
    pub step: &'static str,
    /// The pull requests that made it.
    pub made_by: &'static str,
    pub done: bool,
    /// What was checked after reopening, or the first error.
    pub detail: String,
}

/// What the steps work on: the file's first layout and its table.
struct Context {
    layout: u32,
    occurrence: u64,
    table: u64,
}

/// What earlier steps made, for later ones.
#[derive(Default)]
struct Made {
    script: Option<u64>,
    copy: Option<u64>,
    layout: Option<u32>,
}

pub(super) fn open(copy: &Path) -> Result<Box<dyn DataProvider>, String> {
    // The file is opened again as soon as its host has let go of it
    // (`trust`'s reopening), within 30 s.
    let started = std::time::Instant::now();
    loop {
        let mut provider: Box<dyn DataProvider> = om_provider::NativeProvider::from_env()
            .map(|p| Box::new(p) as Box<dyn DataProvider>)
            .map_err(|e| e.message)?;
        match super::open(&mut *provider, copy) {
            Ok(_) => return Ok(provider),
            Err(e)
                if e.contains("already open")
                    && started.elapsed() < std::time::Duration::from_secs(30) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

pub(super) fn panic_text(panic: Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "a panic".into())
}

/// One step: `act` on the file opened, then the file closed, opened
/// again and `check`ed; the row, and what `act` made.
fn run<T>(
    card: &mut BuildCard,
    copy: &Path,
    (step, made_by): (&'static str, &'static str),
    act: impl FnOnce(&mut dyn DataProvider) -> Result<T, String>,
    check: impl FnOnce(&mut dyn DataProvider, &T) -> Result<String, String>,
) -> Option<T> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut provider = open(copy).map_err(|e| format!("opening: {e}"))?;
        let made = act(&mut *provider);
        provider.close();
        drop(provider);
        let made = made?;
        let mut provider = open(copy).map_err(|e| format!("reopening: {e}"))?;
        let checked = check(&mut *provider, &made);
        provider.close();
        checked.map(|detail| (made, detail))
    }))
    .map_err(|p| format!("panicked: {}", panic_text(p)))
    .and_then(|r| r);
    match result {
        Ok((made, detail)) => {
            card.steps.push(BuildStep {
                step,
                made_by,
                done: true,
                detail,
            });
            Some(made)
        }
        Err(detail) => {
            card.steps.push(BuildStep {
                step,
                made_by,
                done: false,
                detail,
            });
            None
        }
    }
}

fn one_step(text: &str) -> Result<ScriptStep, String> {
    match parse_steps(text, &NoStepNames, Origin::Unknown)?
        .into_iter()
        .next()
    {
        Some(Pasted::Step(step)) => Ok(step),
        Some(Pasted::Failed { reason, .. }) => Err(reason),
        None => Err(format!("“{text}” is no step")),
    }
}

fn script(catalog: &ScriptCatalog, id: u64) -> Result<&Script, String> {
    catalog
        .scripts
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("script {id} is not in the catalog"))
}

fn scripts(p: &mut dyn DataProvider) -> Result<ScriptCatalog, String> {
    p.scripts_for_editing().map_err(|e| e.message)
}

fn save(p: &mut dyn DataProvider, catalog: ScriptCatalog) -> Result<ScriptCatalog, String> {
    p.save_script_catalog(catalog).map_err(|e| e.message)
}

/// A UUID for a copied member, as the Script Workspace makes one.
fn fresh_uuid() -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::with_capacity(16);
    for _ in 0..2 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        out.extend(hasher.finish().to_be_bytes());
    }
    out
}

/// A frame `height` tall at the top of the layout's body part.
fn body_frame(p: &mut dyn DataProvider, layout: u32, nth: f64) -> Result<(Rect, [u8; 32]), String> {
    let geometry = p
        .layout_geometry(layout)
        .map_err(|e| e.message)?
        .ok_or("the layout has no geometry")?;
    let revision = geometry
        .revision
        .ok_or("the layout's revision is not known")?;
    let body = geometry
        .parts
        .iter()
        .find(|part| part.kind_name == "Body")
        .ok_or("the layout has no body part")?;
    let top = body.top + 8.0 + nth * 30.0;
    let frame = Rect {
        top,
        left: 20.0,
        bottom: top + 24.0,
        right: 140.0,
        ..Rect::default()
    };
    Ok((frame, revision))
}

/// The buttons of `layout`: each one's label text and step id.
fn buttons(p: &mut dyn DataProvider, layout: u32) -> Result<Vec<(String, u16)>, String> {
    let render = p
        .render_layout(layout)
        .map_err(|e| e.message)?
        .ok_or("the layout does not render")?;
    Ok(render
        .layout
        .objects
        .objects
        .iter()
        .filter_map(|o| {
            let step = o.script_step()?;
            Some((
                o.text.clone().unwrap_or_default(),
                u16::try_from(step.id).ok()?,
            ))
        })
        .collect())
}

fn schema_edit(
    p: &mut dyn DataProvider,
    change: impl FnOnce(&mut dyn DataProvider, DatabaseSchema) -> Result<(), String>,
) -> Result<(), String> {
    let schema = p.begin_schema_edit().map_err(|e| e.message)?;
    match change(p, schema) {
        Ok(()) => p.commit_schema_changes().map(|_| ()).map_err(|e| e.message),
        Err(e) => {
            let _ = p.cancel_schema_changes();
            Err(e)
        }
    }
}

/// A new text field `name` of `table`, given `options` by `set`.
fn field_with_options(
    p: &mut dyn DataProvider,
    table: u64,
    name: &str,
    set: impl FnOnce(&DatabaseSchema, &mut om_data::field_options::FieldOptions) -> Result<(), String>,
) -> Result<u64, String> {
    let mut id = 0;
    schema_edit(p, |p, _| {
        let schema = p
            .apply_schema_change(SchemaChange::CreateField {
                table_id: table,
                name: name.into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .map_err(|e| format!("Create: {}", e.message))?;
        let field = schema
            .table(table)
            .and_then(|t| t.fields.iter().find(|f| f.name == name))
            .cloned()
            .ok_or("the new field is not in the schema")?;
        let mut options = field.options.ok_or("the new field has no options")?;
        set(&schema, &mut options)?;
        id = field.id;
        p.apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: table,
            field_id: field.id,
            options: Box::new(options),
        })
        .map(|_| ())
        .map_err(|e| format!("Options for Field: {}", e.message))
    })?;
    Ok(id)
}

/// `field`'s auto-enter value as the schema reads it after reopening.
fn auto_enter(p: &mut dyn DataProvider, table: u64, field: u64) -> Result<AutoEnterValue, String> {
    let schema = p.database_schema().map_err(|e| e.message)?;
    schema
        .table(table)
        .and_then(|t| t.fields.iter().find(|f| f.id == field))
        .and_then(|f| f.options.clone())
        .map(|o| o.auto_enter.value)
        .ok_or_else(|| format!("field {field} or its options are gone"))
}

/// The measure on a copy of `original` in `dir`.
pub(super) fn card(original: &Path, dir: &Path) -> BuildCard {
    let mut card = BuildCard::default();
    let copy = dir.join("build.fmp12");
    if let Err(e) = std::fs::create_dir_all(dir).and_then(|()| std::fs::copy(original, &copy)) {
        card.error = Some(format!("The file could not be copied: {e}"));
        return card;
    }
    let context = (|| -> Result<Context, String> {
        let mut p = open(&copy)?;
        let schema = p
            .calc_schema()
            .map_err(|e| e.message)?
            .ok_or("no calculation schema")?;
        p.close();
        // The first layout of a table of this file (a layout of an external
        // occurrence, or none, has no table here).
        let (layout, occurrence, table) = schema
            .layouts()
            .iter()
            .find_map(|l| {
                let occurrence = l.occurrence?;
                Some((l.id, occurrence, schema.table_of(occurrence)?.id))
            })
            .ok_or("no layout of a table of this file")?;
        Ok(Context {
            layout,
            occurrence,
            table,
        })
    })();
    let context = match context {
        Ok(context) => context,
        Err(e) => {
            card.error = Some(e);
            return card;
        }
    };
    let mut made = Made::default();
    scripts_steps(&mut card, &copy, &mut made);
    layout_steps(&mut card, &copy, &context, &mut made);
    field_steps(&mut card, &copy, &context);
    later_steps(&mut card, &copy, &context, &made);
    setup_steps(&mut card, &copy, &made);
    card
}

fn scripts_steps(card: &mut BuildCard, copy: &Path, made: &mut Made) {
    const NAME: &str = "Scorecard Script";
    made.script = run(
        card,
        copy,
        ("Create a script (New Script), saved", "#611"),
        |p| {
            let mut catalog = scripts(p)?;
            let id = catalog.scripts.iter().map(|s| s.id).max().unwrap_or(0) + 1;
            catalog.scripts.push(Script {
                id,
                name: "New Script".into(),
                kind: ScriptKind::Script,
                ..Script::default()
            });
            let stored = save(p, catalog)?;
            stored
                .scripts
                .iter()
                .rev()
                .find(|s| s.name == "New Script" && s.kind == ScriptKind::Script)
                .map(|s| s.id)
                .ok_or_else(|| "the stored catalog has no new script".into())
        },
        |p, id| {
            Ok(format!(
                "“{}” (id {id}) is there",
                script(&scripts(p)?, *id)?.name
            ))
        },
    );
    let Some(id) = made.script else {
        return;
    };
    run(
        card,
        copy,
        ("Rename it", "#611"),
        |p| {
            let mut catalog = scripts(p)?;
            let at = catalog
                .scripts
                .iter()
                .position(|s| s.id == id)
                .ok_or("the script is gone")?;
            catalog.scripts[at].name = NAME.into();
            save(p, catalog).map(|_| ())
        },
        |p, ()| {
            let name = script(&scripts(p)?, id)?.name.clone();
            if name == NAME {
                Ok(format!("named “{name}”"))
            } else {
                Err(format!("named “{name}”, not “{NAME}”"))
            }
        },
    );
    let beep = one_step("Beep").map(|s| s.id());
    run(
        card,
        copy,
        ("Add a step and save the script", "#611"),
        |p| {
            let mut script = script(&scripts(p)?, id)?.clone();
            // As the Script Workspace adds a step: with its data key.
            let at = script.steps.len();
            script
                .insert_steps(at, [one_step("Beep")?])
                .map_err(|e| e.to_string())?;
            p.save_script(script).map(|_| ()).map_err(|e| e.message)
        },
        |p, ()| {
            let steps: Vec<u16> = script(&scripts(p)?, id)?
                .steps
                .iter()
                .map(ScriptStep::id)
                .collect();
            if Ok(steps.as_slice()) == beep.as_ref().map(std::slice::from_ref) {
                Ok("one step, Beep".into())
            } else {
                Err(format!("steps {steps:?}"))
            }
        },
    );
    made.copy = run(
        card,
        copy,
        ("Duplicate the script", "#619"),
        |p| {
            let mut catalog = scripts(p)?;
            let at = catalog
                .scripts
                .iter()
                .position(|s| s.id == id)
                .ok_or("the script is gone")?;
            let mut copy = catalog.scripts[at].clone();
            copy.id = catalog.scripts.iter().map(|s| s.id).max().unwrap_or(0) + 1;
            copy.name = format!("{} Copy", copy.name);
            for node in &mut copy.metadata {
                if node.segment == [0xd8] {
                    node.value = Some(fresh_uuid());
                }
            }
            let name = copy.name.clone();
            catalog.scripts.insert(at + 1, copy);
            let stored = save(p, catalog)?;
            stored
                .scripts
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.id)
                .ok_or_else(|| "the stored catalog has no copy".into())
        },
        |p, copy| {
            let catalog = scripts(p)?;
            let (original, copied) = (script(&catalog, id)?, script(&catalog, *copy)?);
            let ids = |s: &Script| s.steps.iter().map(ScriptStep::id).collect::<Vec<_>>();
            if ids(original) == ids(copied) && !copied.steps.is_empty() {
                Ok(format!("“{}” with the original's steps", copied.name))
            } else {
                Err(format!("“{}” has steps {:?}", copied.name, ids(copied)))
            }
        },
    );
    let Some(copy_id) = made.copy else {
        return;
    };
    run(
        card,
        copy,
        ("Move the copy to the top of the list", "#619"),
        |p| {
            let mut catalog = scripts(p)?;
            let at = catalog
                .scripts
                .iter()
                .position(|s| s.id == copy_id)
                .ok_or("the copy is gone")?;
            let moved = catalog.scripts.remove(at);
            catalog.scripts.insert(0, moved);
            save(p, catalog).map(|_| ())
        },
        |p, ()| match scripts(p)?.scripts.first() {
            Some(first) if first.id == copy_id => Ok(format!("“{}” first", first.name)),
            Some(first) => Err(format!("“{}” first", first.name)),
            None => Err("no scripts".into()),
        },
    );
    run(
        card,
        copy,
        ("Delete the copy", "#619"),
        |p| {
            let mut catalog = scripts(p)?;
            catalog.scripts.retain(|s| s.id != copy_id);
            save(p, catalog).map(|_| ())
        },
        |p, ()| {
            let catalog = scripts(p)?;
            match (
                catalog.scripts.iter().any(|s| s.id == copy_id),
                script(&catalog, id),
            ) {
                (false, Ok(_)) => Ok("gone; the original is kept".into()),
                (true, _) => Err("the copy is still there".into()),
                (false, Err(e)) => Err(format!("the original is gone too: {e}")),
            }
        },
    );
}

fn layout_steps(card: &mut BuildCard, copy: &Path, context: &Context, made: &mut Made) {
    const NAME: &str = "Scorecard Layout";
    made.layout = run(
        card,
        copy,
        ("Create a layout (New Layout / Report)", "#615"),
        |p| {
            p.create_layout(LayoutRequest {
                name: NAME.into(),
                occurrence: context.occurrence,
                like: None,
            })
            .map(|created| created.layout)
            .map_err(|e| e.message)
        },
        |p, layout| {
            let found = p
                .layouts()
                .map_err(|e| e.message)?
                .into_iter()
                .any(|l| l.id == *layout && l.name == NAME);
            found
                .then(|| format!("“{NAME}” is listed"))
                .ok_or_else(|| "the layout is not listed".into())
        },
    );
    if let (Some(layout), Some(script)) = (made.layout, made.script) {
        run(
            card,
            copy,
            ("Place a button that performs the script", "#614"),
            |p| {
                let (frame, expected) = body_frame(p, layout, 0.0)?;
                p.place_layout_button(LayoutButtonPlacement {
                    layout,
                    placement: ButtonPlacement {
                        frame,
                        label: "Run".into(),
                        action: ButtonAction::PerformScript {
                            script,
                            parameter: None,
                        },
                    },
                    expected,
                })
                .map(|_| ())
                .map_err(|e| e.message)
            },
            |p, ()| {
                let perform = one_step("Perform Script").map(|s| s.id()).unwrap_or(1);
                let found = buttons(p, layout)?;
                found
                    .iter()
                    .any(|(_, step)| *step == perform)
                    .then(|| "a Perform Script button".into())
                    .ok_or_else(|| format!("buttons {found:?}"))
            },
        );
    }
    if let Some(layout) = made.layout {
        run(
            card,
            copy,
            ("Place a button with a single step", "#626"),
            |p| {
                let (frame, expected) = body_frame(p, layout, 1.0)?;
                p.place_layout_button(LayoutButtonPlacement {
                    layout,
                    placement: ButtonPlacement {
                        frame,
                        label: "Beep".into(),
                        action: ButtonAction::Step(one_step("Beep")?),
                    },
                    expected,
                })
                .map(|_| ())
                .map_err(|e| e.message)
            },
            |p, ()| {
                let beep = one_step("Beep")?.id();
                let found = buttons(p, layout)?;
                found
                    .iter()
                    .any(|(_, step)| *step == beep)
                    .then(|| "a Beep button".into())
                    .ok_or_else(|| format!("buttons {found:?}"))
            },
        );
    }
    let layout = context.layout;
    run(
        card,
        copy,
        ("Move and resize a stored object", "#629"),
        |p| {
            let geometry = p
                .layout_geometry(layout)
                .map_err(|e| e.message)?
                .ok_or("the layout has no geometry")?;
            let expected = geometry
                .revision
                .ok_or("the layout's revision is not known")?;
            let object = geometry
                .objects
                .iter()
                .find(|o| o.movable && o.key != 0 && o.kind_name == "Field")
                .or_else(|| geometry.objects.iter().find(|o| o.movable && o.key != 0))
                .ok_or("no object Layout mode can move")?;
            let r = &object.rect;
            let frame = Rect {
                top: r.top + 4.0,
                left: r.left + 4.0,
                bottom: r.bottom + 4.0,
                right: r.right + 12.0,
                ..Rect::default()
            };
            p.set_layout_object_frames(LayoutObjectFrames {
                layout,
                frames: vec![ObjectFrame {
                    object: object.key,
                    frame: frame.clone(),
                }],
                expected,
            })
            .map_err(|e| e.message)?;
            Ok((object.key, frame))
        },
        |p, (key, frame)| {
            let geometry = p
                .layout_geometry(layout)
                .map_err(|e| e.message)?
                .ok_or("the layout has no geometry")?;
            let r = &geometry
                .objects
                .iter()
                .find(|o| o.key == *key)
                .ok_or("the object is gone")?
                .rect;
            if (r.top, r.left, r.bottom, r.right)
                == (frame.top, frame.left, frame.bottom, frame.right)
            {
                Ok(format!("object {key} at its new frame"))
            } else {
                Err(format!(
                    "object {key} at {:?}, not {:?}",
                    (r.top, r.left, r.bottom, r.right),
                    (frame.top, frame.left, frame.bottom, frame.right)
                ))
            }
        },
    );
}

fn field_steps(card: &mut BuildCard, copy: &Path, context: &Context) {
    let (table, occurrence) = (context.table, context.occurrence);
    run(
        card,
        copy,
        ("Options for Field: a calculated value", "#601"),
        |p| {
            field_with_options(p, table, "Scorecard Calculated", |_, options| {
                options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation::new(
                    OptionFormula::new("\"built\"", occurrence),
                ));
                Ok(())
            })
        },
        |p, field| match auto_enter(p, table, *field)? {
            AutoEnterValue::Calculated(calculation)
                if calculation.formula.text.as_deref() == Some("\"built\"") =>
            {
                Ok("calculated value \"built\"".into())
            }
            other => Err(format!("auto-enter {other:?}")),
        },
    );
    run(
        card,
        copy,
        (
            "Options for Field: a lookup (from a table matching on a plain text field)",
            "#618",
        ),
        |p| {
            let mut made = None;
            schema_edit(p, |p, schema| {
                let found = lookup_route(&schema)
                    .ok_or("no relationship a lookup can follow from a plain text match field")?;
                let (from_table, lookup) = found;
                let schema = p
                    .apply_schema_change(SchemaChange::CreateField {
                        table_id: from_table,
                        name: "Scorecard Lookup".into(),
                        data_type: FieldKind::Text,
                        comment: String::new(),
                    })
                    .map_err(|e| format!("Create: {}", e.message))?;
                let field = schema
                    .table(from_table)
                    .and_then(|t| t.fields.iter().find(|f| f.name == "Scorecard Lookup"))
                    .cloned()
                    .ok_or("the new field is not in the schema")?;
                let mut options = field.options.ok_or("the new field has no options")?;
                options.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup.clone()));
                p.apply_schema_change(SchemaChange::SetFieldOptions {
                    table_id: from_table,
                    field_id: field.id,
                    options: Box::new(options),
                })
                .map_err(|e| format!("Options for Field: {}", e.message))?;
                made = Some((from_table, field.id, lookup));
                Ok(())
            })?;
            made.ok_or_else(|| "nothing was made".into())
        },
        |p, (from_table, field, lookup)| match auto_enter(p, *from_table, *field)? {
            AutoEnterValue::LookedUp(Some(read)) if &read == lookup => Ok(format!(
                "in table {from_table}, looks up field {} through occurrence {}",
                read.copied, read.to
            )),
            other => Err(format!("auto-enter {other:?}")),
        },
    );
}

/// A lookup a user would make: from a table of this file whose match
/// fields are plain stored text (a foreign key, not an auto-entered key),
/// through an unsorted `=` relationship, copying a stored text field of
/// the other side's table; the table it starts from.
fn lookup_route(schema: &DatabaseSchema) -> Option<(u64, Lookup)> {
    let table_of = |occurrence: u64| {
        schema
            .occurrences
            .iter()
            .find(|o| o.id == occurrence)?
            .table_id
    };
    let field = |table: u64, id: u64| schema.table(table)?.fields.iter().find(|f| f.id == id);
    let plain_text = |f: &om_data::SchemaField| {
        f.kind == SchemaFieldKind::Normal
            && f.data_type == FieldKind::Text
            && f.options.as_ref().is_some_and(|o| {
                !o.storage.global
                    && o.storage.repetitions <= 1
                    && o.auto_enter.value == AutoEnterValue::None
                    && !o.auto_enter.prohibit_modification
            })
    };
    let stored_text = |f: &om_data::SchemaField| {
        f.kind == SchemaFieldKind::Normal
            && f.data_type == FieldKind::Text
            && f.options.as_ref().is_some_and(|o| !o.storage.global)
    };
    schema.relationships.iter().find_map(|r| {
        let sides = [
            (r.left_occurrence, r.right_occurrence, r.right().sort, false),
            (r.right_occurrence, r.left_occurrence, r.left().sort, true),
        ];
        sides.into_iter().find_map(|(from, to, sorted, reversed)| {
            let (from_table, to_table) = (table_of(from)?, table_of(to)?);
            let matches = !r.matches.is_empty()
                && r.matches.iter().all(|m| {
                    let (ours, theirs) = if reversed {
                        (m.right_field, m.left_field)
                    } else {
                        (m.left_field, m.right_field)
                    };
                    m.operator == 0
                        && field(from_table, ours).is_some_and(plain_text)
                        && field(to_table, theirs).is_some_and(stored_text)
                });
            if sorted || !matches {
                return None;
            }
            let copied = schema
                .table(to_table)?
                .fields
                .iter()
                .find(|f| stored_text(f))?
                .id;
            Some((
                from_table,
                Lookup {
                    from,
                    to,
                    copied,
                    copy_empty: false,
                    no_match: NoMatch::DoNotCopy,
                },
            ))
        })
    })
}

/// The first normal text field of `table` the schema lists.
fn text_field(schema: &DatabaseSchema, table: u64) -> Option<(u64, String)> {
    schema
        .table(table)?
        .fields
        .iter()
        .find(|f| f.kind == SchemaFieldKind::Normal && f.data_type == FieldKind::Text)
        .map(|f| (f.id, f.name.clone()))
}

/// A stored object: its key, kind, text and the field it shows.
type StoredObject = (u64, om_layout::ObjectKind, Option<String>, Option<u64>);

/// Objects of `layout` as stored.
fn stored_objects(p: &mut dyn DataProvider, layout: u32) -> Result<Vec<StoredObject>, String> {
    let render = p
        .render_layout(layout)
        .map_err(|e| e.message)?
        .ok_or("the layout does not render")?;
    Ok(render
        .layout
        .objects
        .objects
        .iter()
        .map(|o| {
            (
                o.id,
                o.kind(),
                o.text.clone(),
                o.field_spec().and_then(|s| s.field_id()),
            )
        })
        .collect())
}

/// The steps merged after the first Build measure (#652): Duplicate and
/// Delete Layout, a text and a field label, deleting a placed object,
/// Duplicate field, a lookup from a parent table on its UUID key and
/// global storage on a field that may hold values.
fn later_steps(card: &mut BuildCard, copy: &Path, context: &Context, made: &Made) {
    let first = context.layout;
    let copy_of = run(
        card,
        copy,
        ("Duplicate a layout (Duplicate Layout)", "#671"),
        |p| {
            let objects = stored_objects(p, first)?.len();
            let created = p.duplicate_layout(first).map_err(|e| e.message)?;
            Ok((created.layout, objects))
        },
        |p, (layout, objects)| {
            let name = p
                .layouts()
                .map_err(|e| e.message)?
                .into_iter()
                .find(|l| l.id == *layout)
                .map(|l| l.name)
                .ok_or("the copy is not listed")?;
            let copied = stored_objects(p, *layout)?.len();
            if copied == *objects {
                Ok(format!(
                    "“{name}” listed, with the original's {objects} objects"
                ))
            } else {
                Err(format!(
                    "“{name}” has {copied} objects, the original {objects}"
                ))
            }
        },
    )
    .map(|(layout, _)| layout);
    if let Some(layout) = copy_of {
        run(
            card,
            copy,
            ("Delete a layout (Delete Layout)", "#677"),
            |p| p.delete_layout(layout).map(|_| ()).map_err(|e| e.message),
            |p, ()| {
                let layouts = p.layouts().map_err(|e| e.message)?;
                match (
                    layouts.iter().any(|l| l.id == layout),
                    layouts.iter().any(|l| l.id == first),
                ) {
                    (false, true) => Ok("the copy is gone; the original is kept".into()),
                    (true, _) => Err("the copy is still listed".into()),
                    (false, false) => Err("the original is gone too".into()),
                }
            },
        );
    }
    if let Some(layout) = made.layout {
        const TEXT: &str = "Scorecard Text";
        run(
            card,
            copy,
            ("Place a text (Text tool)", "#666"),
            |p| {
                let (frame, expected) = body_frame(p, layout, 2.0)?;
                p.place_layout_text(om_data::layout_placement::LayoutTextPlacement {
                    layout,
                    text: om_data::layout_placement::Label {
                        text: TEXT.into(),
                        frame,
                    },
                    expected,
                })
                .map(|placed| placed.text_object)
                .map_err(|e| e.message)
            },
            |p, key| {
                let found = stored_objects(p, layout)?
                    .into_iter()
                    .any(|(id, _, text, _)| {
                        id == *key && text.as_deref().is_some_and(|t| t.contains(TEXT))
                    });
                found
                    .then(|| format!("object {key} reads “{TEXT}”"))
                    .ok_or_else(|| format!("object {key} with “{TEXT}” is not on the layout"))
            },
        );
        run(
            card,
            copy,
            ("Place a field with its label", "#666"),
            |p| {
                let schema = p.database_schema().map_err(|e| e.message)?;
                let (field, name) =
                    text_field(&schema, context.table).ok_or("the table has no text field")?;
                let (frame, expected) = body_frame(p, layout, 4.0)?;
                let label_frame = Rect {
                    top: frame.top - 20.0,
                    bottom: frame.top - 4.0,
                    ..frame.clone()
                };
                p.place_layout_field(om_data::layout_placement::LayoutFieldPlacement {
                    layout,
                    table: context.table,
                    placement: om_data::layout_placement::Placement {
                        occurrence: context.occurrence,
                        field,
                        frame,
                        label: Some(om_data::layout_placement::Label {
                            text: name.clone(),
                            frame: label_frame,
                        }),
                    },
                    expected,
                })
                .map_err(|e| e.message)?;
                Ok((field, name))
            },
            |p, (field, name)| {
                let objects = stored_objects(p, layout)?;
                let shown = objects.iter().any(|(_, _, _, f)| *f == Some(*field));
                let labelled = objects.iter().any(|(_, kind, text, _)| {
                    *kind == om_layout::ObjectKind::Text
                        && text.as_deref().is_some_and(|t| t.contains(name.as_str()))
                });
                if shown && labelled {
                    Ok(format!("{name} with a label “{name}”"))
                } else {
                    Err(format!("field shown: {shown}; label shown: {labelled}"))
                }
            },
        );
    }
    run(
        card,
        copy,
        ("Delete a placed object", "#641"),
        |p| {
            let geometry = p
                .layout_geometry(first)
                .map_err(|e| e.message)?
                .ok_or("the layout has no geometry")?;
            let expected = geometry
                .revision
                .ok_or("the layout's revision is not known")?;
            let mut candidates: Vec<_> = geometry
                .objects
                .iter()
                .filter(|o| o.movable && o.key != 0)
                .collect();
            candidates.sort_by_key(|o| if o.kind_name == "Text" { 0 } else { 1 });
            let mut refusals = Vec::new();
            for object in candidates {
                match p.delete_layout_objects(om_data::layout_objects::LayoutObjectDeletion {
                    layout: first,
                    objects: vec![object.key],
                    expected,
                }) {
                    Ok(_) => return Ok(object.key),
                    Err(e) => refusals.push(format!("object {}: {}", object.key, e.message)),
                }
            }
            Err(format!(
                "no object could be deleted: {}",
                refusals.join("; ")
            ))
        },
        |p, key| {
            let gone = !stored_objects(p, first)?.iter().any(|(id, ..)| id == key);
            gone.then(|| format!("object {key} is gone"))
                .ok_or_else(|| format!("object {key} is still there"))
        },
    );
    run(
        card,
        copy,
        ("Duplicate a field (Manage Database)", "#646"),
        |p| {
            let mut made = None;
            schema_edit(p, |p, schema| {
                let (field, name) =
                    text_field(&schema, context.table).ok_or("the table has no text field")?;
                p.apply_schema_change(SchemaChange::DuplicateField {
                    table_id: context.table,
                    field_id: field,
                })
                .map_err(|e| format!("Duplicate: {}", e.message))?;
                made = Some(name);
                Ok(())
            })?;
            made.ok_or_else(|| "nothing was duplicated".into())
        },
        |p, name| {
            let schema = p.database_schema().map_err(|e| e.message)?;
            let copy_name = format!("{name} Copy");
            let copied = schema
                .table(context.table)
                .and_then(|t| t.fields.iter().find(|f| f.name == copy_name))
                .ok_or_else(|| format!("no field “{copy_name}”"))?;
            (copied.data_type == FieldKind::Text && copied.kind == SchemaFieldKind::Normal)
                .then(|| format!("“{copy_name}”, a text field"))
                .ok_or_else(|| format!("“{copy_name}” is not a normal text field"))
        },
    );
    run(
        card,
        copy,
        (
            "Options for Field: a lookup from a parent table on its UUID key",
            "#678",
        ),
        |p| {
            let mut made = None;
            let table = context.table;
            schema_edit(p, |p, schema| {
                let lookup = parent_lookup(&schema, table).ok_or(
                    "no unsorted relationship from this table to copy a text field through",
                )?;
                let schema = p
                    .apply_schema_change(SchemaChange::CreateField {
                        table_id: table,
                        name: "Scorecard Parent Lookup".into(),
                        data_type: FieldKind::Text,
                        comment: String::new(),
                    })
                    .map_err(|e| format!("Create: {}", e.message))?;
                let field = schema
                    .table(table)
                    .and_then(|t| {
                        t.fields
                            .iter()
                            .find(|f| f.name == "Scorecard Parent Lookup")
                    })
                    .cloned()
                    .ok_or("the new field is not in the schema")?;
                let mut options = field.options.ok_or("the new field has no options")?;
                options.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup.clone()));
                p.apply_schema_change(SchemaChange::SetFieldOptions {
                    table_id: table,
                    field_id: field.id,
                    options: Box::new(options),
                })
                .map_err(|e| format!("Options for Field: {}", e.message))?;
                made = Some((field.id, lookup));
                Ok(())
            })?;
            made.ok_or_else(|| "nothing was made".into())
        },
        |p, (field, lookup)| match auto_enter(p, context.table, *field)? {
            AutoEnterValue::LookedUp(Some(read)) if &read == lookup => Ok(format!(
                "looks up field {} through occurrence {}",
                read.copied, read.to
            )),
            other => Err(format!("auto-enter {other:?}")),
        },
    );
    run(
        card,
        copy,
        (
            "Options for Field: global storage on a field that may hold values",
            "#635",
        ),
        |p| {
            let schema = p.database_schema().map_err(|e| e.message)?;
            let candidates: Vec<(u64, String)> = schema
                .table(context.table)
                .map(|t| {
                    t.fields
                        .iter()
                        .filter(|f| {
                            f.kind == SchemaFieldKind::Normal
                                && f.data_type == FieldKind::Text
                                && f.options.as_ref().is_some_and(|o| {
                                    !o.storage.global && o.auto_enter.value == AutoEnterValue::None
                                })
                        })
                        .map(|f| (f.id, f.name.clone()))
                        .collect()
                })
                .unwrap_or_default();
            // The first that no other definition uses: the draft refuses
            // the others, with the reason.
            let mut refusals = Vec::new();
            for (field, name) in candidates {
                let result = schema_edit(p, |p, schema| {
                    let mut options = schema
                        .table(context.table)
                        .and_then(|t| t.fields.iter().find(|f| f.id == field))
                        .and_then(|f| f.options.clone())
                        .ok_or("the field has no options")?;
                    options.storage.global = true;
                    options.storage.indexing = om_data::field_options::Indexing::None;
                    p.apply_schema_change(SchemaChange::SetFieldOptions {
                        table_id: context.table,
                        field_id: field,
                        options: Box::new(options),
                    })
                    .map(|_| ())
                    .map_err(|e| e.message)
                });
                match result {
                    Ok(()) => return Ok((field, name)),
                    Err(e) => refusals.push(format!("{name}: {e}")),
                }
            }
            Err(format!(
                "no field could be made global: {}",
                refusals.join("; ")
            ))
        },
        |p, (field, name)| {
            let schema = p.database_schema().map_err(|e| e.message)?;
            let global = schema
                .table(context.table)
                .and_then(|t| t.fields.iter().find(|f| f.id == *field))
                .and_then(|f| f.options.as_ref())
                .is_some_and(|o| o.storage.global);
            global
                .then(|| format!("{name} uses global storage"))
                .ok_or_else(|| format!("{name} does not use global storage"))
        },
    );
}

/// A lookup from `table` (a parent, matching on its auto-entered UUID key)
/// through an unsorted `=` relationship of an occurrence of it, copying a
/// stored text field of the other side's table.
/// The layout's revision, as Layout mode knows it.
fn revision(p: &mut dyn DataProvider, layout: u32) -> Result<[u8; 32], String> {
    p.layout_geometry(layout)
        .map_err(|e| e.message)?
        .ok_or("the layout has no geometry")?
        .revision
        .ok_or_else(|| "the layout's revision is not known".into())
}

fn rendered(p: &mut dyn DataProvider, layout: u32) -> Result<om_layout::Layout, String> {
    p.render_layout(layout)
        .map_err(|e| e.message)?
        .map(|r| r.layout)
        .ok_or_else(|| format!("layout {layout} is not rendered"))
}

fn layout_flags(p: &mut dyn DataProvider, layout: u32) -> Result<u64, String> {
    rendered(p, layout)?
        .def
        .map(|d| d.flags)
        .ok_or_else(|| "the layout has no definition".into())
}

/// Layout mode's setup steps (#714, #720, #817, #766, #827, #847, #855):
/// each through the operation its dialog or the Inspector calls.
fn setup_steps(card: &mut BuildCard, copy: &Path, made: &Made) {
    use om_data::layout_catalog::{general, views, LayoutSetupChange};
    use om_data::layout_placement::{part_options, LayoutWidthChange, PartOptionsChange};
    if let Some(layout) = made.layout {
        const NAME: &str = "Scorecard Setup";
        // Table view off, and Enable Quick Find turned the other way.
        let quick_find = general::OPTIONS
            .iter()
            .find(|o| o.0 == "enable_quick_find")
            .map_or(0x8000, |o| o.2);
        run(
            card,
            copy,
            (
                "Layout Setup: name, views and General options",
                "#714, #720, #817",
            ),
            |p| {
                let expected = revision(p, layout)?;
                let stored = layout_flags(p, layout)?;
                let hidden =
                    p.layout_menu().map_err(|e| e.message)?.iter().any(|e| {
                        e.id == layout && e.flags & om_data::layout_catalog::MENU_HIDDEN != 0
                    });
                let flags = (stored | views::TABLE_OFF) ^ quick_find;
                p.set_layout_setup(LayoutSetupChange {
                    layout,
                    expected,
                    name: NAME.into(),
                    flags,
                    hidden,
                })
                .map_err(|e| e.message)?;
                Ok(flags & (views::ALL | general::ALL))
            },
            |p, wanted| {
                let name = p
                    .layouts()
                    .map_err(|e| e.message)?
                    .into_iter()
                    .find(|l| l.id == layout)
                    .map(|l| l.name)
                    .ok_or("the layout is not listed")?;
                let flags = layout_flags(p, layout)? & (views::ALL | general::ALL);
                if name == NAME && flags == *wanted {
                    Ok(format!(
                        "“{name}”, Table view off, Enable Quick Find switched (setup bits {flags:#x})"
                    ))
                } else {
                    Err(format!(
                        "named “{name}”, setup bits {flags:#x}, not “{NAME}” and {wanted:#x}"
                    ))
                }
            },
        );
        run(
            card,
            copy,
            ("A layout's width (its right edge dragged)", "#766"),
            |p| {
                let expected = revision(p, layout)?;
                let width = rendered(p, layout)?
                    .def
                    .map(|d| d.base_width)
                    .ok_or("the layout has no definition")?
                    + 40.0;
                p.set_layout_width(LayoutWidthChange {
                    layout,
                    expected,
                    width,
                })
                .map_err(|e| e.message)?;
                Ok(width)
            },
            |p, width| {
                let now = rendered(p, layout)?.def.map(|d| d.base_width);
                if now == Some(*width) {
                    Ok(format!("{width} points wide (40 more)"))
                } else {
                    Err(format!("{now:?} points wide, not {width}"))
                }
            },
        );
        run(
            card,
            copy,
            ("A part's options (Part Setup)", "#847, #855"),
            |p| {
                let expected = revision(p, layout)?;
                let part = rendered(p, layout)?
                    .parts
                    .into_iter()
                    .find(|part| {
                        part_options::offered(part.part_type)
                            .is_some_and(|o| o & part_options::RESTART_PAGE_NUMBERS != 0)
                    })
                    .ok_or("no part offers Restart page numbers")?;
                let flags = part.flags ^ part_options::RESTART_PAGE_NUMBERS;
                p.set_part_options(PartOptionsChange {
                    layout,
                    expected,
                    part: part.id,
                    flags,
                    page_break_after: part.page_break_after,
                    break_field: None,
                })
                .map_err(|e| e.message)?;
                Ok((part.id, part.part_type, flags))
            },
            |p, (id, kind, flags)| {
                let now = rendered(p, layout)?
                    .parts
                    .into_iter()
                    .find(|part| part.id == *id)
                    .map(|part| part.flags);
                if now == Some(*flags) {
                    Ok(format!(
                        "part {id} (type {kind}): Restart page numbers switched (flags {flags:#x})"
                    ))
                } else {
                    Err(format!("part {id} has flags {now:?}, not {flags:#x}"))
                }
            },
        );
    }
    run(
        card,
        copy,
        ("A field's control style (Inspector)", "#827"),
        |p| {
            use om_data::layout_objects::FieldControlChange;
            let schema = p
                .calc_schema()
                .map_err(|e| e.message)?
                .ok_or("no calculation schema")?;
            let date = |o: &om_layout::LayoutObject| {
                o.field_spec()
                    .and_then(|spec| {
                        schema.field(om_layout::occurrence_id(spec.table_key)?, spec.field_id()?)
                    })
                    .is_some_and(|d| d.value_type == om_data::calc::ValueType::Date)
            };
            // The first layout with a drop-down list or pop-up menu that
            // has its value list, shown as the other, or a date edit box or
            // drop-down calendar, shown as the other.
            let layouts = p.layouts().map_err(|e| e.message)?;
            let mut found = None;
            for info in layouts {
                let Ok(Some(render)) = p.render_layout(info.id) else {
                    continue;
                };
                found = render
                    .layout
                    .objects
                    .objects
                    .iter()
                    .filter(|o| render.field_bindings.contains_key(&o.id))
                    .find_map(|o| {
                        let field = o.field_obj.as_ref()?;
                        let control = match field.display_type {
                            1 if field.value_list_key != 0 => 2,
                            2 if field.value_list_key != 0 => 1,
                            0 if date(o) => 6,
                            6 if date(o) => 0,
                            _ => return None,
                        };
                        Some((info.id, o.id, control))
                    });
                if found.is_some() {
                    break;
                }
            }
            let (layout, object, control) =
                found.ok_or("no list or menu with a value list, nor a date field")?;
            let expected = revision(p, layout)?;
            p.set_field_control(FieldControlChange {
                layout,
                expected,
                object,
                control,
                value_list: None,
            })
            .map_err(|e| e.message)?;
            Ok((layout, object, control))
        },
        |p, (layout, object, control)| {
            let now = rendered(p, *layout)?
                .objects
                .get(*object)
                .and_then(|o| o.field_obj.as_ref())
                .map(|f| f.display_type);
            let name = |c: u32| {
                om_data::layout_objects::FIELD_CONTROLS
                    .iter()
                    .find(|f| f.0 == c)
                    .map_or("?", |f| f.2)
            };
            let on = layout_name(p, *layout);
            if now == Some(*control) {
                Ok(format!(
                    "object {object} on “{on}” now shown as: {}",
                    name(*control)
                ))
            } else {
                Err(format!(
                    "object {object} on “{on}” has display type {now:?}, not {control} ({})",
                    name(*control)
                ))
            }
        },
    );
}

fn parent_lookup(schema: &DatabaseSchema, table: u64) -> Option<Lookup> {
    let table_of = |occurrence: u64| {
        schema
            .occurrences
            .iter()
            .find(|o| o.id == occurrence)?
            .table_id
    };
    schema.relationships.iter().find_map(|r| {
        let sides = [
            (r.left_occurrence, r.right_occurrence, r.right().sort),
            (r.right_occurrence, r.left_occurrence, r.left().sort),
        ];
        sides.into_iter().find_map(|(from, to, sorted)| {
            if sorted || table_of(from)? != table || r.matches.iter().any(|m| m.operator != 0) {
                return None;
            }
            let to_table = table_of(to)?;
            let copied = schema
                .table(to_table)?
                .fields
                .iter()
                .find(|f| {
                    f.kind == SchemaFieldKind::Normal
                        && f.data_type == FieldKind::Text
                        && f.options.as_ref().is_some_and(|o| !o.storage.global)
                })?
                .id;
            Some(Lookup {
                from,
                to,
                copied,
                copy_empty: false,
                no_match: NoMatch::DoNotCopy,
            })
        })
    })
}

/// One layout or field step for FileMaker Pro to check (`build_step_files`):
/// what it did, for the README.
type StepFile = fn(&mut dyn DataProvider, &Context, &mut Made) -> Result<String, String>;

fn bounds(r: &om_data::GeomRect) -> String {
    format!(
        "top {}, left {}, bottom {}, right {} (points)",
        r.top, r.left, r.bottom, r.right
    )
}

fn layout_name(p: &mut dyn DataProvider, layout: u32) -> String {
    p.layouts()
        .ok()
        .and_then(|ls| ls.into_iter().find(|l| l.id == layout))
        .map_or_else(|| format!("layout {layout}"), |l| l.name)
}

/// A frame for a new button where layout `layout` shows nothing yet: the
/// first 120 × 24 point spot of its body clear of every object.
fn free_frame(p: &mut dyn DataProvider, layout: u32) -> Result<(Rect, [u8; 32]), String> {
    let geometry = p
        .layout_geometry(layout)
        .map_err(|e| e.message)?
        .ok_or("the layout has no geometry")?;
    let revision = geometry
        .revision
        .ok_or("the layout's revision is not known")?;
    let body = geometry
        .parts
        .iter()
        .find(|part| part.kind_name == "Body")
        .ok_or("the layout has no body part")?;
    let clear = |top: f64, left: f64| {
        geometry.objects.iter().all(|o| {
            let r = &o.rect;
            r.bottom <= top - 4.0
                || r.top >= top + 28.0
                || r.right <= left - 4.0
                || r.left >= left + 124.0
        })
    };
    let mut top = body.top + 4.0;
    while top + 24.0 <= body.top + body.height - 4.0 {
        let mut left = 8.0;
        while left + 120.0 <= geometry.width - 8.0 {
            if clear(top, left) {
                let frame = Rect {
                    top,
                    left,
                    bottom: top + 24.0,
                    right: left + 120.0,
                    ..Rect::default()
                };
                return Ok((frame, revision));
            }
            left += 10.0;
        }
        top += 8.0;
    }
    Err("the body has no clear 120 × 24 point spot".into())
}

fn step_new_layout(
    p: &mut dyn DataProvider,
    c: &Context,
    made: &mut Made,
) -> Result<String, String> {
    const NAME: &str = "Scorecard Layout";
    let occurrence = p
        .calc_schema()
        .ok()
        .flatten()
        .and_then(|s| s.occurrence(c.occurrence).map(|o| o.name.clone()))
        .unwrap_or_else(|| format!("occurrence {}", c.occurrence));
    let created = p
        .create_layout(LayoutRequest {
            name: NAME.into(),
            occurrence: c.occurrence,
            like: None,
        })
        .map_err(|e| e.message)?;
    made.layout = Some(created.layout);
    Ok(format!(
        "New Layout / Report (#615): a Form layout \"{NAME}\" (id {}) showing records of {occurrence}, at the end of the layout list; its body is empty.",
        created.layout
    ))
}

/// Place a button on the first layout of a table of this file with a
/// clear spot in its body: the layout, the button and its frame.
fn place_button(
    p: &mut dyn DataProvider,
    label: &str,
    action: ButtonAction,
) -> Result<(u32, u64, Rect), String> {
    let schema = p
        .calc_schema()
        .map_err(|e| e.message)?
        .ok_or("no calculation schema")?;
    let mut found = None;
    for l in schema.layouts() {
        if l.occurrence.and_then(|o| schema.table_of(o)).is_none() {
            continue;
        }
        if let Ok(spot) = free_frame(p, l.id) {
            found = Some((l.id, spot));
            break;
        }
    }
    let (layout, (frame, expected)) =
        found.ok_or("no layout of this file has a clear 120 × 24 point spot in its body")?;
    let placed = p
        .place_layout_button(LayoutButtonPlacement {
            layout,
            placement: ButtonPlacement {
                frame: frame.clone(),
                label: label.into(),
                action,
            },
            expected,
        })
        .map_err(|e| e.message)?;
    Ok((layout, placed.button, frame))
}

fn rect_text(r: &Rect) -> String {
    format!(
        "top {}, left {}, bottom {}, right {} (points)",
        r.top, r.left, r.bottom, r.right
    )
}

fn step_perform_button(
    p: &mut dyn DataProvider,
    _: &Context,
    _: &mut Made,
) -> Result<String, String> {
    let catalog = scripts(p)?;
    let script = catalog
        .scripts
        .iter()
        .find(|s| s.kind == ScriptKind::Script)
        .ok_or("the file has no script")?
        .clone();
    let (layout, button, frame) = place_button(
        p,
        "Run",
        ButtonAction::PerformScript {
            script: script.id,
            parameter: None,
        },
    )?;
    Ok(format!(
        "Button with Perform Script (#614) on layout \"{}\": object {button}, label \"Run\", performs the file's own script \"{}\" (id {}), no parameter; {}.",
        layout_name(p, layout),
        script.name,
        script.id,
        rect_text(&frame)
    ))
}

fn step_single_button(
    p: &mut dyn DataProvider,
    _: &Context,
    _: &mut Made,
) -> Result<String, String> {
    let (layout, button, frame) = place_button(p, "Beep", ButtonAction::Step(one_step("Beep")?))?;
    Ok(format!(
        "Button with a single step (#626) on layout \"{}\": object {button}, label \"Beep\", step Beep; {}.",
        layout_name(p, layout),
        rect_text(&frame)
    ))
}

fn step_move_object(
    p: &mut dyn DataProvider,
    c: &Context,
    made: &mut Made,
) -> Result<String, String> {
    let geometry = p
        .layout_geometry(c.layout)
        .map_err(|e| e.message)?
        .ok_or("the layout has no geometry")?;
    let expected = geometry
        .revision
        .ok_or("the layout's revision is not known")?;
    let object = geometry
        .objects
        .iter()
        .find(|o| o.movable && o.key != 0 && o.kind_name == "Field")
        .ok_or("no field object Layout mode can move")?
        .clone();
    let r = &object.rect;
    let frame = Rect {
        top: r.top + 4.0,
        left: r.left + 4.0,
        bottom: r.bottom + 4.0,
        right: r.right + 12.0,
        ..Rect::default()
    };
    p.set_layout_object_frames(LayoutObjectFrames {
        layout: c.layout,
        frames: vec![ObjectFrame {
            object: object.key,
            frame: frame.clone(),
        }],
        expected,
    })
    .map_err(|e| e.message)?;
    made.copy = Some(object.key);
    Ok(format!(
        "Moved and resized object (#629) on layout \"{}\": object {}, a field showing {}, from {} to {} (4 points down and right, 8 points wider).",
        layout_name(p, c.layout),
        object.key,
        object.field_name.as_deref().unwrap_or("(unnamed)"),
        bounds(&object.rect),
        rect_text(&frame)
    ))
}

fn step_delete_object(
    p: &mut dyn DataProvider,
    c: &Context,
    made: &mut Made,
) -> Result<String, String> {
    let geometry = p
        .layout_geometry(c.layout)
        .map_err(|e| e.message)?
        .ok_or("the layout has no geometry")?;
    let expected = geometry
        .revision
        .ok_or("the layout's revision is not known")?;
    // A text label first, then a field, never the object moved before.
    let mut candidates: Vec<_> = geometry
        .objects
        .iter()
        .filter(|o| o.movable && o.key != 0 && Some(o.key) != made.copy)
        .collect();
    candidates.sort_by_key(|o| match o.kind_name.as_str() {
        "Text" => 0,
        "Field" => 1,
        _ => 2,
    });
    let texts: std::collections::BTreeMap<u64, String> = p
        .render_layout(c.layout)
        .ok()
        .flatten()
        .map(|r| {
            r.layout
                .objects
                .objects
                .iter()
                .filter_map(|o| Some((o.id, o.text.clone()?)))
                .collect()
        })
        .unwrap_or_default();
    let mut refusals = Vec::new();
    for object in candidates {
        match p.delete_layout_objects(om_data::layout_objects::LayoutObjectDeletion {
            layout: c.layout,
            objects: vec![object.key],
            expected,
        }) {
            Ok(_) => {
                return Ok(format!(
                    "Deleted object (#641) on layout \"{}\": object {}, a {} {}, which was at {}; it should be gone.",
                    layout_name(p, c.layout),
                    object.key,
                    object.kind_name.to_lowercase(),
                    match (object.field_name.as_deref(), texts.get(&object.key)) {
                        (Some(field), _) => format!("showing {field}"),
                        (None, Some(text)) => format!("reading \"{}\"", text.trim()),
                        (None, None) => String::new(),
                    },
                    bounds(&object.rect)
                ))
            }
            Err(e) => refusals.push(format!("object {}: {}", object.key, e.message)),
        }
    }
    Err(format!(
        "no object could be deleted: {}",
        refusals.join("; ")
    ))
}

fn names(p: &mut dyn DataProvider, table: u64, field: u64) -> (String, String) {
    p.database_schema()
        .ok()
        .and_then(|s| {
            let t = s.table(table)?;
            Some((
                t.name.clone(),
                t.fields.iter().find(|f| f.id == field)?.name.clone(),
            ))
        })
        .unwrap_or_else(|| (format!("table {table}"), format!("field {field}")))
}

fn step_calculated(p: &mut dyn DataProvider, c: &Context, _: &mut Made) -> Result<String, String> {
    let occurrence = c.occurrence;
    let field = field_with_options(p, c.table, "Scorecard Calculated", |_, options| {
        options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation::new(
            OptionFormula::new("\"built\"", occurrence),
        ));
        Ok(())
    })?;
    let (table, name) = names(p, c.table, field);
    Ok(format!(
        "Calculated value (#601): a new text field {table}::{name} (id {field}) whose Options for Field auto-enter the calculated value \"built\" (Do not replace existing value, Do not evaluate if all referenced fields are empty: on, as FileMaker Pro offers them). A new record gets \"built\" in it."
    ))
}

fn step_lookup(p: &mut dyn DataProvider, _: &Context, _: &mut Made) -> Result<String, String> {
    let mut made = None;
    schema_edit(p, |p, schema| {
        let (from_table, lookup) = lookup_route(&schema)
            .ok_or("no relationship a lookup can follow from a plain text match field")?;
        let schema = p
            .apply_schema_change(SchemaChange::CreateField {
                table_id: from_table,
                name: "Scorecard Lookup".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .map_err(|e| format!("Create: {}", e.message))?;
        let field = schema
            .table(from_table)
            .and_then(|t| t.fields.iter().find(|f| f.name == "Scorecard Lookup"))
            .cloned()
            .ok_or("the new field is not in the schema")?;
        let mut options = field.options.ok_or("the new field has no options")?;
        options.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup.clone()));
        p.apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: from_table,
            field_id: field.id,
            options: Box::new(options),
        })
        .map_err(|e| format!("Options for Field: {}", e.message))?;
        let occurrence = |id: u64| {
            schema
                .occurrences
                .iter()
                .find(|o| o.id == id)
                .map_or_else(|| format!("occurrence {id}"), |o| o.name.clone())
        };
        let to_table = schema
            .occurrences
            .iter()
            .find(|o| o.id == lookup.to)
            .and_then(|o| o.table_id)
            .unwrap_or_default();
        let copied = schema
            .table(to_table)
            .and_then(|t| t.fields.iter().find(|f| f.id == lookup.copied))
            .map_or_else(|| format!("field {}", lookup.copied), |f| f.name.clone());
        let table = schema
            .table(from_table)
            .map_or_else(String::new, |t| t.name.clone());
        made = Some(format!(
            "Lookup (#618): a new text field {table}::Scorecard Lookup (id {}) whose Options for Field look up {}::{copied}, starting with occurrence {} (Don't copy contents if empty: on; if no exact match: do not copy).",
            field.id,
            occurrence(lookup.to),
            occurrence(lookup.from)
        ));
        Ok(())
    })?;
    made.ok_or_else(|| "nothing was made".into())
}

/// The context of the file at `copy` (its first layout of a table of its
/// own), as [`card`] reads it.
fn context_of(copy: &Path) -> Result<Context, String> {
    let mut p = open(copy)?;
    let schema = p
        .calc_schema()
        .map_err(|e| e.message)?
        .ok_or("no calculation schema")?;
    p.close();
    let (layout, occurrence, table) = schema
        .layouts()
        .iter()
        .find_map(|l| {
            let occurrence = l.occurrence?;
            Some((l.id, occurrence, schema.table_of(occurrence)?.id))
        })
        .ok_or("no layout of a table of this file")?;
    Ok(Context {
        layout,
        occurrence,
        table,
    })
}

/// Apply `steps` to a fresh copy of `original` in `out` named `name`, each
/// on the file opened anew and closed after it; the README lines, or the
/// first error. The copy kept is taken under the host's lock.
fn step_file(
    original: &Path,
    out: &Path,
    name: &str,
    steps: &[StepFile],
) -> Result<Vec<String>, String> {
    let work = out.join(".work");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let copy = work.join(name);
    std::fs::copy(original, &copy).map_err(|e| e.to_string())?;
    let context = context_of(&copy)?;
    let mut made = Made::default();
    let mut lines = Vec::new();
    for step in steps {
        let mut p = open(&copy)?;
        let result = step(&mut *p, &context, &mut made);
        p.close();
        drop(p);
        lines.push(result?);
    }
    super::trust::copy_released(&copy, &out.join(name)).map_err(|e| e.to_string())?;
    std::fs::remove_file(&copy).map_err(|e| e.to_string())?;
    Ok(lines)
}

/// Files for FileMaker Pro's check of criterion 7 (#221, #541): each layout
/// and field step of the build measure applied alone to a fresh copy of
/// the file `OM_BUILD_STEP_SOURCE`, and one copy with all of them, each
/// left closed in `OM_BUILD_STEP_FILES` with a README saying where to look.
/// No script is made: a button performs one of the file's own scripts.
#[test]
#[ignore = "requires OM_BUILD_STEP_SOURCE (a .fmp12 file, Admin with no password) and OM_BUILD_STEP_FILES (an output folder)"]
fn build_step_files() {
    // A generator, not a check: without its folders it has nothing to do,
    // and a filter that also matches it (`--lib scorecard`) does not fail
    // on it.
    let (Some(source), Some(out)) = (
        std::env::var_os("OM_BUILD_STEP_SOURCE"),
        std::env::var_os("OM_BUILD_STEP_FILES"),
    ) else {
        eprintln!("build_step_files: set OM_BUILD_STEP_SOURCE and OM_BUILD_STEP_FILES");
        return;
    };
    let (source, out) = (
        std::path::PathBuf::from(source),
        std::path::PathBuf::from(out),
    );
    std::fs::create_dir_all(&out).expect("the output folder");
    let steps: [(&str, StepFile); 7] = [
        ("01-new-layout.fmp12", step_new_layout),
        ("02-button-perform-script.fmp12", step_perform_button),
        ("03-button-single-step.fmp12", step_single_button),
        ("04-moved-resized-object.fmp12", step_move_object),
        ("05-deleted-object.fmp12", step_delete_object),
        ("06-calculated-value.fmp12", step_calculated),
        ("07-lookup.fmp12", step_lookup),
    ];
    let mut readme = String::new();
    let mut failed = Vec::new();
    for (name, step) in steps {
        match step_file(&source, &out, name, &[step]) {
            Ok(lines) => readme.push_str(&format!("{name}\n  {}\n\n", lines.join("\n  "))),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    let all: Vec<StepFile> = steps.iter().map(|(_, step)| *step).collect();
    match step_file(&source, &out, "08-all-steps.fmp12", &all) {
        Ok(lines) => readme.push_str(&format!(
            "08-all-steps.fmp12 (the seven steps above, in that order, on one copy)\n  {}\n\n",
            lines.join("\n  ")
        )),
        Err(e) => failed.push(format!("08-all-steps.fmp12: {e}")),
    }
    let _ = std::fs::remove_dir_all(out.join(".work"));
    std::fs::write(out.join("README.body"), &readme).expect("README");
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The id of the script named `name`.
fn named(catalog: &ScriptCatalog, name: &str) -> Result<u64, String> {
    catalog
        .scripts
        .iter()
        .find(|s| s.kind == ScriptKind::Script && s.name == name)
        .map(|s| s.id)
        .ok_or_else(|| format!("no script “{name}”"))
}

fn step_names(script: &Script) -> String {
    let names: Vec<String> = script
        .steps
        .iter()
        .map(|s| {
            om_data::scripts::step_name(s.id())
                .unwrap_or("?")
                .to_owned()
        })
        .collect();
    names.join(", ")
}

fn script_new(p: &mut dyn DataProvider, _: &Context, _: &mut Made) -> Result<String, String> {
    let mut catalog = scripts(p)?;
    let id = catalog.scripts.iter().map(|s| s.id).max().unwrap_or(0) + 1;
    catalog.scripts.push(Script {
        id,
        name: "New Script".into(),
        kind: ScriptKind::Script,
        ..Script::default()
    });
    let stored = save(p, catalog)?;
    let made = stored
        .scripts
        .iter()
        .rev()
        .find(|s| s.name == "New Script")
        .ok_or("the stored catalog has no new script")?;
    Ok(format!(
        "New script (#611, New Script): \"New Script\" (id {}), no steps, at the end of the scripts list, after the Tasks folder.",
        made.id
    ))
}

fn script_rename(p: &mut dyn DataProvider, _: &Context, _: &mut Made) -> Result<String, String> {
    let mut catalog = scripts(p)?;
    let id = named(&catalog, "Go to Entry Field")?;
    let at = catalog.scripts.iter().position(|s| s.id == id).unwrap();
    catalog.scripts[at].name = "Go to Entry Field Renamed".into();
    save(p, catalog)?;
    Ok(format!(
        "Renamed script (#611): \"Go to Entry Field\" (id {id}) is now \"Go to Entry Field Renamed\", in the same place in the Tasks folder with the same steps; what performs it refers to it by id, so it shows the new name."
    ))
}

fn script_add_step(p: &mut dyn DataProvider, _: &Context, _: &mut Made) -> Result<String, String> {
    let catalog = scripts(p)?;
    // Renamed first when the operations run together.
    let id = named(&catalog, "Go to Entry Field")
        .or_else(|_| named(&catalog, "Go to Entry Field Renamed"))?;
    let mut script = script(&catalog, id)?.clone();
    let name = script.name.clone();
    let before = step_names(&script);
    let at = script.steps.len();
    script
        .insert_steps(at, [one_step("Beep")?])
        .map_err(|e| e.to_string())?;
    let after = step_names(&script);
    p.save_script(script).map_err(|e| e.message)?;
    Ok(format!(
        "Step added and saved (#611): script \"{name}\" (id {id}) had {before}; it now ends with Beep: {after}."
    ))
}

fn script_duplicate(p: &mut dyn DataProvider, _: &Context, _: &mut Made) -> Result<String, String> {
    let mut catalog = scripts(p)?;
    let id = named(&catalog, "Choose Assignee")?;
    let at = catalog.scripts.iter().position(|s| s.id == id).unwrap();
    let mut copy = catalog.scripts[at].clone();
    copy.id = catalog.scripts.iter().map(|s| s.id).max().unwrap_or(0) + 1;
    copy.name = format!("{} Copy", copy.name);
    for node in &mut copy.metadata {
        if node.segment == [0xd8] {
            node.value = Some(fresh_uuid());
        }
    }
    let steps = copy.steps.len();
    catalog.scripts.insert(at + 1, copy);
    let stored = save(p, catalog)?;
    let made = stored
        .scripts
        .iter()
        .find(|s| s.name == "Choose Assignee Copy")
        .ok_or("the stored catalog has no copy")?;
    Ok(format!(
        "Duplicated script (#619): \"Choose Assignee Copy\" (id {}), right after \"Choose Assignee\" (id {id}) in the Tasks folder, with the same {steps} steps.",
        made.id
    ))
}

fn script_move(p: &mut dyn DataProvider, _: &Context, _: &mut Made) -> Result<String, String> {
    let mut catalog = scripts(p)?;
    let id = named(&catalog, "Sort by Status")?;
    let at = catalog.scripts.iter().position(|s| s.id == id).unwrap();
    let moved = catalog.scripts.remove(at);
    catalog.scripts.insert(0, moved);
    save(p, catalog)?;
    Ok(format!(
        "Moved script (#619): \"Sort by Status\" (id {id}) is now first in the scripts list, above (outside) the Tasks folder; its steps are unchanged."
    ))
}

fn script_delete(p: &mut dyn DataProvider, _: &Context, _: &mut Made) -> Result<String, String> {
    let mut catalog = scripts(p)?;
    let id = named(&catalog, "Cancel Add Assignee")?;
    catalog.scripts.retain(|s| s.id != id);
    save(p, catalog)?;
    Ok(format!(
        "Deleted script (#619): \"Cancel Add Assignee\" (id {id}) is gone from the scripts list; anything that performed it (likely only Add Assignee's Cancel button; references were not checked here) now names a missing script, which FileMaker Pro shows as <unknown>."
    ))
}

/// Files for FileMaker Pro's check of the Script Workspace's operations
/// (#221, #667): each script operation of the build measure applied alone
/// to a fresh copy of the file `OM_BUILD_STEP_SOURCE` (Tasks: its own
/// scripts are renamed, given a step, duplicated, moved and deleted), and
/// one copy with all of them, left closed in `OM_BUILD_STEP_FILES` with a
/// README saying what each had done to it.
#[test]
#[ignore = "requires OM_BUILD_STEP_SOURCE (Tasks.fmp12, Admin with no password) and OM_BUILD_STEP_FILES (an output folder)"]
fn build_script_step_files() {
    let (Some(source), Some(out)) = (
        std::env::var_os("OM_BUILD_STEP_SOURCE"),
        std::env::var_os("OM_BUILD_STEP_FILES"),
    ) else {
        eprintln!("build_script_step_files: set OM_BUILD_STEP_SOURCE and OM_BUILD_STEP_FILES");
        return;
    };
    let (source, out) = (
        std::path::PathBuf::from(source),
        std::path::PathBuf::from(out),
    );
    std::fs::create_dir_all(&out).expect("the output folder");
    let steps: [(&str, StepFile); 6] = [
        ("09-new-script.fmp12", script_new),
        ("10-renamed-script.fmp12", script_rename),
        ("11-script-step-added.fmp12", script_add_step),
        ("12-duplicated-script.fmp12", script_duplicate),
        ("13-moved-script.fmp12", script_move),
        ("14-deleted-script.fmp12", script_delete),
    ];
    let mut readme = String::new();
    let mut failed = Vec::new();
    for (name, step) in steps {
        match step_file(&source, &out, name, &[step]) {
            Ok(lines) => readme.push_str(&format!("{name}\n  {}\n\n", lines.join("\n  "))),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    let all: Vec<StepFile> = steps.iter().map(|(_, step)| *step).collect();
    match step_file(&source, &out, "15-all-script-operations.fmp12", &all) {
        Ok(lines) => readme.push_str(&format!(
            "15-all-script-operations.fmp12 (the six operations above, in that order, on one copy)\n  {}\n\n",
            lines.join("\n  ")
        )),
        Err(e) => failed.push(format!("15-all-script-operations.fmp12: {e}")),
    }
    let _ = std::fs::remove_dir_all(out.join(".work"));
    std::fs::write(out.join("README.body"), &readme).expect("README");
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// File > Create New... (#712, #724): the result of making a blank file
/// with [`create_new`].
#[derive(Default, Serialize)]
pub(super) struct CreateNewCard {
    pub done: bool,
    pub detail: String,
}

/// File > Create New... (#712, #724): a blank file made by the command's
/// own operation (`om_data::new_file::create`), then through the provider
/// a text field added in Manage Database, placed on the file's layout, and
/// two records with a value in it; the file closed and opened again and
/// all of it read back. With `keep`, the file is copied there as
/// `Create New.fmp12`, for FileMaker's consistency check.
pub(super) fn create_new(dir: &Path, keep: Option<&Path>) -> CreateNewCard {
    const FIELD: &str = "Scorecard Note";
    const VALUES: [&str; 2] = ["first", "second"];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join("Create New.fmp12");
        om_data::new_file::create(&path).map_err(|e| format!("Create New: {e}"))?;
        let mut p = open(&path).map_err(|e| format!("opening the new file: {e}"))?;
        let (table, layout, occurrence) = {
            let schema = p
                .calc_schema()
                .map_err(|e| e.message)?
                .ok_or("no calculation schema")?;
            let layout = schema
                .layouts()
                .first()
                .cloned()
                .ok_or("the new file has no layout")?;
            let occurrence = layout.occurrence.ok_or("its layout shows no table")?;
            let table = schema.table_of(occurrence).ok_or("no table")?.id;
            (table, layout.id, occurrence)
        };
        let field = field_with_options(&mut *p, table, FIELD, |_, _| Ok(()))
            .map_err(|e| format!("Manage Database: {e}"))?;
        let (frame, expected) = free_frame(&mut *p, layout)?;
        p.place_layout_field(om_data::layout_placement::LayoutFieldPlacement {
            layout,
            table,
            placement: om_data::layout_placement::Placement {
                occurrence,
                field,
                frame,
                label: None,
            },
            expected,
        })
        .map_err(|e| format!("placing the field: {}", e.message))?;
        for value in VALUES {
            p.create_record(
                layout,
                om_data::FieldChanges::from([(FIELD.to_string(), Some(value.to_string()))]),
            )
            .map_err(|e| format!("New Record: {}", e.message))?;
        }
        p.close();
        drop(p);
        let mut p = open(&path).map_err(|e| format!("reopening: {e}"))?;
        let page = p.records(layout, 0, 100).map_err(|e| e.message)?;
        p.close();
        let column = page
            .fields
            .iter()
            .position(|f| f.name == FIELD)
            .ok_or("after reopening, the layout does not show the field")?;
        let read: Vec<String> = page
            .records
            .iter()
            .filter_map(|r| r.values.get(column).cloned().flatten())
            .filter(|v| !v.is_empty())
            .collect();
        if read != VALUES {
            return Err(format!("after reopening, {FIELD} reads {read:?}"));
        }
        if let Some(keep) = keep {
            std::fs::create_dir_all(keep).map_err(|e| e.to_string())?;
            std::fs::copy(&path, keep.join("Create New.fmp12")).map_err(|e| e.to_string())?;
        }
        Ok(format!(
            "a blank file, a text field “{FIELD}” on its layout, records “first” and “second” \
             read back after reopening ({} records with the blank file's own)",
            page.records.len()
        ))
    }))
    .map_err(|p| format!("panicked: {}", panic_text(p)))
    .and_then(|r| r);
    match result {
        Ok(detail) => CreateNewCard { done: true, detail },
        Err(detail) => CreateNewCard {
            done: false,
            detail,
        },
    }
}
