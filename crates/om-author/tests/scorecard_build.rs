//! The alpha scorecard's build measure through `om-author` (criterion 7;
//! `docs/development.md`, "Alpha scorecard"): what of an ordinary
//! solution's build `om-author` does on OpenMaker Server, step by step, on
//! a copy of each file named, hosted on loopback by an in-process
//! `om-server` and signed in to as the file's Admin with an empty password,
//! as the starter solutions have it (the server hosts such files only when
//! told to, as FileMaker Server does). Each step is attempted with the
//! command that would do it, by name and JSON parameters as the CLI and
//! MCP server send them; a step with no command reports what `capabilities`
//! says of it. The steps: create a table; add a field of each type (text,
//! number, date, time, timestamp, container, calculation, summary, global);
//! add a relationship; create a layout; place a field and a button on it
//! (the field on the new layout, or on the file's first layout when no
//! layout could be created); move and resize the field, delete its label,
//! Layout Setup, the layout's width, the field's control style and the
//! body's options;
//! create a script (and edit an existing one instead); make the button run
//! it.
//!
//! `om-author` has no local-file target (`om_author::Target` names a host,
//! a port and a hosted file), so a local file is not measured here; and
//! nothing here checks that FileMaker Pro opens the result.
mod common;

use std::path::PathBuf;

use common::fixture::TempFile;
use common::{host_insecure, target, ACCOUNT};
use om_author::{Command, Service};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Serialize)]
struct Step {
    step: String,
    /// `yes`, `no`, or `partial`.
    done: &'static str,
    /// The command and its result or error, or what `capabilities` says.
    detail: String,
}

#[derive(Serialize, Default)]
struct Card {
    file: String,
    table: String,
    layout: String,
    script: String,
    steps: Vec<Step>,
    error: Option<String>,
}

/// Run command `name` with `params`: its result, or its error's kind and
/// message.
fn call(service: &mut Service, name: &str, params: Value) -> Result<Value, String> {
    let command = Command::from_parts(name, params).map_err(|e| format!("{name}: {e}"))?;
    let response = service.handle(&command);
    if response["ok"] == true {
        Ok(response["result"].clone())
    } else {
        Err(format!(
            "{name}: {} ({})",
            response["error"]["message"].as_str().unwrap_or("?"),
            response["error"]["kind"].as_str().unwrap_or("?")
        ))
    }
}

/// Why a write's result was not read back over a new connection, if it
/// was not (the service reports the write done either way).
fn unverified(result: &Value) -> Option<String> {
    let verified = &result["verified"];
    (verified["fresh_connection"] != true).then(|| {
        verified["reason"]
            .as_str()
            .map_or_else(|| format!("not read back: {verified}"), str::to_owned)
    })
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// What `capabilities` says of operation `key`.
fn reason(capabilities: &Value, key: &str) -> String {
    let op = &capabilities["operations"][key];
    match op["reason"].as_str() {
        Some(reason) => format!("capabilities.{key}: {reason}"),
        None => format!("capabilities.{key}: {op}"),
    }
}

fn record(card: &mut Card, step: &str, done: &'static str, detail: impl Into<String>) {
    card.steps.push(Step {
        step: step.into(),
        done,
        detail: detail.into(),
    });
}

/// The table's current fields revision.
fn fields_revision(service: &mut Service, table: &str) -> Result<String, String> {
    let inspected = call(service, "inspect", json!({ "include": ["tables"] }))?;
    inspected["tables"]
        .as_array()
        .and_then(|t| t.iter().find(|t| t["ref"] == table))
        .map(|t| text(&t["fields_revision"]))
        .ok_or_else(|| "the table is no longer listed".into())
}

fn measure(service: &mut Service, card: &mut Card) -> Result<(), String> {
    let capabilities = call(service, "capabilities", json!({}))?;
    let inspected = call(
        service,
        "inspect",
        json!({ "include": ["tables", "occurrences", "layouts", "scripts"] }),
    )?;
    let empty = Vec::new();
    let list = |key: &str| inspected[key].as_array().unwrap_or(&empty).clone();
    // The first layout with a table occurrence, and that occurrence's table.
    let layout = list("layouts")
        .into_iter()
        .find(|l| l["kind"] == "layout" && l["occurrence"].is_string())
        .ok_or("no layout with a table occurrence")?;
    card.layout = text(&layout["name"]);
    let table_ref = list("occurrences")
        .into_iter()
        .find(|o| o["ref"] == layout["occurrence"])
        .map(|o| o["table"].clone())
        .filter(Value::is_string)
        .ok_or("the layout's occurrence has no table of this file")?;
    let table = list("tables")
        .into_iter()
        .find(|t| t["ref"] == table_ref)
        .ok_or("the layout's table is not listed")?;
    card.table = text(&table["name"]);
    let table_ref = text(&table_ref);

    record(
        card,
        "1. Create a table",
        "no",
        reason(&capabilities, "schema"),
    );

    let mut created = Vec::new();
    for kind in [
        "text",
        "number",
        "date",
        "time",
        "timestamp",
        "container",
        "calculation",
        "summary",
        "global",
    ] {
        let name = format!("Build {kind}");
        let revision = fields_revision(service, &table_ref)?;
        let result = call(
            service,
            "create_field",
            json!({
                "table": table_ref,
                "name": name,
                "type": kind,
                "expected_revision": revision,
            }),
        );
        let step = format!("2. Add a {kind} field");
        match result {
            Ok(result) => {
                created.push(name.clone());
                match unverified(&result) {
                    None => record(card, &step, "yes", format!("create_field “{name}”")),
                    Some(why) => record(
                        card,
                        &step,
                        "partial",
                        format!("create_field “{name}”, unverified: {why}"),
                    ),
                }
            }
            Err(e) => record(
                card,
                &step,
                "no",
                format!("{e}; {}", reason(&capabilities, "schema")),
            ),
        }
    }

    record(
        card,
        "3. Add a relationship",
        "no",
        reason(&capabilities, "schema"),
    );
    // A Form layout on the first layout's occurrence, in its theme.
    let made = (|| -> Result<(Value, String), String> {
        let written = call(
            service,
            "create_layout",
            json!({
                "name": "Scorecard Layout",
                "occurrence": layout["occurrence"],
                "layout": layout["ref"],
                "expected_revision": inspected["layouts_revision"],
            }),
        )?;
        if let Some(why) = unverified(&written) {
            return Err(format!("create_layout reported done, unverified: {why}"));
        }
        Ok((
            written["layout"].clone(),
            format!(
                "create_layout “Scorecard Layout” on {}: {}",
                text(&layout["occurrence"]),
                text(&written["layout"]["ref"])
            ),
        ))
    })();
    let (layout, on) = match made {
        Ok((made, detail)) => {
            record(card, "4. Create a layout", "yes", detail);
            card.layout = text(&made["name"]);
            (made, "the new one")
        }
        Err(e) => {
            record(card, "4. Create a layout", "no", e);
            (layout, "an existing one")
        }
    };
    let step5 = format!("5. Place a field on the layout ({on})");

    // Place a field created above on that layout, then read the layout
    // back for it.
    let field = created.first().cloned();
    let placed = (|| -> Result<String, String> {
        let field = field.ok_or("no field was created to place")?;
        let before = call(service, "get_layout", json!({ "layout": layout["ref"] }))?;
        let objects = before["objects"].as_array().map_or(0, Vec::len);
        // In the body part, as placement requires: the label, then the
        // field below it.
        let body = before["parts"]
            .as_array()
            .and_then(|parts| parts.iter().find(|p| p["kind"] == "body"))
            .ok_or("the layout has no body part")?;
        let top = body["top"].as_f64().unwrap_or_default();
        let written = call(
            service,
            "place_field",
            json!({
                "layout": layout["ref"],
                "field_name": field,
                "frame": { "left": 20.0, "top": top + 22.0, "width": 160.0, "height": 20.0 },
                "label": {
                    "text": field,
                    "frame": { "left": 20.0, "top": top + 4.0, "width": 160.0, "height": 16.0 },
                },
                "expected_revision": before["revision"],
            }),
        )?;
        if let Some(why) = unverified(&written) {
            return Err(format!("place_field reported done, unverified: {why}"));
        }
        let after = call(service, "get_layout", json!({ "layout": layout["ref"] }))?;
        let count = after["objects"].as_array().map_or(0, Vec::len);
        Ok(format!(
            "place_field “{field}” with a label on “{}”: {objects} objects before, {count} after",
            card.layout
        ))
    })();
    match placed {
        Ok(detail) => record(card, &step5, "yes", detail),
        Err(e) => record(card, &step5, "no", e),
    }
    layout_steps(service, card, &layout);
    record(
        card,
        "6. Place a button on the layout",
        "no",
        reason(&capabilities, "layouts"),
    );

    record(
        card,
        "7. Create a script",
        "no",
        reason(&capabilities, "create_script"),
    );
    // Instead, replace an existing script's steps and run it on the host.
    let mut edited = Err("the file has no script to edit".to_owned());
    for script in list("scripts") {
        let Ok(got) = call(service, "get_script", json!({ "script": script["ref"] })) else {
            continue;
        };
        card.script = text(&script["name"]);
        edited = (|| -> Result<String, String> {
            // A step a host runs (Beep, for one, is error 3 there, as on
            // FileMaker Server).
            let source = "Exit Script [ Text Result: \"built\" ]\n";
            let plan = call(
                service,
                "plan",
                json!({
                    "script": script["ref"],
                    "source": source,
                    "expected_revision": got["revision"],
                }),
            )?;
            let applied = call(
                service,
                "apply",
                json!({
                    "script": script["ref"],
                    "source": source,
                    "expected_revision": got["revision"],
                    "expected_after_revision": plan["after_revision"],
                }),
            )?;
            if let Some(why) = unverified(&applied) {
                return Err(format!("apply reported done, unverified: {why}"));
            }
            let back = call(service, "get_script", json!({ "script": script["ref"] }))?;
            let ran = call(service, "run_script", json!({ "script": script["ref"] }))
                .map(|r| {
                    format!(
                        "run_script: last error {}, result “{}”",
                        r["last_error"],
                        text(&r["result"])
                    )
                })
                .unwrap_or_else(|e| e);
            Ok(format!(
                "apply “Exit Script” to “{}”: reads back {:?}; {ran}",
                card.script,
                text(&back["source"]).trim()
            ))
        })();
        break;
    }
    match edited {
        Ok(detail) => record(
            card,
            "7b. Replace an existing script's steps",
            "yes",
            detail,
        ),
        Err(e) => record(card, "7b. Replace an existing script's steps", "no", e),
    }
    record(
        card,
        "8. Make the button run the script",
        "no",
        format!("no command; {}", reason(&capabilities, "layouts")),
    );
    Ok(())
}

/// The layout as `get_layout` reads it.
fn get_layout(service: &mut Service, layout: &Value) -> Result<Value, String> {
    call(service, "get_layout", json!({ "layout": layout["ref"] }))
}

/// Run a write and fail unless it was read back over a new connection.
fn write(service: &mut Service, name: &str, params: Value) -> Result<Value, String> {
    let written = call(service, name, params)?;
    match unverified(&written) {
        None => Ok(written),
        Some(why) => Err(format!("{name} reported done, unverified: {why}")),
    }
}

/// Layout mode's edits of the layout the field was placed on, each with
/// its command, then read back with `get_layout` where it shows the
/// result: move and resize the placed field, delete its label, Layout
/// Setup, the width, the field's control and a part's options.
fn layout_steps(service: &mut Service, card: &mut Card, layout: &Value) {
    let objects = |got: &Value, kind: &str| -> Vec<Value> {
        got["objects"]
            .as_array()
            .map(|o| o.iter().filter(|o| o["kind"] == kind).cloned().collect())
            .unwrap_or_default()
    };
    let moved = (|| -> Result<String, String> {
        let got = get_layout(service, layout)?;
        let field = objects(&got, "field")
            .pop()
            .ok_or("the layout has no field object")?;
        let f = &field["frame"];
        let frame = json!({
            "left": f["left"].as_f64().unwrap_or_default() + 4.0,
            "top": f["top"].as_f64().unwrap_or_default() + 4.0,
            "width": f["width"].as_f64().unwrap_or_default() + 8.0,
            "height": f["height"].as_f64().unwrap_or_default(),
        });
        write(
            service,
            "set_object_frames",
            json!({
                "layout": layout["ref"],
                "objects": [{ "object": field["ref"], "frame": frame }],
                "expected_revision": got["revision"],
            }),
        )?;
        let back = get_layout(service, layout)?;
        let now = back["objects"]
            .as_array()
            .and_then(|o| o.iter().find(|o| o["ref"] == field["ref"]))
            .map(|o| o["frame"].clone())
            .ok_or("the object is gone")?;
        if now == frame {
            Ok(format!(
                "set_object_frames {}: reads back at {now}",
                text(&field["ref"])
            ))
        } else {
            Err(format!(
                "set_object_frames: reads back at {now}, not {frame}"
            ))
        }
    })();
    match moved {
        Ok(detail) => record(card, "5b. Move and resize a placed object", "yes", detail),
        Err(e) => record(card, "5b. Move and resize a placed object", "no", e),
    }
    let deleted = (|| -> Result<String, String> {
        let got = get_layout(service, layout)?;
        let label = objects(&got, "text")
            .pop()
            .ok_or("the layout has no text object")?;
        write(
            service,
            "delete_objects",
            json!({
                "layout": layout["ref"],
                "objects": [label["ref"]],
                "expected_revision": got["revision"],
            }),
        )?;
        let back = get_layout(service, layout)?;
        let gone = back["objects"]
            .as_array()
            .is_some_and(|o| o.iter().all(|o| o["ref"] != label["ref"]));
        gone.then(|| {
            format!(
                "delete_objects {}: gone on reading back",
                text(&label["ref"])
            )
        })
        .ok_or_else(|| "delete_objects: the object is still there".into())
    })();
    match deleted {
        Ok(detail) => record(card, "5c. Delete a placed object", "yes", detail),
        Err(e) => record(card, "5c. Delete a placed object", "no", e),
    }
    let set_up = (|| -> Result<String, String> {
        let got = get_layout(service, layout)?;
        write(
            service,
            "layout_setup",
            json!({
                "layout": layout["ref"],
                "expected_revision": got["revision"],
                "name": "Scorecard Setup",
                "views": ["form", "list"],
                "options": { "enable_quick_find": false },
            }),
        )?;
        let back = get_layout(service, layout)?;
        let name = text(&back["layout"]["name"]);
        (name == "Scorecard Setup")
            .then(|| "layout_setup: named “Scorecard Setup”, Table view off, Quick Find off; the name reads back (get_layout shows no views or options)".to_owned())
            .ok_or_else(|| format!("layout_setup: reads back named “{name}”"))
    })();
    match set_up {
        Ok(detail) => record(
            card,
            "5d. Layout Setup: name, views, General options",
            "yes",
            detail,
        ),
        Err(e) => record(
            card,
            "5d. Layout Setup: name, views, General options",
            "no",
            e,
        ),
    }
    let widened = (|| -> Result<String, String> {
        let got = get_layout(service, layout)?;
        let width = got["width"].as_f64().ok_or("no width")? + 40.0;
        write(
            service,
            "set_layout_width",
            json!({
                "layout": layout["ref"],
                "expected_revision": got["revision"],
                "width": width,
            }),
        )?;
        let now = get_layout(service, layout)?["width"].as_f64();
        (now == Some(width))
            .then(|| format!("set_layout_width {width}: reads back"))
            .ok_or_else(|| format!("set_layout_width: reads back {now:?}, not {width}"))
    })();
    match widened {
        Ok(detail) => record(card, "5e. A layout's width", "yes", detail),
        Err(e) => record(card, "5e. A layout's width", "no", e),
    }
    let controlled = (|| -> Result<String, String> {
        let got = get_layout(service, layout)?;
        let field = objects(&got, "field")
            .pop()
            .ok_or("the layout has no field object")?;
        let lists = call(service, "get_value_lists", json!({}))?;
        let list = lists["value_lists"]
            .as_array()
            .and_then(|l| l.first())
            .map(|l| l["ref"].clone())
            .ok_or("the file has no value list")?;
        write(
            service,
            "set_field_control",
            json!({
                "object": field["ref"],
                "expected_revision": got["revision"],
                "control": "pop_up_menu",
                "value_list": list,
            }),
        )?;
        let back = get_layout(service, layout)?;
        let control = back["objects"]
            .as_array()
            .and_then(|o| o.iter().find(|o| o["ref"] == field["ref"]))
            .map(|o| text(&o["control"]))
            .unwrap_or_default();
        (control == "pop_up_menu")
            .then(|| {
                format!(
                    "set_field_control {}: a pop-up menu of {}",
                    text(&field["ref"]),
                    text(&list)
                )
            })
            .ok_or_else(|| format!("set_field_control: reads back as “{control}”"))
    })();
    match controlled {
        Ok(detail) => record(card, "5f. A field's control style", "yes", detail),
        Err(e) => record(card, "5f. A field's control style", "no", e),
    }
    let parted = (|| -> Result<String, String> {
        let got = get_layout(service, layout)?;
        let body = got["parts"]
            .as_array()
            .and_then(|p| p.iter().find(|p| p["kind"] == "body"))
            .ok_or("the layout has no body part")?;
        write(
            service,
            "set_part_options",
            json!({
                "layout": layout["ref"],
                "expected_revision": got["revision"],
                "part": body["id"],
                "options": { "restart_page_numbers": true },
            }),
        )?;
        Ok(format!(
            "set_part_options body {}: Restart page numbers on, read back by the command (get_layout shows no part options)",
            body["id"]
        ))
    })();
    match parted {
        Ok(detail) => record(card, "5g. A part's options", "yes", detail),
        Err(e) => record(card, "5g. A part's options", "no", e),
    }
}

#[test]
#[ignore = "requires OM_SCORECARD_FILES (paths of .fmp12 files, ':'-separated) and OM_SCORECARD_OUT (an output folder)"]
fn build_scorecard() {
    let files = std::env::var("OM_SCORECARD_FILES").expect("OM_SCORECARD_FILES");
    let out = PathBuf::from(std::env::var("OM_SCORECARD_OUT").expect("OM_SCORECARD_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let mut cards = Vec::new();
    for (n, path) in std::env::split_paths(&files).enumerate() {
        let name = path
            .components()
            .rev()
            .take(2)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("/");
        let mut card = Card {
            file: name,
            ..Card::default()
        };
        let copy = TempFile(out.join(format!("build-{n}.fmp12")));
        std::fs::copy(&path, &copy.0).expect("copy the file");
        let hosted = host_insecure(&copy, "OpenMaker Server scorecard");
        let mut service = Service::new(target(&hosted, ACCOUNT, ""));
        if let Err(e) = measure(&mut service, &mut card) {
            card.error = Some(e);
        }
        drop(service);
        drop(hosted);
        cards.push(card);
    }
    std::fs::write(
        out.join("build.json"),
        serde_json::to_string_pretty(&cards).unwrap(),
    )
    .unwrap();
    let mut md = String::new();
    for card in &cards {
        md.push_str(&format!(
            "\n### {}: table {}, layout {}, script {}\n\n| Step | Done | Command and result |\n|---|---|---|\n",
            card.file, card.table, card.layout, card.script
        ));
        for step in &card.steps {
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                step.step,
                step.done,
                step.detail.replace('|', "\\|").replace('\n', " ")
            ));
        }
        if let Some(error) = &card.error {
            md.push_str(&format!("\nStopped: {error}\n"));
        }
    }
    std::fs::write(out.join("build.md"), md).unwrap();
}
