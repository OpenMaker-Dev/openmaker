//! FileMaker Pro 26's English starter solutions (`DynamicTemplates/*/en.fmp12`)
//! run end to end through the desktop window (ROADMAP.md, alpha criterion
//! 5). The scorecard's script measure (`scorecard.rs`) counts the steps
//! the runtime supports without running one; this runs them: every script
//! the Scripts menu lists and every script button on every layout, each on
//! a fresh disposable copy seeded with two records.
//!
//! Each run records the layout, mode, record and windows before and after,
//! what the script left on screen (an alert, a custom dialog, a
//! confirmation, ...) and how it ended: `ended`, `paused` (a script waiting
//! for the user, such as a card window), `prompt` (waiting on a dialog),
//! `alert` (an error alert), `did not finish` within the time limit, or
//! `not on screen` for a button the layout does not show without data or
//! a click first (in a portal without rows, a closed popover, a hidden
//! panel). The report goes to `OM_SCORECARD_OUT/starter-runs.json` and a
//! table on stdout. It is a measure, not a pass/fail test: nothing here is
//! compared with FileMaker, and the expected end of each run is read from
//! its steps by whoever reads the report. One outcome fails it: `card not
//! drawn`, a card window the run opened that the rendered window does not
//! show over its parent (#865); `card not checked` is a card a run opened
//! without a window step to expect it, so nothing was rendered to compare.
//! Controls are found and clicked through the accessibility tree, which
//! has a card's controls whether it is painted or not.
//!
//! Ignored by default: the templates are git-ignored inputs (`re/extracted`,
//! docs/development.md). Run with
//!   OM_TEMPLATES=<DynamicTemplates folder> OM_SCORECARD_OUT=<folder> \
//!     xvfb-run -a cargo test -p om-gui --test starter_runs -- --ignored --nocapture
//! `OM_STARTER_RUNS` (comma-separated solution names) limits the solutions.
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::App;
use egui::accesskit::Toggled;
use egui::Role;
use egui_kittest::kittest::{By, NodeT, Queryable};
use egui_kittest::Harness;
use om_data::calc::{Ctx, Date};
use om_data::host::{CalcNeed, CalcSchema, RecordValues};
use om_data::meta::JoinOp;
use om_data::{DataProvider, FieldChanges, LocalFile, LocalOpen, ScriptStep, ScriptWrite};
use om_gui::licensing::{LicenseKey, LicenseStore};
use om_gui::OpenMakerApp;
use om_layout::{ObjectKind, TextRun};
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use serde::Serialize;

const TEMPLATES: [&str; 2] = [
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    "../../re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/DynamicTemplates",
];

/// Each solution's details layout and the field its two seeded records
/// are named in (as `starter_finds` seeds them).
const SOLUTIONS: [(&str, &str, &str); 6] = [
    ("Assets", "Asset Details", "Name"),
    ("Contacts", "Contact Details", "First Name"),
    ("Content_Management", "Content Details", "Name"),
    ("Inventory", "Product Details", "Name"),
    ("Meetings", "Meeting Details", "Title"),
    ("Tasks", "Task Details", "Task"),
];

/// How long a run may take before it counts as not finishing.
const LIMIT: Duration = Duration::from_secs(20);

/// A store the window must not need: local files take no license.
struct NoLicense;

impl LicenseStore for NoLicense {
    fn load(&mut self) -> Result<Option<LicenseKey>, om_data::DataError> {
        Ok(None)
    }
    fn save(&mut self, _: &LicenseKey) -> Result<(), om_data::DataError> {
        Ok(())
    }
    fn remove(&mut self) -> Result<(), om_data::DataError> {
        Ok(())
    }
}

fn provider() -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: Vec::new(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    })
}

/// The template `solution` (its English file): in `OM_TEMPLATES` (a
/// `DynamicTemplates` folder) or this checkout's `re/extracted`.
fn template(solution: &str) -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::env::var_os("OM_TEMPLATES")
        .map(PathBuf::from)
        .into_iter()
        .chain(TEMPLATES.iter().map(|dir| manifest.join(dir)))
        .map(|dir| dir.join(solution).join("en.fmp12"))
        .find(|path| path.is_file())
}

fn open_native(path: &Path) -> NativeProvider {
    let mut provider = provider();
    let opened = provider
        .open_local(&LocalFile {
            path: path.to_owned(),
            credentials: Some(("Admin".into(), String::new().into())),
        })
        .unwrap();
    assert!(matches!(opened, LocalOpen::Opened { read_only: None }));
    provider
}

/// A disposable copy of `solution` in `dir` with two records named Alpha
/// and Beta in `field` of layout `layout`.
fn seeded(dir: &Path, solution: &str, layout: &str, field: &str) -> PathBuf {
    let path = dir.join(format!("{solution}.fmp12"));
    std::fs::copy(template(solution).expect("the template"), &path).unwrap();
    let mut provider = open_native(&path);
    let layout = provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == layout)
        .unwrap()
        .id;
    for name in ["Alpha", "Beta"] {
        let values: FieldChanges = [(field.to_owned(), Some(name.to_owned()))]
            .into_iter()
            .collect();
        provider.create_record(layout, values).unwrap();
    }
    seed_related(&mut provider);
    provider.close();
    path
}

/// Every record of base table `table`, as the calculation host reads it.
fn records(provider: &mut NativeProvider, table: u64) -> BTreeMap<u64, RecordValues> {
    let Ok(data) = provider.calc_data(&[CalcNeed::Records { table }]) else {
        return BTreeMap::new();
    };
    let ids: Vec<u64> = data
        .tables
        .get(&table)
        .map(|ids| ids.to_vec())
        .unwrap_or_default();
    let needs: Vec<CalcNeed> = ids
        .iter()
        .map(|&record| CalcNeed::Record { table, record })
        .collect();
    let Ok(data) = provider.calc_data(&needs) else {
        return BTreeMap::new();
    };
    ids.into_iter()
        .filter_map(|id| Some((id, (*data.records.get(&(table, id))?.clone()?).clone())))
        .collect()
}

fn text(schema: &CalcSchema, values: &RecordValues, field: u64) -> String {
    let cx = Ctx::new(
        &schema.locale,
        Date::from_ymd(2026, 11, 14).expect("a date"),
    );
    values
        .get(field, 1)
        .map(|v| v.to_text(&cx).as_str().to_owned())
        .unwrap_or_default()
}

/// A record in every base table a layout shows that has none, then one
/// related record for each record a portal's parent layout shows, through
/// a direct relationship's equal match fields (as the scorecard's trust
/// measure writes a portal row), so portal rows and their buttons are on
/// screen. What fails to write is left out.
fn seed_related(provider: &mut NativeProvider) {
    let Ok(Some(schema)) = provider.calc_schema() else {
        return;
    };
    let mut tables = BTreeSet::new();
    for entry in schema.layouts() {
        let Some(table) = entry.occurrence.and_then(|o| schema.table_of(o)) else {
            continue;
        };
        if tables.insert(table.id) && records(provider, table.id).is_empty() {
            let _ = provider.create_record(entry.id, FieldChanges::new());
        }
    }
    let mut done = BTreeSet::new();
    for entry in schema.layouts() {
        let Some(own) = entry.occurrence else {
            continue;
        };
        let Some(parent) = schema.table_of(own).map(|t| t.id) else {
            continue;
        };
        let Ok(Some(render)) = provider.render_layout(entry.id) else {
            continue;
        };
        for portal in render
            .layout
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
            let Some(table) = schema.table_of(occurrence).map(|t| t.id) else {
                continue;
            };
            let Some(path) = schema.path(own, occurrence) else {
                continue;
            };
            let [hop] = path.as_slice() else {
                continue;
            };
            let matches: Vec<(u64, u64)> = hop
                .predicates
                .iter()
                .filter(|p| p.op == JoinOp::Equal)
                .map(|p| (p.left, p.right))
                .collect();
            if matches.is_empty() || !done.insert((parent, occurrence)) {
                continue;
            }
            for values in records(provider, parent).values().take(2) {
                let cells = matches
                    .iter()
                    .map(|&(left, right)| {
                        let key = text(&schema, values, left);
                        (right, (!key.is_empty()).then_some(key))
                    })
                    .collect();
                let _ = provider.write_record(&ScriptWrite {
                    layout: entry.id,
                    table: Some(table),
                    record: None,
                    cells,
                    fields: FieldChanges::new(),
                    typed: BTreeSet::new(),
                    skip_validation: true,
                    privilege_set: None,
                    record_number: None,
                    prepared: None,
                    single_field_step: false,
                });
            }
        }
    }
}

/// What a solution offers to run: its Scripts menu, and each layout's
/// script buttons by the label the window gives them.
struct Offered {
    menu: Vec<String>,
    /// Every script trigger: where it is (`file`, `layout L`, `object O
    /// on L`), its event and its script.
    triggers: Vec<(String, String, u64)>,
    /// Each object with object triggers on a layout: what to do to fire
    /// them (a field to type into, or a tab to switch to).
    trigger_objects: Vec<(String, Target)>,
    script_names: BTreeMap<u64, String>,
    /// Each layout with script buttons: its folders in the layout menu
    /// (`None` when the menu leaves it out), and each button's label with
    /// what it does (its step, and the script Perform Script names).
    buttons: Vec<(String, Option<Vec<String>>, Vec<Button>)>,
}

/// A layout's script button.
#[derive(Clone)]
struct Button {
    /// The label the window gives it.
    label: String,
    /// What it does.
    action: String,
    /// Its width and height, to tell it from another object with the same
    /// label (Content Management's details layout has two "Content").
    size: (f32, f32),
    /// The layouts it goes to: a Go to Layout's target, or a New Window's
    /// or Go to Layout's in the script it performs.
    opens: Vec<String>,
    /// The tabs to bring its panels to the front first, outermost first:
    /// each panel's tab label and width.
    tabs: Vec<(String, f32)>,
    /// It may open a window: a New Window or Go to Related Record in a new
    /// window in the script it performs or one that script performs.
    opens_window: bool,
}

/// An object to act on so its object triggers fire.
#[derive(Clone)]
enum Target {
    /// A field: click into it, type, and Tab out (OnObjectEnter, Modify,
    /// Keystroke, Validate, Save, Exit).
    Field {
        label: String,
        size: (f32, f32),
        tabs: Vec<(String, f32)>,
    },
    /// A tab panel: switch to it (OnPanelSwitch).
    Tab { tabs: Vec<(String, f32)> },
}

/// The tabs whose panels hold object `id`, outermost first.
fn tab_chain(objects: &[om_layout::LayoutObject], id: u64) -> Vec<(String, f32)> {
    let mut chain = Vec::new();
    let mut at = id;
    let mut depth = 0;
    while let Some(parent) = objects.iter().find(|p| p.children.contains(&at)) {
        if let Some(panel) = &parent.tab_panel {
            chain.push((
                parent.text.clone().unwrap_or_default(),
                f32::from(panel.tab_width.max(1) as u16),
            ));
        }
        at = parent.id;
        depth += 1;
        if depth > 64 {
            break;
        }
    }
    chain.reverse();
    chain
}

/// The layout a Go to Layout (stored layout, step value 0 or 5) or New
/// Window step names.
fn layout_target(step: &ScriptStep, names: &BTreeMap<u64, String>) -> Option<String> {
    match (step.id(), step.value) {
        (6, 0 | 5) | (122, _) => names.get(&step.member_key()?.id()?).cloned(),
        _ => None,
    }
}

/// Each layout the layout menu shows, with the folders it is in
/// (`layout_menu.rs`: folders `0x1` to their end `0x2`; `0x8` leaves a
/// layout out, `0x10` is a separator).
fn menu_paths(provider: &mut NativeProvider) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut paths = std::collections::BTreeMap::new();
    let mut folders: Vec<String> = Vec::new();
    for entry in provider.layout_menu().unwrap() {
        if entry.flags & 1 != 0 {
            folders.push(entry.name.clone());
        } else if entry.flags & 2 != 0 {
            folders.pop();
        } else if entry.flags & (8 | 0x10) == 0 {
            paths.insert(entry.name.clone(), folders.clone());
        }
    }
    paths
}

fn offered(path: &Path, (seed_layout, seed_field): (&str, &str)) -> Offered {
    let mut provider = open_native(path);
    let schema = provider.calc_schema().unwrap();
    let catalog = provider.scripts().unwrap();
    let script_names: BTreeMap<u64, String> = catalog
        .scripts
        .iter()
        .map(|s| (s.id, s.name.clone()))
        .collect();
    // The scripts that may open a window, through the scripts they perform.
    let mut windowed = BTreeSet::new();
    for _ in 0..catalog.scripts.len() {
        let before = windowed.len();
        for script in &catalog.scripts {
            let opens = script.steps.iter().any(|step| {
                opens_window(step) || step.script_id().is_some_and(|id| windowed.contains(&id))
            });
            if opens {
                windowed.insert(script.id);
            }
        }
        if windowed.len() == before {
            break;
        }
    }
    let event_name = |id: u16| {
        om_scripts::runtime::TriggerEvent::from_id(id)
            .map_or_else(|| format!("event {id}"), |e| format!("{e:?}"))
    };
    let mut triggers = Vec::new();
    let mut trigger_objects = Vec::new();
    if let Some(options) = schema.as_ref().and_then(|s| s.file_options.as_ref()) {
        for (event, script) in [
            ("FirstWindowOpen", options.first_window_open),
            ("LastWindowClose", options.last_window_close),
            ("WindowOpen", options.window_open),
            ("WindowClose", options.window_close),
            ("FileAvPlayerChange", options.av_player_change),
            ("WindowTransaction", options.window_transaction),
        ] {
            if let Some(script) = script {
                triggers.push(("file".to_owned(), event.to_owned(), script));
            }
        }
    }
    let script_name = |id: u64| {
        catalog
            .scripts
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| format!("script {id}"), |s| s.name.clone())
    };
    let menu = catalog
        .scripts
        .iter()
        .filter(|s| s.kind == om_data::ScriptKind::Script && s.in_scripts_menu())
        .map(|s| s.name.clone())
        .collect();
    let paths = menu_paths(&mut provider);
    let layouts = provider.layouts().unwrap();
    let layout_names: std::collections::BTreeMap<u64, String> = layouts
        .iter()
        .map(|l| (u64::from(l.id), l.name.clone()))
        .collect();
    let mut buttons = Vec::new();
    // The seeded table: a merge field of its layouts reads the seeded name.
    let table_of = |name: &str| {
        let schema = schema.as_ref()?;
        let entry = schema.layouts().iter().find(|l| l.name == name)?;
        Some(schema.table_of(entry.occurrence?)?.id)
    };
    let seeded_table = table_of(seed_layout);
    for layout in layouts {
        let Some(render) = provider.render_layout(layout.id).unwrap() else {
            continue;
        };
        let seeded_here = seeded_table.is_some() && table_of(&layout.name) == seeded_table;
        // The seeded value shows only in the layout's own table's field: a
        // related table's field of the same name (Assets' header button
        // `<<Assignments::Name>>`) reads that table.
        let own = schema
            .as_ref()
            .and_then(|schema| {
                let entry = schema.layouts().iter().find(|l| l.name == layout.name)?;
                schema.occurrence(entry.occurrence?)
            })
            .map(|occurrence| format!("{}::{seed_field}", occurrence.name));
        for t in &render.layout.triggers {
            if let Some(script) = t.script {
                triggers.push((
                    format!("layout {}", layout.name),
                    event_name(t.event),
                    script,
                ));
            }
        }
        let objects = &render.layout.objects.objects;
        for o in objects.iter().filter(|o| !o.triggers.is_empty()) {
            let name = o.name.clone().unwrap_or_else(|| format!("object {}", o.id));
            for t in &o.triggers {
                if let Some(script) = t.script {
                    triggers.push((
                        format!("{name} on {}", layout.name),
                        event_name(t.event),
                        script,
                    ));
                }
            }
            let tabs = tab_chain(objects, o.id);
            let target = if o.tab_panel.is_some() {
                let mut tabs = tabs;
                tabs.push((
                    o.text.clone().unwrap_or_default(),
                    f32::from(o.tab_panel.as_ref().map_or(1, |p| p.tab_width.max(1)) as u16),
                ));
                Some(Target::Tab { tabs })
            } else {
                o.field_spec()
                    .and_then(|spec| {
                        let schema = schema.as_ref()?;
                        let occurrence = om_layout::occurrence_id(spec.table_key)?;
                        let table = schema.table_of(occurrence)?;
                        Some(table.fields.get(&spec.field_id()?)?.name.clone())
                    })
                    .map(|label| Target::Field {
                        label,
                        size: o.bounds.as_ref().map_or((0.0, 0.0), |b| {
                            ((b.right - b.left) as f32, (b.bottom - b.top) as f32)
                        }),
                        tabs,
                    })
            };
            if let Some(target) = target {
                trigger_objects.push((layout.name.clone(), target));
            }
        }
        let labels: Vec<Button> = render
            .layout
            .objects
            .objects
            .iter()
            .filter_map(|o| Some((o, ScriptStep::from_layout(o.script_step()?)?)))
            .map(|(o, step)| {
                let action = match (step.id(), step.script_id()) {
                    // Perform Script: the script and its steps' names.
                    (_, Some(id)) => {
                        let steps = catalog
                            .scripts
                            .iter()
                            .find(|s| s.id == id)
                            .map(|s| {
                                s.steps
                                    .iter()
                                    .map(|s| s.name().to_owned())
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            })
                            .unwrap_or_default();
                        // The step's line shows its parameter.
                        let line = om_data::scripts::display::step_line(
                            &step,
                            &om_data::scripts::display::NoStepNames,
                        )
                        .text;
                        let parameter = line
                            .split("Parameter: ")
                            .nth(1)
                            .map(|p| format!(" [{}", p))
                            .unwrap_or_default();
                        format!(
                            "{} \"{}\"{parameter} ({steps})",
                            step.name(),
                            script_name(id)
                        )
                    }
                    // Go to Layout: its target (script-steps.md 3.2).
                    (6, None) => match step.value {
                        1 => "Go to Layout [original layout]".into(),
                        3 | 4 => "Go to Layout [by calculation]".into(),
                        _ => {
                            let target = step
                                .member_key()
                                .and_then(|k| k.id())
                                .and_then(|id| layout_names.get(&id).cloned())
                                .unwrap_or_else(|| "<unknown>".into());
                            format!("Go to Layout [\"{target}\"]")
                        }
                    },
                    _ => step.name().to_owned(),
                };
                // The window labels a button by its text, a merge field by
                // the current record's value (the seeded name, here; other
                // fields read empty), or by its id (document.rs).
                let text: String = if o.text_runs.is_empty() {
                    o.text.clone().unwrap_or_default()
                } else {
                    o.text_runs
                        .iter()
                        .map(|run| match run {
                            TextRun::Literal(text) => text.as_str(),
                            TextRun::MergeField(name)
                                if seeded_here
                                    && (name == seed_field || own.as_deref() == Some(name)) =>
                            {
                                "Alpha"
                            }
                            _ => "",
                        })
                        .collect()
                };
                let label = if text.trim().is_empty() {
                    format!("Button {}", o.id)
                } else {
                    text
                };
                let size = o.bounds.as_ref().map_or((0.0, 0.0), |b| {
                    ((b.right - b.left) as f32, (b.bottom - b.top) as f32)
                });
                let opens = match step.script_id() {
                    Some(id) => catalog
                        .scripts
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| {
                            s.steps
                                .iter()
                                .filter_map(|s| layout_target(s, &layout_names))
                                .collect()
                        })
                        .unwrap_or_default(),
                    None => layout_target(&step, &layout_names).into_iter().collect(),
                };
                Button {
                    label,
                    action,
                    size,
                    opens,
                    tabs: tab_chain(objects, o.id),
                    opens_window: opens_window(&step)
                        || step.script_id().is_some_and(|id| windowed.contains(&id)),
                }
            })
            .collect();
        if !labels.is_empty() {
            buttons.push((
                layout.name.clone(),
                paths.get(&layout.name).cloned(),
                labels,
            ));
        }
    }
    provider.close();
    Offered {
        menu,
        triggers,
        trigger_objects,
        script_names,
        buttons,
    }
}

/// Bring the panels of `tabs` to the front, outermost first: of the tabs
/// with each label, the one nearest its width.
fn front_tabs(app: &mut App, tabs: &[(String, f32)]) {
    for (label, width) in tabs {
        let by = || By::new().role(Role::Tab).label(label);
        let nearest = app
            .query_all(by())
            .enumerate()
            .map(|(i, n)| (i, (n.rect().width() - width).abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        if let Some(i) = nearest {
            app.query_all(by()).nth(i).unwrap().click();
            app.run_steps(3);
            finish(app);
        }
    }
}

/// How a run reaches a layout from the file's first window: the layout
/// menu, then the buttons that open the next layout, in order.
#[derive(Clone)]
enum Hop {
    Menu(Vec<String>, String),
    Click(Button),
}

/// A route to each layout with buttons: through the layout menu, or
/// through a button on a layout already reached whose run opens it (a
/// card window's layout).
fn routes(plan: &Offered) -> BTreeMap<String, Vec<Hop>> {
    let mut routes: BTreeMap<String, Vec<Hop>> = plan
        .buttons
        .iter()
        .filter_map(|(layout, folders, _)| {
            Some((
                layout.clone(),
                vec![Hop::Menu(folders.clone()?, layout.clone())],
            ))
        })
        .collect();
    loop {
        let mut added = false;
        for (layout, _, buttons) in &plan.buttons {
            let Some(route) = routes.get(layout).cloned() else {
                continue;
            };
            for button in buttons {
                for target in &button.opens {
                    if !routes.contains_key(target) {
                        let mut to = route.clone();
                        to.push(Hop::Click(button.clone()));
                        routes.insert(target.clone(), to);
                        added = true;
                    }
                }
            }
        }
        if !added {
            return routes;
        }
    }
}

/// Click `button`: of the nodes with its label, the one nearest its size.
/// `false` when none is on screen.
fn click_button(app: &mut App, button: &Button) -> bool {
    front_tabs(app, &button.tabs);
    let by = || By::new().role(Role::Button).label(&button.label);
    let nearest = app
        .query_all(by())
        .enumerate()
        .map(|(i, node)| {
            let r = node.rect();
            let off = (r.width() - button.size.0).abs() + (r.height() - button.size.1).abs();
            (i, off)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i);
    match nearest {
        Some(i) => {
            app.query_all(by()).nth(i).unwrap().click();
            true
        }
        None => false,
    }
}

fn replace(app: &mut App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// File > Open `path` as Admin, whose password is empty, until it has
/// settled on the layout it opens on.
fn open(path: &Path) -> App {
    let window_provider = provider();
    let mut app = Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .build_eframe(move |_cc| {
            OpenMakerApp::with_license_store(Box::new(window_provider), Box::new(NoLicense))
        });
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    common::wait_for_label(&mut app, "Open File");
    replace(&mut app, "Path", &path.display().to_string());
    common::click(&mut app, "Open");
    // A starter opens as its File Options' account, with no sign-in
    // dialog; answer one if it comes.
    let stem = path.file_stem().unwrap().to_string_lossy();
    let sign_in = format!("Open “{stem}”");
    common::wait_until(&mut app, "the file or its sign-in", |app| {
        app.state().is_open() || app.query_by_label(&sign_in).is_some()
    });
    if app.query_by_label(&sign_in).is_some() {
        common::click(&mut app, "OK");
    }
    finish(&mut app);
    app
}

/// Where the window is.
#[derive(Serialize, Clone, PartialEq)]
struct Place {
    layout: String,
    mode: String,
    record: String,
    windows: Vec<String>,
    /// The label of the control that holds the cursor, if any.
    focus: String,
    /// The tab in front in each tab control shown, by its label: Go to
    /// Object on a tab panel or on an object in one brings it to the front
    /// in FileMaker Pro 26 (#866).
    tabs: Vec<String>,
}

fn place(app: &mut App) -> Place {
    Place {
        layout: app
            .query_by_label("Layout")
            .and_then(|n| n.value())
            .unwrap_or_default(),
        mode: format!("{:?}", app.state().mode()),
        record: common::find_text_field(app, "Record Number")
            .and_then(|n| n.value())
            .unwrap_or_default(),
        windows: app.state_mut().window_titles(),
        focus: app
            .query_all(By::new())
            .find(|n| n.is_focused())
            .map(|n| {
                let node = n.accesskit_node();
                format!(
                    "{:?} {}",
                    node.role(),
                    node.label().or_else(|| node.value()).unwrap_or_default()
                )
            })
            .unwrap_or_default(),
        tabs: app
            .query_all(By::new().role(Role::Tab))
            .filter(|n| n.accesskit_node().toggled() == Some(Toggled::True))
            .map(|n| n.accesskit_node().label().unwrap_or_default())
            .collect(),
    }
}

/// Step the window until the run ends, pauses, waits on a prompt or the
/// limit passes; the outcome and the prompt's text when it is an alert.
fn finish(app: &mut App) -> (String, Option<String>) {
    let deadline = Instant::now() + LIMIT;
    // A click lands on the next frame, and a script it starts in another
    // window (a card) may begin a frame later: settled must hold a while.
    let mut settled = 0;
    loop {
        app.step();
        let state = app.state();
        if let Some(kind) = state.script_prompt() {
            app.run_steps(2);
            let state = app.state();
            return match kind {
                "alert" => ("alert".into(), state.error().map(str::to_owned)),
                other => (format!("prompt: {other}"), None),
            };
        }
        if state.error().is_some() {
            return ("alert".into(), state.error().map(str::to_owned));
        }
        if state.script_paused() {
            app.run_steps(2);
            return ("paused".into(), None);
        }
        if state.is_settled() && !state.script_running() {
            settled += 1;
            if settled >= 10 {
                return ("ended".into(), None);
            }
        } else {
            settled = 0;
        }
        if Instant::now() > deadline {
            return ("did not finish".into(), None);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A step that opens a window: New Window, or Go to Related Record in a
/// new window.
fn opens_window(step: &ScriptStep) -> bool {
    step.id() == 122 || (step.id() == 74 && step.has_flag(om_scripts::flag::TURN_ON))
}

/// The outcome for a card the run opened (one not in `open`, the cards
/// open before it): `card not drawn` when it is not painted over its parent
/// in the window, against `before`, the window rendered before the run
/// (`common::card_painted`); `card not checked` when the run was not
/// expected to open one, so nothing was rendered before it (a trigger's
/// script, say). The accessibility tree has a card's controls whether it
/// is painted or not, so a run behind an undrawn card ends like any other
/// (#865).
fn card_outcome(
    app: &mut App,
    before: Option<&image::RgbaImage>,
    open: &[String],
) -> Option<String> {
    let cards: Vec<_> = (app.state().cards().into_iter())
        .filter(|card| !open.contains(&card.name))
        .collect();
    if cards.is_empty() {
        return None;
    }
    let Some(before) = before else {
        return Some("card not checked: no render before the run".into());
    };
    let after = common::rendered(app);
    let ppp = app.ctx.pixels_per_point();
    cards
        .iter()
        .find_map(|card| common::card_painted(before, &after, ppp, card).err())
        .map(|error| format!("card not drawn: {error}"))
}

#[derive(Serialize)]
struct Run {
    solution: String,
    /// `menu` or `button on <layout>`.
    from: String,
    name: String,
    /// What the button does, or the script's own name from the menu.
    action: String,
    outcome: String,
    alert: Option<String>,
    before: Place,
    after: Place,
    seconds: f32,
    /// The scripts the run started (`OpenMakerApp::scripts_started`): a
    /// trigger as its event and script, a button, a menu script.
    started: Vec<String>,
}

/// The starts logged since `from`.
fn started_since(app: &App, from: usize) -> Vec<String> {
    app.state()
        .scripts_started()
        .get(from..)
        .unwrap_or_default()
        .to_vec()
}

fn run_menu(dir: &Path, solution: &str, seed: (&str, &str), name: &str) -> Run {
    let path = seeded(dir, solution, seed.0, seed.1);
    let mut app = open(&path);
    let before = place(&mut app);
    let start = Instant::now();
    let logged = app.state().scripts_started().len();
    common::open_menu(&mut app, "Scripts");
    let found = app.query_all(By::new().label(name)).next().is_some();
    let (outcome, alert) = if found {
        app.query_all(By::new().label(name)).next().unwrap().click();
        finish(&mut app)
    } else {
        ("not on screen".into(), None)
    };
    Run {
        solution: solution.into(),
        from: "menu".into(),
        name: name.into(),
        action: format!("Perform Script \"{name}\""),
        outcome,
        alert,
        before,
        after: place(&mut app),
        seconds: start.elapsed().as_secs_f32(),
        started: started_since(&app, logged),
    }
}

/// The layout menu's `layout`, through its `folders`.
fn select(app: &mut App, folders: &[String], layout: &str) {
    common::click(app, "Layout");
    // The menu draws after the layout, whose objects may share a name
    // with a folder or a layout (Tasks' "Tasks" button): take the last.
    for folder in folders {
        if let Some(node) = app.query_all(By::new().label(folder)).last() {
            node.hover();
        }
        app.run_steps(3);
    }
    if let Some(node) = app.query_all(By::new().label(layout)).last() {
        node.click();
    }
    app.run_steps(2);
    finish(app);
}

fn run_button(
    dir: &Path,
    solution: &str,
    seed: (&str, &str),
    (layout, route): (&str, Option<&[Hop]>),
    button: &Button,
) -> Run {
    let (label, action) = (button.label.as_str(), button.action.as_str());
    let path = seeded(dir, solution, seed.0, seed.1);
    let mut app = open(&path);
    let mut reached = route.is_some();
    for hop in route.unwrap_or_default() {
        match hop {
            Hop::Menu(_, to) if place(&mut app).layout == *to => {}
            Hop::Menu(folders, to) => {
                select(&mut app, folders, to);
                reached &= place(&mut app).layout == *to;
            }
            Hop::Click(by) => {
                reached &= click_button(&mut app, by);
                finish(&mut app);
            }
        }
    }
    if !reached {
        let here = place(&mut app);
        return Run {
            solution: solution.into(),
            from: format!("button on {layout}"),
            name: label.into(),
            action: action.into(),
            outcome: "layout not reached".into(),
            alert: None,
            before: here.clone(),
            after: here,
            seconds: 0.0,
            started: Vec::new(),
        };
    }
    let before = place(&mut app);
    // Rendering is slow under software rendering: only before a button
    // that may open a card.
    let shown = button.opens_window.then(|| common::rendered(&mut app));
    let cards: Vec<String> = app.state().cards().into_iter().map(|c| c.name).collect();
    let start = Instant::now();
    let logged = app.state().scripts_started().len();
    let (mut outcome, alert) = if click_button(&mut app, button) {
        finish(&mut app)
    } else {
        ("not on screen".into(), None)
    };
    if let Some(card) = card_outcome(&mut app, shown.as_ref(), &cards) {
        outcome = card;
    }
    Run {
        solution: solution.into(),
        from: format!("button on {layout}"),
        name: label.into(),
        action: action.into(),
        outcome,
        alert,
        before,
        after: place(&mut app),
        seconds: start.elapsed().as_secs_f32(),
        started: started_since(&app, logged),
    }
}

/// File > Open itself: what the file's and its first layout's triggers
/// started.
fn run_open(dir: &Path, solution: &str, seed: (&str, &str)) -> Run {
    let path = seeded(dir, solution, seed.0, seed.1);
    let start = Instant::now();
    let mut app = open(&path);
    let after = place(&mut app);
    let (outcome, alert) = match app.state().error() {
        Some(alert) => ("alert".into(), Some(alert.to_owned())),
        None => ("ended".into(), None),
    };
    Run {
        solution: solution.into(),
        from: "open".into(),
        name: "File > Open".into(),
        action: String::new(),
        outcome,
        alert,
        before: after.clone(),
        after,
        seconds: start.elapsed().as_secs_f32(),
        started: started_since(&app, 0),
    }
}

/// Layout `layout` from the layout menu (OnLayoutExit, OnLayoutEnter,
/// OnRecordLoad), then Find mode and back (OnModeExit, OnModeEnter).
fn run_layout(
    dir: &Path,
    solution: &str,
    seed: (&str, &str),
    folders: &[String],
    layout: &str,
) -> Run {
    let path = seeded(dir, solution, seed.0, seed.1);
    let mut app = open(&path);
    let before = place(&mut app);
    let start = Instant::now();
    let logged = app.state().scripts_started().len();
    if before.layout != layout {
        select(&mut app, folders, layout);
    }
    let mut outcome = finish(&mut app);
    if outcome.0 == "ended" {
        common::click(&mut app, "Find");
        finish(&mut app);
        app.key_press(egui::Key::Escape);
        app.run_steps(2);
        outcome = finish(&mut app);
    }
    Run {
        solution: solution.into(),
        from: "layout menu".into(),
        name: layout.into(),
        action: "Go to the layout, Find mode, Escape".into(),
        outcome: outcome.0,
        alert: outcome.1,
        before,
        after: place(&mut app),
        seconds: start.elapsed().as_secs_f32(),
        started: started_since(&app, logged),
    }
}

/// An object with object triggers on `layout`: type into its field and
/// Tab out, or switch to its tab panel.
fn run_object(
    dir: &Path,
    solution: &str,
    seed: (&str, &str),
    (layout, route): (&str, Option<&[Hop]>),
    target: &Target,
) -> Run {
    let path = seeded(dir, solution, seed.0, seed.1);
    let mut app = open(&path);
    let name = match target {
        Target::Field { label, .. } => label.clone(),
        Target::Tab { tabs } => tabs.last().map(|t| t.0.clone()).unwrap_or_default(),
    };
    let mut reached = route.is_some();
    for hop in route.unwrap_or_default() {
        match hop {
            Hop::Menu(_, to) if place(&mut app).layout == *to => {}
            Hop::Menu(folders, to) => {
                select(&mut app, folders, to);
                reached &= place(&mut app).layout == *to;
            }
            Hop::Click(by) => {
                reached &= click_button(&mut app, by);
                finish(&mut app);
            }
        }
    }
    let before = place(&mut app);
    let start = Instant::now();
    let logged = app.state().scripts_started().len();
    let (outcome, alert) = if !reached {
        ("layout not reached".into(), None)
    } else {
        match target {
            Target::Tab { tabs } => {
                front_tabs(&mut app, tabs);
                finish(&mut app)
            }
            Target::Field { label, size, tabs } => {
                front_tabs(&mut app, tabs);
                let by = || By::new().label(label);
                let nearest = app
                    .query_all(by())
                    .filter(|n| {
                        matches!(
                            n.accesskit_node().role(),
                            Role::TextInput | Role::ComboBox | Role::MultilineTextInput
                        )
                    })
                    .enumerate()
                    .map(|(i, n)| {
                        let r = n.rect();
                        (i, (r.width() - size.0).abs() + (r.height() - size.1).abs())
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i);
                match nearest {
                    Some(i) => {
                        app.query_all(by())
                            .filter(|n| {
                                matches!(
                                    n.accesskit_node().role(),
                                    Role::TextInput | Role::ComboBox | Role::MultilineTextInput
                                )
                            })
                            .nth(i)
                            .unwrap()
                            .click();
                        app.run_steps(3);
                        finish(&mut app);
                        app.event(egui::Event::Text("1".into()));
                        app.run_steps(2);
                        app.key_press(egui::Key::Tab);
                        app.run_steps(2);
                        finish(&mut app)
                    }
                    None => ("not on screen".into(), None),
                }
            }
        }
    };
    Run {
        solution: solution.into(),
        from: format!("object on {layout}"),
        name,
        action: match target {
            Target::Field { .. } => "click, type, Tab".into(),
            Target::Tab { .. } => "switch to its tab".into(),
        },
        outcome,
        alert,
        before,
        after: place(&mut app),
        seconds: start.elapsed().as_secs_f32(),
        started: started_since(&app, logged),
    }
}

/// A trigger, and what the runs saw of it.
#[derive(Serialize)]
struct TriggerSeen {
    solution: String,
    place: String,
    event: String,
    script: String,
    /// The runs that started its script for its event.
    fired_in: Vec<String>,
    /// Every run it fired in ended (or waits on a prompt) with no alert.
    ended: bool,
}

#[test]
#[ignore = "requires the starter templates under OM_TEMPLATES or re/extracted"]
fn the_starter_solutions_scripts_and_buttons_run() {
    let only: Option<Vec<String>> = std::env::var("OM_STARTER_RUNS")
        .ok()
        .map(|s| s.split(',').map(|s| s.trim().to_owned()).collect());
    let mut runs = Vec::new();
    let mut seen = Vec::new();
    for (solution, layout, field) in SOLUTIONS {
        if only
            .as_ref()
            .is_some_and(|o| !o.iter().any(|s| s == solution))
        {
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let plan = offered(
            &seeded(dir.path(), solution, layout, field),
            (layout, field),
        );
        let first = runs.len();
        {
            let dir = tempfile::tempdir().unwrap();
            runs.push(run_open(dir.path(), solution, (layout, field)));
        }
        for name in &plan.menu {
            let dir = tempfile::tempdir().unwrap();
            runs.push(run_menu(dir.path(), solution, (layout, field), name));
        }
        let routes = routes(&plan);
        let with_triggers: BTreeSet<&str> = plan
            .triggers
            .iter()
            .filter_map(|(place, _, _)| place.strip_prefix("layout "))
            .collect();
        for (on, folders, _) in &plan.buttons {
            if let (Some(folders), true) = (folders, with_triggers.contains(on.as_str())) {
                let dir = tempfile::tempdir().unwrap();
                runs.push(run_layout(
                    dir.path(),
                    solution,
                    (layout, field),
                    folders,
                    on,
                ));
            }
        }
        for (on, target) in &plan.trigger_objects {
            let dir = tempfile::tempdir().unwrap();
            runs.push(run_object(
                dir.path(),
                solution,
                (layout, field),
                (on, routes.get(on).map(Vec::as_slice)),
                target,
            ));
        }
        for (on, _, labels) in &plan.buttons {
            for button in labels {
                let dir = tempfile::tempdir().unwrap();
                runs.push(run_button(
                    dir.path(),
                    solution,
                    (layout, field),
                    (on, routes.get(on).map(Vec::as_slice)),
                    button,
                ));
            }
        }
        for (place, event, script) in &plan.triggers {
            let key = format!("{event} {script}");
            let fired: Vec<&Run> = runs[first..]
                .iter()
                .filter(|r| r.started.contains(&key))
                .collect();
            seen.push(TriggerSeen {
                solution: solution.into(),
                place: place.clone(),
                event: event.clone(),
                script: plan
                    .script_names
                    .get(script)
                    .cloned()
                    .unwrap_or_else(|| format!("script {script}")),
                fired_in: fired
                    .iter()
                    .map(|r| format!("{} {}", r.from, r.name))
                    .collect(),
                ended: fired
                    .iter()
                    .all(|r| r.alert.is_none() && r.outcome != "did not finish"),
            });
        }
    }
    for run in &runs {
        let moved = if run.before == run.after {
            String::new()
        } else {
            format!(
                " -> {} [{}] #{} windows {:?} focus {:?} tabs {:?}",
                run.after.layout,
                run.after.mode,
                run.after.record,
                run.after.windows,
                run.after.focus,
                run.after.tabs
            )
        };
        println!(
            "{:<20} {:<30} {:<16} {:<44} {:<16}{}{}",
            run.solution,
            run.from,
            run.name,
            run.action,
            run.outcome,
            moved,
            run.alert
                .as_deref()
                .map(|a| format!(" alert: {}", a.replace('\n', " ")))
                .unwrap_or_default()
        );
    }
    for t in &seen {
        println!(
            "TRIGGER {:<20} {:<40} {:<16} {:<28} fired {} {}",
            t.solution,
            t.place,
            t.event,
            t.script,
            t.fired_in.len(),
            if t.fired_in.is_empty() {
                ""
            } else if t.ended {
                "ended"
            } else {
                "alert or unfinished"
            }
        );
    }
    if let Some(out) = std::env::var_os("OM_SCORECARD_OUT") {
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("starter-runs.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "runs": runs, "triggers": seen }))
                .unwrap(),
        )
        .unwrap();
    }
    assert!(!runs.is_empty(), "no starter solution found");
    // Unlike the other outcomes, a card the window does not paint is a
    // failure: the user is left with a dimmed, locked window (#865).
    let undrawn: Vec<String> = (runs.iter())
        .filter(|run| run.outcome.starts_with("card not drawn"))
        .map(|run| {
            format!(
                "{} {} {}: {}",
                run.solution, run.from, run.name, run.outcome
            )
        })
        .collect();
    assert!(undrawn.is_empty(), "{undrawn:#?}");
}
