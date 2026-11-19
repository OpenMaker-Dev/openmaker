//! The Contacts template's scripts (capture 35) behind a provider that
//! stores what the Script Workspace saves.

use std::sync::{Arc, Mutex};

use om_data::calc::{Locale, ValueType};
use om_data::host::{CalcSchema, FieldDef};
use om_data::{
    Calculation, Connection, DataError, DataProvider, HostedFile, LayoutInfo, RecordPage, Script,
    ScriptCatalog, ScriptKind, ScriptStep,
};

#[derive(Default)]
pub struct State {
    pub scripts: ScriptCatalog,
    pub events: Vec<String>,
    /// Like the native provider today: no script saves.
    pub read_only: bool,
}

pub struct Provider(pub Arc<Mutex<State>>);

impl DataProvider for Provider {
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![])
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 3,
            name: "Contact Details".into(),
        }])
    }
    fn records(&mut self, _: u32, _: u64, _: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: vec![],
            records: vec![],
            total: 0,
        })
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(self.0.lock().unwrap().scripts.clone())
    }
    fn save_script(&mut self, script: Script) -> Result<Script, DataError> {
        let mut state = self.0.lock().unwrap();
        if state.read_only {
            return Err(DataError::new(
                "Saving scripts to the host is not available yet.",
            ));
        }
        let entry = state
            .scripts
            .scripts
            .iter_mut()
            .find(|s| s.id == script.id)
            .ok_or_else(|| DataError::new("Missing script"))?;
        *entry = script.clone();
        state.events.push(format!("save {}", script.name));
        Ok(script)
    }
    fn save_script_catalog(&mut self, catalog: ScriptCatalog) -> Result<ScriptCatalog, DataError> {
        let mut state = self.0.lock().unwrap();
        if state.read_only {
            return Err(DataError::new(
                "Saving scripts to the host is not available yet.",
            ));
        }
        state.scripts.scripts = catalog.scripts;
        state.events.push("catalog".into());
        Ok(state.scripts.clone())
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        // The fields of capture 42, a second layout and the scripts for
        // the pickers.
        let mut schema = schema();
        schema.add_layout(4, "Contact List", Some(0xd00002));
        let state = self.0.lock().unwrap();
        schema.set_scripts(
            state
                .scripts
                .scripts
                .iter()
                .map(|s| om_data::meta::ScriptEntry {
                    id: s.id,
                    name: s.name.clone(),
                    kind: s.kind,
                })
                .collect(),
        );
        Ok(Some(Arc::new(schema)))
    }
    fn close(&mut self) {}
}

/// The Contacts table's fields, in capture 42's order.
pub fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Contacts", Locale::en_us());
    schema.add_table(0x8002, "Contacts");
    let text = |id, name: &str| FieldDef::data(id, name, ValueType::Text);
    let initial = om_data::formula::Formula {
        context: None,
        formatted: None,
        polish: om_data::formula::Expr::Empty,
    };
    for field in [
        text(1, "First Name"),
        text(2, "Last Name"),
        FieldDef::data(3, "Photo", ValueType::Container),
        text(4, "Title"),
        text(5, "Company"),
        text(6, "Job Title"),
        text(7, "Website"),
        FieldDef::calculation(8, "Initial", ValueType::Text, initial, true),
        text(9, "PrimaryKey"),
        text(10, "CreatedBy"),
        text(11, "ModifiedBy"),
        FieldDef::data(12, "CreationTimestamp", ValueType::Timestamp),
        FieldDef::data(13, "ModificationTimestamp", ValueType::Timestamp),
    ] {
        schema.add_field(0x8002, field);
    }
    schema.add_occurrence(0xd00002, "Contacts", 0x8002);
    schema.add_layout(3, "Contact Details", Some(0xd00002));
    schema
}

/// Two clicks in one frame: kittest deals queued events one frame each,
/// further apart than egui's double-click interval.
pub fn double_click_at(app: &mut super::App, pos: egui::Pos2) {
    let input = app.input_mut();
    input.events.push(egui::Event::PointerMoved(pos));
    for pressed in [true, false, true, false] {
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    app.run_steps(2);
}

/// Where the editor draws the text after `prefix` on the step line
/// labelled `line`: the gutter, the text inset and monospaced characters
/// of the step editor (`editor.rs`).
pub fn step_text_position(app: &super::App, line: &str, prefix: &str) -> egui::Pos2 {
    use egui_kittest::kittest::Queryable;
    let row = app.get_by_label(line).rect();
    let width = app
        .ctx
        .fonts_mut(|f| f.glyph_width(&egui::FontId::monospace(12.5), 'M'));
    egui::pos2(
        row.left() + 36.0 + 15.0 + (prefix.chars().count() as f32 + 2.0) * width,
        row.center().y,
    )
}

pub fn calc(source: &str) -> Calculation {
    let names = om_data::formula::NoNames;
    let compiled =
        om_data::formula::compile(source, &om_data::formula::Context::new(&names)).unwrap();
    Calculation::new(
        Some(om_data::formula::formatted::encode(&compiled.tokens)),
        om_data::formula::polish::encode(&compiled.expr),
    )
}

/// The Contacts template's catalog (35): folder Contacts with Go to Entry
/// Field and Sort by Name.
pub fn contacts() -> ScriptCatalog {
    let member = |id: u64, name: &str, kind: ScriptKind| Script {
        id,
        name: name.into(),
        kind,
        ..Script::default()
    };
    let mut go = member(2, "Go to Entry Field", ScriptKind::Script);
    let mut object = ScriptStep::new(145);
    object.set_calculation(0, Some(&calc("Get ( ScriptParameter )")));
    go.insert_steps(0, [object, ScriptStep::new(50)]).unwrap();
    let mut sort = member(3, "Sort by Name", ScriptKind::Script);
    sort.insert_steps(0, [ScriptStep::new(39)]).unwrap();
    let mut folder = member(1, "Contacts", ScriptKind::Folder);
    folder.metadata = vec![om_data::runtime::OwnedNode::leaf([0xf0], [1])];
    let mut end = member(4, "--", ScriptKind::FolderEnd);
    end.metadata = vec![om_data::runtime::OwnedNode::leaf([0xf0], [2])];
    ScriptCatalog {
        scripts: vec![folder, go, sort, end],
        writable: true,
        unavailable_reason: None,
    }
}

/// The script catalog index `[17, 1]` FileMaker keeps for `scripts`, in
/// their order (scripts.md, "The catalog index"): the last issued id
/// `last` (above the highest member when scripts were deleted), the name
/// list, the display order, the count, the member cache `07`, its digest
/// `08`, `20`, the UUID and a stamp. Put it in `root` beside the scripts.
pub fn add_script_index(root: &mut om_hbam::OwnedNode, scripts: &[Script], last: u64) {
    use om_hbam::{serial, OwnedNode};
    let key_string = |id: u64| {
        let segment = serial::encode(id);
        let mut out = vec![segment.len() as u8];
        out.extend(segment);
        out
    };
    let mut names: Vec<OwnedNode> = scripts
        .iter()
        .map(|s| {
            let mut key = om_data::field_catalog::name_key(&s.name);
            key.extend([0, 0, 0]);
            OwnedNode::leaf(key, key_string(s.id))
        })
        .collect();
    names.sort_by(|a, b| a.segment.cmp(&b.segment));
    let order = scripts
        .iter()
        .enumerate()
        .map(|(i, s)| OwnedNode::leaf((i as u32 + 1).to_be_bytes().to_vec(), key_string(s.id)))
        .collect();
    let cache = scripts
        .iter()
        .map(|s| {
            OwnedNode::list(
                serial::encode(s.id),
                vec![
                    om_scripts::text_node(&[0x10], &s.name),
                    OwnedNode::leaf([0xf0], [0u8; 8]),
                ],
            )
        })
        .collect();
    let mut index = OwnedNode::list(
        vec![1],
        vec![
            OwnedNode::leaf([0x00], key_string(last)),
            OwnedNode::list([0x01], names),
            OwnedNode::list([0x03], order),
            OwnedNode::leaf([0x04], (scripts.len() as u32).to_be_bytes()),
            OwnedNode::list([0x07], cache),
            OwnedNode::leaf([0x20], [4]),
            OwnedNode::leaf([0xd8], [0x42; 16]),
            OwnedNode::leaf([0xfc], [1, 3]),
        ],
    );
    let digest = om_data::script_catalog::catalog_digest(&index).expect("the cache's digest");
    index.set_child(OwnedNode::leaf([0x08], digest));
    root.put(&[&[17], &[1]], index);
}

/// In an open file's window: open the Script Workspace, make a new script
/// with New Script, name it `name`, paste `steps` (step text) into it, see
/// its first line begin with `first_line`, and save it with ⌘S. Leaves the
/// workspace open on the saved script.
pub fn create_script(app: &mut super::App, name: &str, steps: &str, first_line: &str) {
    let first_line = format!("Line 1: {first_line}");
    use egui::Modifiers;
    use egui_kittest::kittest::{NodeT, Queryable};
    super::open_menu(app, "Scripts");
    super::click(app, "Script Workspace...");
    // The toolbar's New Script is enabled once the catalog is read.
    super::wait_until(app, "the workspace's scripts", |app| {
        app.query_all_by_label("New Script")
            .next()
            .is_some_and(|n| !n.accesskit_node().is_disabled())
    });
    app.query_all_by_label("New Script").next().unwrap().click();
    super::wait_until(app, "the new script's name field", |app| {
        super::find_text_field(app, "Script Name").is_some()
    });
    super::text_field(app, "Script Name").focus();
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    super::text_field(app, "Script Name").type_text(name);
    app.run_steps(2);
    app.key_press_modifiers(Modifiers::NONE, egui::Key::Enter);
    let tab = format!("Tab {name}");
    super::wait_until(app, "the named script's tab", |app| {
        app.query_by_label(&tab).is_some() && super::find_text_field(app, "Script Name").is_none()
    });
    // Let the rename's save finish before pasting.
    for _ in 0..20 {
        app.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.event(egui::Event::Paste(steps.into()));
    app.run_steps(3);
    super::wait_until(app, "the pasted steps", |app| {
        app.query_by_label_contains(&first_line).is_some()
    });
    super::wait_for_label(app, &format!("Tab {name} *"));
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::S);
    app.run_steps(2);
    super::wait_until(app, "the script's save", |app| {
        app.query_by_label(&tab).is_some()
            || app.query_by_label_contains("could not be saved").is_some()
    });
    assert!(
        app.query_by_label_contains("could not be saved").is_none(),
        "the save was refused"
    );
}

/// The script catalog of the `.fmp12` file at `path`, read from disk.
pub fn scripts_on_disk(path: &std::path::Path) -> ScriptCatalog {
    let file = om_hbam::HbamFile::open(path).expect("open the saved file");
    let tree = file.key_tree().expect("read its tree");
    ScriptCatalog::decode(&tree).expect("decode its scripts")
}
