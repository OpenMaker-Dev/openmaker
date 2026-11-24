//! The design analysis commands against OpenMaker Server in this process,
//! on a synthetic file whose references are known: the field Tasks::Count
//! is used by a script step, a layout field object and a calculation
//! field; the script Caller performs Callee and is performed by a button;
//! Callee sets a field that was deleted, exports and imports other fields
//! and reads one by name with `GetField`; the script Unused is used by
//! nothing.

mod common;

use common::*;
use om_author::{Command, Service};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_layout::encode;
use om_scripts::{flag, ScriptStep, StoredKey};
use om_transfer::spec::{
    ExportField, ExportOptions, FieldSpec, ImportAction, ImportOptions, Key, MapEntry, Mapping,
};
use om_transfer::{ExportCharset, ImportCharset};
use serde_json::{json, Value};

const CALLER: u64 = 1;
const CALLEE: u64 = 2;
const UNUSED: u64 = 3;
/// A field id the Tasks table does not have.
const DELETED: u64 = 99;
/// Layout Tasks' objects: Count's edit box and Caller's button.
const COUNT_BOX: u64 = 1;
const BUTTON: u64 = 2;

/// Export Records of `field` of Tasks: its export order `85`
/// (script-steps.md 11.4).
fn export_records(field: u64) -> ScriptStep {
    let none = FieldSpec {
        table: Key::none(),
        field: Key::none(),
        repetition: 1,
    };
    let order = ExportOptions {
        fields: vec![ExportField {
            field: FieldSpec::new(fixture::OCCURRENCE, field),
            sum_by: none,
        }],
        group_by: Vec::new(),
        formatting: false,
        charset: ExportCharset::Windows,
        use_name_alias: None,
        field_names: None,
    };
    let mut step = ScriptStep::new(36);
    step.set_data_leaf(0x85, Some(om_scripts::bytes_node(&[0x85], &order.encode())));
    step
}

/// Import Records into `field` of Tasks: its import order `84`
/// (script-steps.md 11.3).
fn import_records(field: u64) -> ScriptStep {
    let order = ImportOptions {
        table: Key::from_id(fixture::OCCURRENCE),
        action: ImportAction::Add,
        flags: 0,
        charset: ImportCharset::Windows,
        map: vec![MapEntry {
            field: Key::from_id(field),
            mapping: Mapping::Import,
        }],
    };
    let mut step = ScriptStep::new(35);
    step.set_data_leaf(0x84, Some(om_scripts::bytes_node(&[0x84], &order.encode())));
    step
}

fn analysis_tree() -> OwnedNode {
    let scripts = vec![
        fixture::script(
            CALLER,
            "Caller",
            vec![
                fixture::set_field(fixture::COUNT, "1"),
                fixture::perform_script(CALLEE),
            ],
        ),
        fixture::script(
            CALLEE,
            "Callee",
            vec![
                fixture::set_field(DELETED, "\"x\""),
                export_records(fixture::NAME),
                import_records(fixture::SETTING),
                fixture::set_variable("$due", "GetField ( \"Tasks::Due\" )"),
                fixture::exit_with("\"done\""),
            ],
        ),
        fixture::script(UNUSED, "Unused", vec![fixture::exit_with("\"nothing\"")]),
    ];
    // Tasks::Doubled is the calculation `Count * 2` (fixtures/tasks.rs).
    let mut root = fixture::tree(&scripts, true);
    let bounds = |left: f64| om_layout::Rect {
        top: 50.0,
        left,
        bottom: 70.0,
        right: left + 100.0,
        other: Vec::new(),
    };
    // Field specs store the occurrence's key as its serial value.
    let occurrence = serial::decode(&uint_segment(fixture::OCCURRENCE)).unwrap();
    let mut reserved = [0u8; 14];
    reserved[..6].copy_from_slice(&StoredKey::from_serial_id(CALLER).unwrap().0);
    let objects = [
        encode::field_object(COUNT_BOX, &bounds(20.0), occurrence, fixture::COUNT),
        encode::button_object(BUTTON, &bounds(200.0), 1, 0, flag::SCRIPT_KEY, &reserved),
    ];
    let store = encode::append_to_store(&[], &[COUNT_BOX, BUTTON], &objects);
    root.put(
        &[&[4], &[5], &uint_segment(fixture::LAYOUT), &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
    root
}

fn run(service: &mut Service, command: &str, params: Value) -> Value {
    let request = json!({
        "version": "openmaker.authoring/v1",
        "command": command,
        "params": params,
    });
    ok(service.handle_json(&request.to_string()))
}

/// The references of a result's list: each holder's reference and part,
/// and the step's place for a script.
/// A use: its holder's reference, the part, and a step's index and line.
type Held = (String, String, Option<(u64, u64)>);

fn holders(uses: &Value) -> Vec<Held> {
    uses.as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let step = &u["place"]["step"];
            (
                text(&u["holder"]["ref"]).to_owned(),
                text(&u["place"]["part"]).to_owned(),
                step.is_object().then(|| {
                    (
                        step["index"].as_u64().unwrap(),
                        step["line"].as_u64().unwrap(),
                    )
                }),
            )
        })
        .collect()
}

#[test]
fn each_analysis_returns_exactly_the_files_references_with_its_coverage() {
    let file = TempFile::from_tree("author-analysis", &analysis_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let r = |kind: &str, id: &str| format!("{kind}:{id}@{FILE}");

    // The commands are listed with their parameters.
    let commands = ok(service.handle(&Command::Commands));
    for (id, cli, tool) in [
        ("find_uses", "find-uses", "openmaker_find_uses"),
        (
            "broken_references",
            "broken-references",
            "openmaker_broken_references",
        ),
        (
            "unused_objects",
            "unused-objects",
            "openmaker_unused_objects",
        ),
        ("script_calls", "script-calls", "openmaker_script_calls"),
    ] {
        let spec = commands["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("{id} is not listed"));
        assert_eq!(spec["cli"], cli);
        assert_eq!(spec["mcp_tool"], tool);
        assert_eq!(spec["effect"], "reads");
        assert_eq!(spec["params"]["type"], "object", "{spec:#}");
    }

    // Count: a script step, a layout object and a calculation, each
    // through the Tasks occurrence.
    let count = run(
        &mut service,
        "find_uses",
        json!({"field_name": "Count", "table_name": "Tasks"}),
    );
    // Each result is printed for --nocapture runs.
    println!("find_uses {count:#}");
    assert_eq!(count["object"]["ref"], r("field", "32769.2"), "{count:#}");
    assert_eq!(count["object"]["found"], true);
    let mut found = holders(&count["uses"]);
    found.sort();
    assert_eq!(
        found,
        [
            (r("field", "32769.5"), "calculation".into(), None),
            (r("layout_object", "1.1"), "field".into(), None),
            (r("script", "1"), "Field".into(), Some((1, 1))),
        ],
        "{count:#}"
    );
    for u in count["uses"].as_array().unwrap() {
        assert_eq!(u["resolution"], "resolved");
        assert_eq!(u["via"]["ref"], r("occurrence", "13631489"));
    }
    assert_eq!(count["total"], 3);
    // Every place was read, privilege sets included, but Callee's
    // `GetField` names a field when it runs: the result says so. The read
    // was clean, and nothing changed while it ran.
    assert_eq!(count["nothing_known_unread"], false);
    assert_eq!(count["coverage"]["read_cleanly"], true, "{count:#}");
    assert_eq!(count["coverage"]["consistent"], true, "{count:#}");
    assert_eq!(count["coverage"]["changed_during_read"], json!([]));
    assert_eq!(count["coverage"]["unread"]["total"], 0, "{count:#}");
    assert_eq!(count["coverage"]["by_name"]["total"], 1, "{count:#}");
    let by_name = &count["coverage"]["by_name"]["places"][0];
    assert_eq!(by_name["how"], "get_field");
    assert_eq!(by_name["holder"]["ref"], r("script", "2"));
    assert_eq!(by_name["place"]["step"]["index"], 4);
    assert_eq!(count["coverage"]["unavailable"], json!([]));
    assert_eq!(count["coverage"]["stopped"], Value::Null);
    assert_eq!(count["coverage"]["read"]["scripts"], 3);

    // One reference whose target was not found: Callee's Set Field of
    // the deleted field.
    let broken = run(&mut service, "broken_references", json!({}));
    println!("broken_references {broken:#}");
    assert_eq!(broken["total"], 1, "{broken:#}");
    assert_eq!(
        holders(&broken["references"]),
        [(r("script", "2"), "Field".into(), Some((1, 1)))]
    );
    let missing = &broken["references"][0];
    assert_eq!(missing["resolution"], "target_not_found");
    assert_eq!(missing["missing"]["ref"], r("field", "32769.99"));
    assert_eq!(missing["missing"]["found"], false);
    // A deleted object's reference finds what still names it.
    let deleted = run(
        &mut service,
        "find_uses",
        json!({"object": r("field", "32769.99")}),
    );
    assert_eq!(deleted["object"]["found"], false);
    assert_eq!(holders(&deleted["uses"]), holders(&broken["references"]));

    // Import and export orders name their fields (script-steps.md 11.3,
    // 11.4).
    for (field, part, step) in [
        (fixture::NAME, "Export order", 2),
        (fixture::SETTING, "Import order", 3),
    ] {
        let found = run(
            &mut service,
            "find_uses",
            json!({"object": r("field", &format!("32769.{field}"))}),
        );
        assert_eq!(
            holders(&found["uses"]),
            [(r("script", "2"), part.into(), Some((step, step)))],
            "{found:#}"
        );
    }

    // Caller is performed by the layout's button.
    let caller = run(&mut service, "find_uses", json!({"script_name": "Caller"}));
    assert_eq!(
        holders(&caller["uses"]),
        [(r("layout_object", "1.2"), "button: Script".into(), None)],
        "{caller:#}"
    );

    // One script no reference was found for, with nothing known unread
    // that could perform one. Not so for fields: `GetField` names one by
    // name.
    let unused = run(&mut service, "unused_objects", json!({"kinds": ["script"]}));
    println!("unused_objects {unused:#}");
    assert_eq!(unused["nothing_known_unread"]["script"], true, "{unused:#}");
    let objects: Vec<(&str, &str)> = unused["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (text(&o["ref"]), text(&o["finding"])))
        .collect();
    assert_eq!(objects, [(r("script", "3").as_str(), "no_reference_found")]);
    let fields = run(&mut service, "unused_objects", json!({"kinds": ["field"]}));
    assert_eq!(fields["nothing_known_unread"]["field"], false);
    let unused_fields: Vec<&str> = fields["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| text(&o["ref"]))
        .collect();
    assert!(unused_fields.contains(&r("field", "32769.3").as_str()));
    assert!(!unused_fields.contains(&r("field", "32769.2").as_str()));

    // Caller performs Callee at its second step; nothing performs Caller.
    let chain = run(
        &mut service,
        "script_calls",
        json!({"script_name": "Caller"}),
    );
    println!("script_calls {chain:#}");
    assert_eq!(
        chain["calls"]["scripts"],
        json!([{"ref": r("script", "2"), "name": "Callee", "depth": 1}])
    );
    let perform = &chain["calls"]["performs"][0];
    assert_eq!(perform["from"]["ref"], r("script", "1"));
    assert_eq!(perform["to"]["ref"], r("script", "2"));
    assert_eq!(perform["place"]["step"]["index"], 2);
    assert_eq!(perform["place"]["step"]["line"], 2);
    assert_eq!(chain["calls"]["total_performs"], 1);
    assert_eq!(chain["calls"]["cycles"], json!([]));
    assert_eq!(chain["callers"]["scripts"], json!([]));
    assert_eq!(chain["nothing_known_unread"], true);
    let callers = run(
        &mut service,
        "script_calls",
        json!({"script": r("script", "2"), "direction": "callers"}),
    );
    assert_eq!(
        callers["callers"]["scripts"],
        json!([{"ref": r("script", "1"), "name": "Caller", "depth": 1}])
    );
    assert!(callers.get("calls").is_none());

    // Names are matched as FileMaker matches them; a missing one is
    // not_found, and two names at once are refused.
    let error = error(
        service.handle_json(
            &json!({"version": "openmaker.authoring/v1", "command": "find_uses",
                "params": {"script_name": "Nobody"}})
            .to_string(),
        ),
    );
    assert_eq!(error["kind"], "not_found");
    let error = common::error(
        service.handle_json(
            &json!({"version": "openmaker.authoring/v1", "command": "find_uses",
                "params": {"script_name": "Caller", "layout_name": "Tasks"}})
            .to_string(),
        ),
    );
    assert_eq!(error["kind"], "invalid_request");
    host.stop();
}
