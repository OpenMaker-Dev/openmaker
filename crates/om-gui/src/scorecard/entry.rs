//! Entry (alpha criterion 2): a user of each given file enters a record
//! through the desktop window. For each base table, on the layout with the
//! most field objects FileMaker Pro lets the user change (the scorecard's
//! `field_card`) among those the layout menu offers, the window opens a
//! copy as File > Open does, goes to the layout from the layout bar, adds
//! a record with the toolbar's New Record and fills each such object with
//! the mouse and keyboard:
//!
//! - an edit box: typing a value of its field's type;
//! - a drop-down list: its list opened and its first item chosen;
//! - a pop-up menu: its menu opened and its first item chosen;
//! - a checkbox or radio button set: its first item clicked;
//! - a drop-down calendar: its calendar opened and the 15th picked;
//! - a container: selected, then Insert > Picture... with a small PNG;
//! - a field of a portal: in the portal's creation row, where its
//!   relationship allows creation (else FileMaker shows no row to type in).
//!
//! The user moves between objects by clicking. Then Ctrl+Enter commits
//! the record (the window has no Save Record button, #871), and the
//! window closes. The file is read back twice: through the provider (what
//! the layout's own fields store) and in a new window at the same record
//! (what each object shows), and both compared with what was shown before
//! closing. Layouts without portals go first: a portal row's list often
//! offers another table's records, which they add.
//!
//! Two more rows, once a run, on the first layout whose tab order has two
//! text edit boxes in a row: a click in the first, typing, Tab and typing
//! again, on a new record (then committed) and in a find request; the
//! second text should be in the second field, as FileMaker Pro 26 moves
//! (#848, #858).
//!
//! It writes `entry.md` and `entry.json`, and keeps each file's copy as the
//! window left it in `saved/<n>/`, for FileMaker Pro to open.
//!
//! What it leaves out: nothing is compared with FileMaker Pro. Layouts
//! the layout menu hides are reached only through the file's buttons and
//! scripts, which this measure does not click. An object on a tab panel
//! not shown is reached by clicking each tab in turn; one on a slide panel
//! or in a popover counts as not drawn. A list with no items (a list of a
//! field's values, with no records yet) offers nothing to choose, and the
//! object is typed into instead. A dialog a trigger or validation shows is
//! noted and answered with OK. Tab is measured in Form view only, between
//! two fields of the layout's own table: not in Table view or portal rows.
//!
//! ```text
//! OM_SCORECARD_FILES=a.fmp12:b.fmp12 OM_SCORECARD_OUT=<folder> \
//!   cargo test -p om-gui --lib scorecard::entry::starter_entry -- --ignored --exact --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Pos2, Rect, Role};
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use egui_kittest::Harness;
use om_data::calc::ValueType;
use serde::Serialize;

use crate::OpenMakerApp;

type App = Harness<'static, OpenMakerApp>;

/// A store the window must not need: local files take no license.
struct NoLicense;

impl crate::licensing::LicenseStore for NoLicense {
    fn load(&mut self) -> Result<Option<crate::licensing::LicenseKey>, om_data::DataError> {
        Ok(None)
    }
    fn save(&mut self, _: &crate::licensing::LicenseKey) -> Result<(), om_data::DataError> {
        Ok(())
    }
    fn remove(&mut self) -> Result<(), om_data::DataError> {
        Ok(())
    }
}

/// A field object to fill.
#[derive(Clone)]
struct Target {
    object: u64,
    /// Its accessible name: the field's binding.
    label: String,
    control: &'static str,
    kind: ValueType,
    /// The portal's table occurrence, for a field in a portal.
    portal: Option<String>,
    /// Its place on the layout.
    top: f64,
    left: f64,
}

/// A layout to enter a record on.
struct Plan {
    id: u32,
    name: String,
    table: String,
    targets: Vec<Target>,
    /// Field objects FileMaker lets the user change that are not filled:
    /// portal fields without a creation row.
    skipped: usize,
    /// Two text edit boxes of the layout's own table, one right after the
    /// other in the layout's tab order: indexes into `targets`.
    tab: Option<(usize, usize)>,
}

#[derive(Serialize)]
struct Row {
    file: String,
    layout: String,
    field: String,
    control: &'static str,
    kind: String,
    portal: Option<String>,
    /// What the user did.
    entered: Option<String>,
    /// What the object showed after the commit.
    shown: Option<String>,
    /// What the provider read for the layout's own field after reopening.
    stored: Option<String>,
    /// What the object showed in a new window.
    reopened: Option<String>,
    /// Dialogs and errors while filling it.
    notes: Vec<String>,
    failure: Option<String>,
    /// Portal readback observations before and after pending data completes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    readback: Vec<Readback>,
}

#[derive(Serialize)]
struct Readback {
    phase: &'static str,
    /// A toolbar position is not a stored record id.
    record_number: Option<String>,
    /// Highest stored root-table id after save, not the shown or portal row id.
    stored_root_record: Option<i64>,
    settled: bool,
    script_running: bool,
    selected_rect: Option<[f32; 4]>,
    candidates: Vec<ReadbackControl>,
}

#[derive(Serialize)]
struct ReadbackControl {
    rect: [f32; 4],
    value: Option<String>,
}

#[derive(Serialize)]
struct LayoutRow {
    file: String,
    layout: String,
    table: String,
    objects: usize,
    skipped: usize,
    error: Option<String>,
}

#[test]
#[ignore = "requires .fmp12 files: OM_SCORECARD_FILES (a path list) and OM_SCORECARD_OUT (a folder)"]
fn starter_entry() {
    let files: Vec<PathBuf> = std::env::split_paths(
        &std::env::var_os("OM_SCORECARD_FILES").expect("set OM_SCORECARD_FILES"),
    )
    .collect();
    let out = PathBuf::from(std::env::var_os("OM_SCORECARD_OUT").expect("set OM_SCORECARD_OUT"));
    let temp = tempfile::tempdir().expect("temporary folder");
    let png = temp.path().join("entry.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 60, 40]))
        .save(&png)
        .expect("write the picture");
    let mut rows = Vec::new();
    let mut layouts = Vec::new();
    let mut tab: Vec<Row> = Vec::new();
    for (i, path) in files.iter().enumerate() {
        let file = super::label(path);
        let name = format!("{}.fmp12", file.split('/').next().unwrap_or("file"));
        let dir = temp.path().join(i.to_string());
        std::fs::create_dir_all(&dir).expect("copy folder");
        let copy = dir.join(&name);
        std::fs::copy(path, &copy).expect("copy the file");
        let plans = match plan(&copy) {
            Ok(plans) => plans,
            Err(error) => {
                layouts.push(LayoutRow {
                    file,
                    layout: String::new(),
                    table: String::new(),
                    objects: 0,
                    skipped: 0,
                    error: Some(error),
                });
                continue;
            }
        };
        for plan in &plans {
            eprintln!(
                "entry: {file} {} ({} objects)",
                plan.name,
                plan.targets.len()
            );
            let (mut layout_rows, error) = measure(&copy, plan, &png);
            for row in &mut layout_rows {
                row.file.clone_from(&file);
            }
            rows.extend(layout_rows);
            if let (true, Some(pair)) = (tab.is_empty(), plan.tab) {
                for find_mode in [false, true] {
                    let mut row = tab_row(&copy, plan, pair, find_mode);
                    row.file.clone_from(&file);
                    tab.push(row);
                }
            }
            layouts.push(LayoutRow {
                file: file.clone(),
                layout: plan.name.clone(),
                table: plan.table.clone(),
                objects: plan.targets.len(),
                skipped: plan.skipped,
                error,
            });
        }
        // The copy as the window saved it, for FileMaker Pro to open (#911).
        let saved = out.join("saved").join(i.to_string());
        std::fs::create_dir_all(&saved).expect("saved folder");
        let original = path
            .file_name()
            .map_or(name.clone().into(), ToOwned::to_owned);
        std::fs::copy(&copy, saved.join(original)).expect("keep the saved copy");
    }
    std::fs::create_dir_all(&out).expect("output folder");
    let json = serde_json::to_string_pretty(&(&layouts, &rows, &tab)).expect("JSON");
    std::fs::write(out.join("entry.json"), json).expect("write entry.json");
    let summary = summary(&layouts, &rows, &tab);
    std::fs::write(out.join("entry.md"), &summary).expect("write entry.md");
    println!("{summary}");
}

/// Exercise the original entry/save/reopen path repeatedly on independent
/// copies. A failure stops the run; an empty settled value remains a failure.
#[test]
#[ignore = "requires OM_ENTRY_FILE, OM_ENTRY_FIELD (a portal pop-up), and OM_SCORECARD_OUT"]
fn portal_popup_readback() {
    let file = PathBuf::from(std::env::var_os("OM_ENTRY_FILE").expect("set OM_ENTRY_FILE"));
    let field = std::env::var("OM_ENTRY_FIELD").expect("set OM_ENTRY_FIELD");
    let out = PathBuf::from(std::env::var_os("OM_SCORECARD_OUT").expect("set OM_SCORECARD_OUT"));
    std::fs::create_dir(&out).expect("a new output folder");
    let runs = std::env::var("OM_ENTRY_RUNS")
        .map(|n| n.parse::<usize>().expect("OM_ENTRY_RUNS must be a number"))
        .unwrap_or(20);
    assert!((1..=20).contains(&runs), "use 1 to 20 targeted runs");
    let temp = tempfile::tempdir().expect("temporary folder");
    let png = temp.path().join("entry.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 60, 40]))
        .save(&png)
        .expect("write the picture");
    let file_name = super::label(&file);
    let name = format!("{}.fmp12", file_name.split('/').next().unwrap_or("file"));
    for run in 1..=runs {
        let dir = temp.path().join(run.to_string());
        std::fs::create_dir(&dir).expect("copy folder");
        let copy = dir.join(&name);
        std::fs::copy(&file, &copy).expect("copy the unchanged input");
        let plan = plan(&copy)
            .expect("read the layouts")
            .into_iter()
            .find(|p| {
                p.targets
                    .iter()
                    .any(|t| t.label == field && t.portal.is_some() && t.control == "pop-up menu")
            })
            .expect("the file must offer the requested portal pop-up");
        let (rows, error) = measure(&copy, &plan, &png);
        let row = rows
            .iter()
            .find(|r| r.field == field && r.portal.is_some() && r.control == "pop-up menu")
            .expect("the target");
        let json = serde_json::to_string_pretty(&(&error, &rows)).expect("JSON");
        std::fs::write(out.join(format!("run-{run}.json")), json).expect("keep the observations");
        let saved = out.join(format!("run-{run}"));
        std::fs::create_dir(&saved).expect("saved copy folder");
        std::fs::copy(&copy, saved.join(&name)).expect("keep the saved copy");
        eprintln!(
            "portal pop-up run {run}/{runs}: {}",
            serde_json::to_string(row).expect("JSON")
        );
        assert!(error.is_none(), "run {run}: {error:?}");
        assert!(row.failure.is_none(), "run {run}: {:?}", row.failure);
        assert!(row.entered.is_some() && row.reopened.is_some());
    }
}

/// For each base table, the layout with the most field objects FileMaker
/// lets the user change, and those objects.
fn plan(copy: &Path) -> Result<Vec<Plan>, String> {
    let mut provider = super::build::open(copy)?;
    let schema = provider
        .calc_schema()
        .map_err(|e| e.message)?
        .ok_or("no calculation schema")?;
    let (tables, prohibited) = super::tables(copy, &schema)?;
    let read_only: BTreeSet<&str> = tables
        .iter()
        .filter(|t| !t.writable)
        .map(|t| t.name.as_str())
        .collect();
    // Separators and folders (`layout_menu.rs`), and layouts the menu
    // does not show (0x08).
    let structural: BTreeSet<u32> = provider
        .layout_menu()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.flags & (0x01 | 0x02 | 0x08 | 0x10) != 0)
        .map(|entry| entry.id)
        .collect();
    let mut best: BTreeMap<String, Plan> = BTreeMap::new();
    let mut buttons = Vec::new();
    let mut uses = super::Uses::default();
    let layouts = provider.layouts().map_err(|e| e.message)?;
    for info in layouts.into_iter().filter(|l| !structural.contains(&l.id)) {
        let Ok(Some(render)) = provider.render_layout(info.id) else {
            continue;
        };
        let alias = render.layout.def.as_ref().map_or(0, |d| d.table_alias_key);
        let Some(table) = om_layout::occurrence_id(alias)
            .and_then(|o| schema.table_of(o))
            .map(|t| t.name.clone())
        else {
            continue;
        };
        let card = super::layout_card(
            &mut *provider,
            info,
            &schema,
            &read_only,
            &prohibited,
            &mut buttons,
            &mut uses,
        );
        let mut targets = Vec::new();
        let mut skipped = 0;
        for field in card.fields.iter().filter(|f| f.filemaker_edits) {
            let portal = match &field.creation_row {
                Some(row) if !row.allowed => {
                    skipped += 1;
                    continue;
                }
                Some(row) => Some(row.portal.clone()),
                None => None,
            };
            let Some(object) = render.layout.objects.get(field.object) else {
                continue;
            };
            // A portal row's editor is named `Occurrence::Field`, as the
            // card names the field; another field by its binding.
            let label = match &portal {
                Some(_) => &field.field,
                None => match render.field_bindings.get(&field.object) {
                    Some(label) => label,
                    None => continue,
                },
            };
            let kind = object
                .field_spec()
                .and_then(|spec| {
                    let occurrence = om_layout::occurrence_id(spec.table_key)?;
                    schema.field(occurrence, spec.field_id()?)
                })
                .map_or(ValueType::Text, |def| def.value_type);
            let bounds = object.bounds.as_ref();
            targets.push(Target {
                object: field.object,
                label: label.clone(),
                control: field.control,
                kind,
                portal,
                top: bounds.map_or(0.0, |b| b.top),
                left: bounds.map_or(0.0, |b| b.left),
            });
        }
        targets.sort_by(|a, b| a.top.total_cmp(&b.top).then(a.left.total_cmp(&b.left)));
        let mut order: Vec<&om_layout::LayoutObject> = render
            .layout
            .objects
            .objects
            .iter()
            .filter(|o| o.tab_order > 0)
            .collect();
        order.sort_by_key(|o| o.tab_order);
        let text_box = |id: u64| {
            targets.iter().position(|t| {
                t.object == id
                    && t.portal.is_none()
                    && t.control == "edit box"
                    && t.kind == ValueType::Text
            })
        };
        let tab = order
            .windows(2)
            .find_map(|pair| Some((text_box(pair[0].id)?, text_box(pair[1].id)?)));
        let plan = Plan {
            id: card.id,
            name: card.name,
            table: table.clone(),
            targets,
            skipped,
            tab,
        };
        let better = best
            .get(&table)
            .is_none_or(|other| plan.targets.len() >= other.targets.len());
        if better && !plan.targets.is_empty() {
            best.insert(table, plan);
        }
    }
    provider.close();
    // Layouts without portals first: a portal row's value list often lists
    // another table's records (Asset Details' assignee, from Employees),
    // which those layouts add.
    let mut plans: Vec<Plan> = best.into_values().collect();
    plans.sort_by_key(|p| (p.targets.iter().any(|t| t.portal.is_some()), p.id));
    Ok(plans)
}

fn kind_name(kind: ValueType) -> String {
    format!("{kind:?}").to_lowercase()
}

/// The role of the node a control is drawn as.
fn role(target: &Target) -> Role {
    if target.kind == ValueType::Container {
        return Role::Button;
    }
    match target.control {
        "pop-up menu" => Role::ComboBox,
        "checkbox set" => Role::Group,
        "radio button set" => Role::RadioGroup,
        _ => Role::TextInput,
    }
}

/// What to type into an edit box of `kind` (the starters' English date
/// and time formats).
fn typed(target: &Target) -> String {
    match target.kind {
        ValueType::Number => "1234.5".into(),
        ValueType::Date => "3/14/2026".into(),
        ValueType::Time => "9:30:15".into(),
        ValueType::Timestamp => "3/14/2026 9:30:15".into(),
        _ => {
            let short = target.label.rsplit("::").next().unwrap_or(&target.label);
            format!("Typed {short}")
        }
    }
}

/// The window on `copy`, opened as File > Open does with the account the
/// file names and an empty password.
fn window(copy: &Path) -> Result<App, String> {
    let provider = om_provider::NativeProvider::from_env().map_err(|e| e.message)?;
    let mut app = Harness::builder()
        .with_size(egui::vec2(1600.0, 1400.0))
        .build_eframe(move |_| {
            OpenMakerApp::with_license_store(Box::new(provider), Box::new(NoLicense))
        });
    app.get_by_role_and_label(Role::Button, "File").click();
    app.run_steps(2);
    app.get_by_label("Open...").click();
    app.run_steps(2);
    wait(&mut app, "the Open File dialog", |app| {
        app.query_by_label("Open File").is_some()
    })?;
    replace(&mut app, "Path", &copy.display().to_string());
    app.get_by_label("Open").click();
    app.run_steps(2);
    // A starter's File Options sign in as Admin without asking
    // (local-files.md 3.1); a file that asks gets its dialog's OK.
    let stem = copy.file_stem().unwrap_or_default().to_string_lossy();
    let asking = format!("Open “{stem}”");
    wait(&mut app, "the file open or its sign-in", |app| {
        app.state().is_open() || app.query_by_label(&asking).is_some()
    })?;
    if app.query_by_label(&asking).is_some() {
        app.get_by_label("OK").click();
        app.run_steps(2);
    }
    let mut notes = Vec::new();
    settle(&mut app, &mut notes)?;
    Ok(app)
}

fn wait(app: &mut App, what: &str, done: impl FnMut(&App) -> bool) -> Result<(), String> {
    wait_for(app, what, Duration::from_secs(30), done)
}

fn wait_for(
    app: &mut App,
    what: &str,
    within: Duration,
    mut done: impl FnMut(&App) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + within;
    loop {
        app.step();
        if done(app) {
            app.run_steps(3);
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// No script running and nothing loading. A dialog a script waits on is
/// noted and answered with its OK button.
fn settle(app: &mut App, notes: &mut Vec<String>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut answered = 0;
    loop {
        app.step();
        if app.state().is_settled() && !app.state().script_running() {
            app.run_steps(3);
            return Ok(());
        }
        if app.state().script_paused() || Instant::now() > deadline - Duration::from_secs(25) {
            if let Some(ok) = app
                .query_all(By::new().role(Role::Button).label("OK"))
                .last()
            {
                if answered < 3 {
                    answered += 1;
                    notes.push(format!("dialog answered with OK: {}", dialog_text(app)));
                    ok.click();
                    app.run_steps(3);
                    continue;
                }
            }
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the window did not settle (script running {}, paused {})",
                app.state().script_running(),
                app.state().script_paused()
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The labels of the frontmost dialog, as far as they can be told apart:
/// the last few text labels drawn.
fn dialog_text(app: &App) -> String {
    let labels: Vec<String> = app
        .query_all_by_role(Role::Label)
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let start = labels.len().saturating_sub(3);
    labels[start..].join(" / ")
}

fn replace(app: &mut App, label: &str, text: &str) {
    app.get_by_role_and_label(Role::TextInput, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// The layout bar's layout menu, then `layout` in it.
fn go_to_layout(app: &mut App, layout: &str, notes: &mut Vec<String>) -> Result<(), String> {
    let before = buttons(app);
    app.get_by_label("Layout").click();
    app.run_steps(3);
    let offered = new_buttons(app, &before);
    // A folder of the menu opens its submenu under the pointer.
    for (_, folder) in &offered {
        if app
            .query_all(By::new().role(Role::Button).label(layout))
            .next()
            .is_some()
        {
            break;
        }
        app.hover_at(folder.center());
        app.run_steps(4);
    }
    let item = app
        .query_all(By::new().role(Role::Button).label(layout))
        .last()
        .ok_or_else(|| {
            let names: Vec<&str> = offered.iter().map(|b| b.0.as_str()).collect();
            format!("the layout menu does not offer “{layout}” (it offers {names:?})")
        })?;
    item.click();
    app.run_steps(3);
    settle(app, notes)
}

/// The rects of the nodes of `role` labelled `label`, top to bottom.
fn rects(app: &App, role: Role, label: &str) -> Vec<Rect> {
    let mut rects: Vec<Rect> = app
        .query_all(By::new().role(role).label(label))
        .map(|n| n.rect())
        .collect();
    rects.sort_by(|a, b| {
        a.min
            .y
            .total_cmp(&b.min.y)
            .then(a.min.x.total_cmp(&b.min.x))
    });
    rects
}

/// Where `target`, the `n`th object of its name, is drawn now: in a
/// portal, its first row (the creation row while the portal is empty).
fn place(app: &App, target: &Target, n: usize) -> Option<Rect> {
    let found = rects(app, role(target), &target.label);
    if target.portal.is_some() {
        found.first().copied()
    } else {
        found.get(n).copied()
    }
}

fn readback(
    app: &App,
    target: &Target,
    selected: Option<Rect>,
    phase: &'static str,
    stored_root_record: Option<i64>,
) -> Readback {
    let coordinates = |r: Rect| [r.min.x, r.min.y, r.max.x, r.max.y];
    Readback {
        phase,
        record_number: app
            .query_by_role_and_label(Role::TextInput, "Record Number")
            .and_then(|n| n.value()),
        stored_root_record,
        settled: app.state().is_settled(),
        script_running: app.state().script_running(),
        selected_rect: selected.map(coordinates),
        candidates: rects(app, role(target), &target.label)
            .into_iter()
            .map(|rect| ReadbackControl {
                rect: coordinates(rect),
                value: shown(app, target, rect),
            })
            .collect(),
    }
}

/// Where `target` is drawn, after showing each tab panel in turn if it is
/// on none shown.
fn find(app: &mut App, target: &Target, n: usize) -> Option<Rect> {
    if let Some(rect) = place(app, target, n) {
        return Some(rect);
    }
    let tabs: Vec<Rect> = app.query_all_by_role(Role::Tab).map(|t| t.rect()).collect();
    for tab in tabs {
        click_at(app, tab.center());
        app.run_steps(3);
        if let Some(rect) = place(app, target, n) {
            return Some(rect);
        }
    }
    None
}

/// The node of `role` at `rect`.
fn node_at<'a>(
    app: &'a App,
    role: Role,
    label: &'a str,
    rect: Rect,
) -> Option<egui_kittest::Node<'a>> {
    app.query_all(By::new().role(role).label(label))
        .find(|n| n.rect() == rect)
}

/// The buttons on screen, by label and rect.
fn buttons(app: &App) -> Vec<(String, Rect)> {
    app.query_all_by_role(Role::Button)
        .filter_map(|n| Some((n.accesskit_node().label()?, n.rect())))
        .collect()
}

/// Buttons drawn now that were not in `before`: a menu's items.
fn new_buttons(app: &App, before: &[(String, Rect)]) -> Vec<(String, Rect)> {
    buttons(app)
        .into_iter()
        .filter(|b| !before.contains(b))
        .collect()
}

/// The items of the list, menu or calendar the control at `control` opened:
/// buttons with labels no button had `before`, across from the control.
/// (Buttons that only moved, as the layout does when the bar unsaved
/// changes show comes or goes, keep their labels.)
fn items(app: &App, before: &[(String, Rect)], control: Rect) -> Vec<(String, Rect)> {
    let known: BTreeSet<&str> = before.iter().map(|(label, _)| label.as_str()).collect();
    buttons(app)
        .into_iter()
        .filter(|(label, rect)| {
            !known.contains(label.as_str())
                && rect.max.x > control.min.x
                && rect.min.x < control.max.x + 200.0
        })
        .collect()
}

fn click_at(app: &mut App, pos: Pos2) {
    app.hover_at(pos);
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    app.run_steps(3);
}

/// What `target`'s node at `rect` shows.
fn shown(app: &App, target: &Target, rect: Rect) -> Option<String> {
    let role = role(target);
    let node = node_at(app, role, &target.label, rect).or_else(|| {
        // Saving may move it a little (a portal row, a sliding object).
        app.query_all(By::new().role(role).label(&target.label))
            .min_by(|a, b| {
                let d = |n: &egui_kittest::Node<'_>| n.rect().min.distance(rect.min);
                d(a).total_cmp(&d(b))
            })
    })?;
    if matches!(role, Role::Group | Role::RadioGroup) {
        let set = node.rect();
        let item = if role == Role::Group {
            Role::CheckBox
        } else {
            Role::RadioButton
        };
        let on: Vec<String> = app
            .query_all_by_role(item)
            .filter(|n| set.contains_rect(n.rect()))
            .filter(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
            .filter_map(|n| n.accesskit_node().label())
            .collect();
        return Some(on.join("\n"));
    }
    node.value()
}

/// Choose the first item of the items `appeared`: not a divider (an
/// empty label) or the Edit... item.
fn first_item(appeared: &[(String, Rect)]) -> Option<(String, Rect)> {
    appeared
        .iter()
        .find(|(label, _)| {
            let label = label.trim();
            !label.is_empty() && label != "Edit..." && label != "Edit…" && label != "Other..."
        })
        .cloned()
}

/// Fill `target`, drawn at `rect`; what the user did.
fn enter(
    app: &mut App,
    target: &Target,
    rect: Rect,
    png: &Path,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let role = role(target);
    let before = buttons(app);
    if target.kind == ValueType::Container {
        click_at(app, rect.center());
        settle(app, notes)?;
        app.get_by_role_and_label(Role::Button, "Insert").click();
        app.run_steps(3);
        let item = app
            .query_by_label("Picture...")
            .ok_or("Insert > Picture... is not offered")?;
        if item.accesskit_node().is_disabled() {
            app.key_press(egui::Key::Escape);
            app.run_steps(2);
            return Err("Insert > Picture... is off with the container selected".into());
        }
        item.click();
        app.run_steps(3);
        wait(app, "the Insert Picture dialog", |app| {
            app.query_by_label("Insert Picture").is_some()
        })?;
        replace(app, "File:", &png.display().to_string());
        app.query_all(By::new().role(Role::Button).label("Insert"))
            .last()
            .ok_or("the dialog's Insert button")?
            .click();
        app.run_steps(3);
        settle(app, notes)?;
        return Ok(format!(
            "inserted {}",
            png.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    match role {
        Role::ComboBox => {
            click_at(app, rect.center());
            settle(app, notes)?;
            // A menu whose items are still being read opens once they are
            // (docs/desktop-client.md, "When the items are read"). A click
            // in a portal row while the portal reads its rows again goes;
            // a user clicks again, and so does the measure, noting it.
            let opened = |app: &mut App| {
                wait_for(app, "the menu's items", Duration::from_secs(5), |app| {
                    !items(app, &before, rect).is_empty()
                })
                .is_ok()
            };
            if !opened(app) {
                notes.push("the first click opened no menu; clicked again".into());
                click_at(app, rect.center());
                settle(app, notes)?;
                opened(app);
            }
            let appeared = items(app, &before, rect);
            let Some((label, item)) = first_item(&appeared) else {
                app.key_press(egui::Key::Escape);
                app.run_steps(2);
                return Err("its menu offers nothing to choose".into());
            };
            click_at(app, item.center());
            settle(app, notes)?;
            Ok(format!("chose “{label}”"))
        }
        Role::Group | Role::RadioGroup => {
            let item = if role == Role::Group {
                Role::CheckBox
            } else {
                Role::RadioButton
            };
            let first = app
                .query_all_by_role(item)
                .filter(|n| rect.contains_rect(n.rect()))
                .min_by(|a, b| {
                    let key = |n: &egui_kittest::Node<'_>| (n.rect().min.y, n.rect().min.x);
                    key(a)
                        .0
                        .total_cmp(&key(b).0)
                        .then(key(a).1.total_cmp(&key(b).1))
                })
                .map(|n| (n.accesskit_node().label().unwrap_or_default(), n.rect()));
            let Some((label, item)) = first else {
                return Err("its set offers nothing to choose".into());
            };
            click_at(app, item.center());
            settle(app, notes)?;
            Ok(format!("clicked “{label}”"))
        }
        _ => {
            click_at(app, rect.center());
            settle(app, notes)?;
            let list = matches!(target.control, "drop-down list" | "drop-down calendar");
            if list {
                let mut appeared = items(app, &before, rect);
                // Its arrow or icon, inside the control's right edge
                // (`drop_down::arrow`): a list or calendar with one opens
                // there, not on entering the field.
                if appeared.is_empty() {
                    click_at(app, Pos2::new(rect.right() - 8.0, rect.center().y));
                    settle(app, notes)?;
                    appeared = items(app, &before, rect);
                }
                let choice = if target.control == "drop-down calendar" {
                    // A day's cell is named by its date, which another
                    // calendar's cell or a field may have shown before:
                    // new by place, not by name.
                    new_buttons(app, &before)
                        .into_iter()
                        .filter(|(_, cell)| {
                            cell.max.x > rect.min.x && cell.min.x < rect.max.x + 200.0
                        })
                        .find(|(label, _)| label.split('/').nth(1) == Some("15"))
                } else {
                    first_item(&appeared)
                };
                if let Some((label, item)) = choice {
                    click_at(app, item.center());
                    settle(app, notes)?;
                    return Ok(format!("chose “{label}”"));
                }
                notes.push(if target.control == "drop-down calendar" {
                    "its calendar did not open; typed instead".into()
                } else {
                    "its list offers nothing to choose; typed instead".into()
                });
            }
            let focused = app
                .query_all(By::new().role(Role::TextInput).label(&target.label))
                .any(|n| n.is_focused());
            if !focused {
                return Err("a click does not put the cursor in it".into());
            }
            let value = typed(target);
            app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            app.event(egui::Event::Text(value.clone()));
            app.run_steps(3);
            Ok(format!("typed “{value}”"))
        }
    }
}

/// Enter a record on `plan`'s layout of `copy` and read it back: a row per
/// object, and what stopped the measure, if anything did.
fn measure(copy: &Path, plan: &Plan, png: &Path) -> (Vec<Row>, Option<String>) {
    let mut rows: Vec<Row> = plan
        .targets
        .iter()
        .map(|t| Row {
            file: String::new(),
            layout: plan.name.clone(),
            field: t.label.clone(),
            control: t.control,
            kind: kind_name(t.kind),
            portal: t.portal.clone(),
            entered: None,
            shown: None,
            stored: None,
            reopened: None,
            notes: Vec::new(),
            failure: None,
            readback: Vec::new(),
        })
        .collect();
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        entry(copy, plan, png, &mut rows)
    }));
    let error = match run {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(panic) => Some(format!("panic: {}", super::build::panic_text(panic))),
    };
    for row in &mut rows {
        row.failure = failure(row, error.as_deref());
    }
    (rows, error)
}

/// The toolbar's New Record, until the new record is shown.
fn new_record(app: &mut App, notes: &mut Vec<String>) -> Result<(), String> {
    let new = app
        .query_all(By::new().role(Role::Button).label("New Record"))
        .min_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .ok_or("no New Record button")?;
    new.click();
    app.run_steps(3);
    // The record is created on the host first; then it is shown.
    wait(app, "the new record", |app| {
        app.state().error().is_some()
            || (app.state().record_edit_state() != Some(true)
                && app
                    .query_by_role_and_label(Role::TextInput, "Record Number")
                    .and_then(|n| n.value())
                    .is_some_and(|n| !n.is_empty()))
    })?;
    settle(app, notes)?;
    match app.state().error() {
        Some(error) => Err(format!("New Record: {error}")),
        None => Ok(()),
    }
}

/// Save the record as a user does without leaving it: Ctrl+Enter (the
/// window shows no bar for unsaved changes, as FileMaker Pro shows none,
/// #871).
fn save(app: &mut App, notes: &mut Vec<String>) -> Result<(), String> {
    if app.state().record_edit_state().is_some() {
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        app.run_steps(3);
        wait(app, "the save", |app| {
            app.state().error().is_some() || app.state().record_edit_state().is_none()
        })?;
    }
    settle(app, notes)?;
    match app.state().error() {
        Some(error) => Err(format!("Commit: {error}")),
        None => Ok(()),
    }
}

/// Each target's place among the objects of its name, to find its node
/// by. Nodes are looked up again before each use, should anything move
/// the layout (the bar that unsaved changes showed before #871 did).
fn order(plan: &Plan) -> Vec<usize> {
    let mut nth: BTreeMap<String, usize> = BTreeMap::new();
    plan.targets
        .iter()
        .map(|target| {
            let n = nth.entry(target.label.clone()).or_default();
            *n += 1;
            *n - 1
        })
        .collect()
}

/// Tab between fields (#848, #858): on a new record (or, `find_mode`, in
/// a find request), a click in text edit box `a`, typing, Tab, typing
/// again (and the commit); the second text should be in `b`, the next
/// field of the layout's tab order, as in FileMaker Pro 26.
fn tab_row(copy: &Path, plan: &Plan, (a, b): (usize, usize), find_mode: bool) -> Row {
    let first = &plan.targets[a];
    let next = &plan.targets[b];
    let mut row = Row {
        file: String::new(),
        layout: plan.name.clone(),
        field: format!("{} → Tab → {}", first.label, next.label),
        control: if find_mode {
            "Tab key, Find mode"
        } else {
            "Tab key, Browse"
        },
        kind: "text".into(),
        portal: None,
        entered: Some("typed “Ann”, Tab, typed “Lee”".into()),
        shown: None,
        stored: None,
        reopened: None,
        notes: Vec::new(),
        failure: None,
        readback: Vec::new(),
    };
    let order = order(plan);
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut app = window(copy)?;
        let mut notes = Vec::new();
        go_to_layout(&mut app, &plan.name, &mut notes)?;
        if find_mode {
            app.query_all(By::new().role(Role::Button).label("Find"))
                .min_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
                .ok_or("no Find button")?
                .click();
            app.run_steps(3);
            wait(&mut app, "Find mode", |app| {
                app.query_by_label("Perform Find").is_some()
            })?;
        } else {
            new_record(&mut app, &mut notes)?;
        }
        let rect = find(&mut app, first, order[a]).ok_or("no control on screen")?;
        click_at(&mut app, rect.center());
        settle(&mut app, &mut notes)?;
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        app.event(egui::Event::Text("Ann".into()));
        app.run_steps(3);
        app.key_press(egui::Key::Tab);
        app.run_steps(3);
        settle(&mut app, &mut notes)?;
        app.event(egui::Event::Text("Lee".into()));
        app.run_steps(3);
        if !find_mode {
            save(&mut app, &mut notes)?;
        }
        let value = |app: &mut App, target: &Target, n: usize| {
            find(app, target, n)
                .and_then(|r| shown(app, target, r))
                .unwrap_or_default()
        };
        let shown_a = value(&mut app, first, order[a]);
        let shown_b = value(&mut app, next, order[b]);
        Ok::<_, String>((shown_a, shown_b, notes))
    }));
    match run {
        Ok(Ok((shown_a, shown_b, notes))) => {
            row.shown = Some(format!(
                "{}: “{shown_a}”, {}: “{shown_b}”",
                first.label, next.label
            ));
            row.notes = notes;
            if shown_a != "Ann" || shown_b != "Lee" {
                row.failure = Some(format!(
                    "Tab does not move to the next field of the tab order: {} shows “{shown_b}”",
                    next.label
                ));
            }
        }
        Ok(Err(error)) => row.failure = Some(format!("not reached: {error}")),
        Err(panic) => {
            row.failure = Some(format!("panic: {}", super::build::panic_text(panic)));
        }
    }
    row
}

fn entry(copy: &Path, plan: &Plan, png: &Path, rows: &mut [Row]) -> Result<(), String> {
    let mut app = window(copy)?;
    let mut notes = Vec::new();
    go_to_layout(&mut app, &plan.name, &mut notes)?;
    new_record(&mut app, &mut notes)?;
    let order = order(plan);
    let mut first_typed: Option<(usize, String)> = None;
    for (i, target) in plan.targets.iter().enumerate() {
        let row = &mut rows[i];
        let Some(rect) = find(&mut app, target, order[i]) else {
            row.notes.push("no control on screen".into());
            continue;
        };
        let mut notes = Vec::new();
        match enter(&mut app, target, rect, png, &mut notes) {
            Ok(entered) => {
                if first_typed.is_none()
                    && target.portal.is_none()
                    && target.kind == ValueType::Text
                    && entered.starts_with("typed")
                {
                    first_typed = Some((i, typed(target)));
                }
                row.entered = Some(entered);
            }
            Err(why) => row.notes.push(why),
        }
        if let Some(error) = app.state().error() {
            notes.push(format!("error: {error}"));
        }
        row.notes.extend(notes);
    }
    save(&mut app, &mut notes)?;
    // What the saved record shows (a container's file name) can arrive a
    // few frames after the save.
    let entered: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].entered.is_some())
        .collect();
    let _ = wait_for(
        &mut app,
        "the saved values",
        Duration::from_secs(5),
        |app| {
            entered.iter().all(|&i| {
                place(app, &plan.targets[i], order[i])
                    .and_then(|r| shown(app, &plan.targets[i], r))
                    .is_some_and(|v| !v.trim().is_empty())
            })
        },
    );
    for (i, target) in plan.targets.iter().enumerate() {
        let rect = find(&mut app, target, order[i]);
        if target.portal.is_some() {
            rows[i].readback.push(readback(
                &app,
                target,
                rect,
                "after-save-before-settle",
                None,
            ));
        }
        settle(&mut app, &mut rows[i].notes)?;
        let rect = place(&app, target, order[i]);
        rows[i].shown = rect.and_then(|r| shown(&app, target, r));
        if target.portal.is_some() {
            rows[i]
                .readback
                .push(readback(&app, target, rect, "after-save", None));
        }
    }
    drop(app);

    // The provider: the layout's own fields of the record added last.
    let mut provider = super::build::open(copy)?;
    let page = provider
        .records(plan.id, 0, 10_000)
        .map_err(|e| format!("reading back: {}", e.message))?;
    let total = page.total;
    let stored_root_record = page.records.iter().map(|r| r.id).max();
    if let Some(record) = page.records.iter().max_by_key(|r| r.id) {
        for (i, target) in plan.targets.iter().enumerate() {
            if target.portal.is_some() {
                continue;
            }
            if let Some(column) = page.fields.iter().position(|f| f.name == target.label) {
                rows[i].stored = Some(record.values[column].clone().unwrap_or_default());
            }
        }
    }
    provider.close();

    // A new window at the same record: the first typed field tells it.
    let mut app = window(copy)?;
    go_to_layout(&mut app, &plan.name, &mut notes)?;
    let mut found = false;
    for number in (1..=total.min(50)).rev() {
        replace(&mut app, "Record Number", &number.to_string());
        app.key_press(egui::Key::Enter);
        app.run_steps(3);
        settle(&mut app, &mut notes)?;
        found = match &first_typed {
            Some((i, value)) => place(&app, &plan.targets[*i], order[*i])
                .and_then(|r| shown(&app, &plan.targets[*i], r))
                .is_some_and(|shown| shown == *value),
            None => true,
        };
        if found {
            break;
        }
    }
    if !found {
        return Err("the new window shows no record with the typed values".into());
    }
    for (i, target) in plan.targets.iter().enumerate() {
        let rect = find(&mut app, target, order[i]);
        if target.portal.is_some() {
            rows[i].readback.push(readback(
                &app,
                target,
                rect,
                "reopened-before-settle",
                stored_root_record,
            ));
        }
        // Revealing a tab starts its portal reads. The creation row can
        // already be drawn while the stored rows are still pending (#1189).
        // Wait for data, then locate the control again: its geometry changes
        // when those rows arrive. Never wait for a particular field value.
        settle(&mut app, &mut rows[i].notes)?;
        let rect = place(&app, target, order[i]);
        rows[i].reopened = rect.and_then(|r| shown(&app, target, r));
        if target.portal.is_some() {
            rows[i].readback.push(readback(
                &app,
                target,
                rect,
                "reopened-settled",
                stored_root_record,
            ));
        }
    }
    Ok(())
}

/// Whether `stored` holds what `entered` typed, by `kind`.
fn same_value(kind: &str, typed: &str, stored: &str) -> bool {
    match kind {
        "number" => {
            let number = |s: &str| s.replace([',', '$'], "").trim().parse::<f64>().ok();
            number(typed).is_some() && number(typed) == number(stored)
        }
        _ => typed == stored,
    }
}

/// Why `row` fails, if it does: the first thing that went wrong.
fn failure(row: &Row, stopped: Option<&str>) -> Option<String> {
    let notes = &row.notes;
    if notes.iter().any(|n| n == "no control on screen") {
        return Some("no control on screen".into());
    }
    let Some(entered) = &row.entered else {
        if let Some(why) = notes.first() {
            return Some(why.clone());
        }
        return stopped.map(|s| format!("not reached: {s}"));
    };
    if let Some(error) = notes.iter().find(|n| n.starts_with("error:")) {
        return Some(error.clone());
    }
    if let Some(why) = stopped.filter(|_| row.shown.is_none()) {
        return Some(format!("not read back: {why}"));
    }
    let shown = row.shown.as_deref().unwrap_or("");
    if shown.trim().is_empty() {
        return Some("empty after the commit".into());
    }
    if row.portal.is_none() && row.kind != "container" {
        match row.stored.as_deref() {
            None => {}
            Some("") => return Some("nothing stored".into()),
            Some(stored) => {
                if let Some(value) = entered
                    .strip_prefix("typed “")
                    .and_then(|v| v.strip_suffix('”'))
                {
                    if !same_value(&row.kind, value, stored) && row.kind != "date" {
                        return Some(format!("stored “{stored}” for “{value}”"));
                    }
                }
            }
        }
    }
    match &row.reopened {
        None => Some(stopped.map_or_else(
            || "not shown in a new window".into(),
            |s| format!("not read back: {s}"),
        )),
        Some(again) if again != shown => {
            Some(format!("shows “{again}” in a new window, “{shown}” before"))
        }
        Some(_) => None,
    }
}

fn summary(layouts: &[LayoutRow], rows: &[Row], tab: &[Row]) -> String {
    let mut out = String::from("# Entry: a record typed through the window\n\n");
    let failing = rows.iter().filter(|r| r.failure.is_some()).count();
    out.push_str(&format!(
        "{} field objects on {} layouts; {} kept after reopening, {} failing.\n\n",
        rows.len(),
        layouts.len(),
        rows.len() - failing,
        failing
    ));
    let mut reasons: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        if let Some(failure) = &row.failure {
            let reason = failure
                .split(['“', ':'])
                .next()
                .unwrap_or(failure)
                .trim()
                .to_owned();
            reasons
                .entry(reason)
                .or_default()
                .insert(format!("{} {}::{}", row.file, row.layout, row.field));
        }
    }
    out.push_str("| Failure | Objects | Where |\n|---|---:|---|\n");
    for (reason, places) in super::ranked(reasons) {
        out.push_str(&format!(
            "| {reason} | {} | {} |\n",
            places.len(),
            super::list(&places, 6)
        ));
    }
    out.push_str("\n## Tab between fields (#848, #858)\n\n");
    if tab.is_empty() {
        out.push_str("No layout has two text edit boxes in a row in its tab order.\n");
    }
    for row in tab {
        out.push_str(&format!(
            "- {} {}, {}: {}. {}\n",
            row.file,
            row.layout,
            row.control,
            row.field,
            row.failure
                .as_deref()
                .unwrap_or("The second text is in the next field.")
        ));
    }
    out.push_str("\n## Layouts\n\n| File | Layout | Table | Objects | Portal fields without a creation row | Stopped |\n|---|---|---|---:|---:|---|\n");
    for l in layouts {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            l.file,
            l.layout,
            l.table,
            l.objects,
            l.skipped,
            l.error.as_deref().unwrap_or("")
        ));
    }
    out.push_str("\n## Objects\n\n| File | Layout | Field | Control | Type | Entered | Shown | Stored | New window | Failure |\n|---|---|---|---|---|---|---|---|---|---|\n");
    let cell = |s: Option<&str>| s.unwrap_or("").replace('\n', "¶").replace('|', "\\|");
    for r in rows {
        let field = match &r.portal {
            Some(portal) => format!("{} (portal {portal})", r.field),
            None => r.field.clone(),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.file,
            r.layout,
            field,
            r.control,
            r.kind,
            cell(r.entered.as_deref()),
            cell(r.shown.as_deref()),
            cell(r.stored.as_deref()),
            cell(r.reopened.as_deref()),
            cell(r.failure.as_deref()),
        ));
    }
    out
}
