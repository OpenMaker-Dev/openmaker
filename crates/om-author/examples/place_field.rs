//! Placing a new field on a layout from start to finish, on a synthetic
//! file: an OpenMaker Server in this process hosts a copy of the Tasks test
//! file whose Tasks layout is a simple Form layout (built in memory, no
//! customer data). The authoring service creates a text field, reads the
//! layout, plans the field's placement with a label and places it; the
//! server restarts, a new connection reads the layout back with the new
//! objects' binding, frame and stacking, and the desktop client stores a
//! value in the field through its record writes and reads it back. Prints
//! each JSON response.
//!
//! `cargo run -p om-author --example place_field`

#[path = "../tests/common/mod.rs"]
mod common;

use common::{fixture, TempFile, ACCOUNT, PASSWORD};
use om_author::api::{
    CreateFieldParams, FieldType, FrameParams, InspectParams, LabelParams, LayoutParams, Part,
    PlaceFieldParams, PlacementParams,
};
use om_author::{Command, Service};
use om_data::DataProvider;
use serde_json::Value;

fn show(response: Value) -> Value {
    println!("{}", serde_json::to_string_pretty(&response).unwrap());
    assert_eq!(response["ok"], true, "the command failed");
    response["result"].clone()
}

fn frame(left: f64, top: f64, width: f64, height: f64) -> FrameParams {
    FrameParams {
        left,
        top,
        width,
        height,
    }
}

fn get_layout(service: &mut Service) -> Value {
    show(service.handle(&Command::GetLayout(LayoutParams {
        layout: None,
        layout_name: Some("Tasks".into()),
    })))
}

fn main() {
    let file = TempFile::from_tree("example-place", &common::form_tree());
    let host = common::host(&file, "OpenMaker Server example");
    let mut service = Service::new(common::target(&host, ACCOUNT, PASSWORD));

    // A text field, as create_field makes it: in the table, on no layout.
    let inspected = service.handle(&Command::Inspect(InspectParams {
        include: Some(vec![Part::Tables]),
    }));
    let tasks = inspected["result"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Tasks")
        .unwrap()
        .clone();
    let created = show(service.handle(&Command::CreateField(CreateFieldParams {
        table: Some(tasks["ref"].as_str().unwrap().into()),
        table_name: None,
        name: "Follow up".into(),
        field_type: FieldType::Text,
        comment: None,
        expected_revision: tasks["fields_revision"].as_str().unwrap().into(),
    })));

    // The layout as stored, then the placement planned and made against
    // its revision: the field beside the Name field's row, its label left
    // of it.
    let layout = get_layout(&mut service);
    let revision = layout["revision"].as_str().unwrap().to_owned();
    let plan = PlacementParams {
        layout: Some(layout["layout"]["ref"].as_str().unwrap().into()),
        layout_name: None,
        field: Some(created["field"]["ref"].as_str().unwrap().into()),
        field_name: None,
        frame: frame(110.0, 80.0, 200.0, 20.0),
        label: Some(LabelParams {
            text: "Follow up".into(),
            frame: frame(20.0, 80.0, 80.0, 20.0),
        }),
        expected_revision: Some(revision.clone()),
    };
    show(service.handle(&Command::PlanFieldPlacement(plan.clone())));
    let placed = show(service.handle(&Command::PlaceField(PlaceFieldParams {
        layout: plan.layout,
        layout_name: None,
        field: plan.field,
        field_name: None,
        frame: plan.frame,
        label: plan.label,
        expected_revision: revision,
    })));

    // The saved file, opened by a new server and read over a new
    // connection.
    host.stop();
    let restarted = common::host(&file, "OpenMaker Server example");
    let mut service = Service::new(common::target(&restarted, ACCOUNT, PASSWORD));
    let read = get_layout(&mut service);
    assert_eq!(read["revision"], placed["revision"]);

    // The desktop client lists the object among the layout's fields and
    // stores a value in it like any other field.
    let mut desktop = common::provider(&restarted);
    let names = desktop.calc_schema().unwrap().unwrap();
    let entry = names.layouts().iter().find(|l| l.id == 1).unwrap();
    let object = placed["objects"][0]["key"].as_u64().unwrap();
    let listed = entry.fields.iter().find(|f| f.object == object).unwrap();
    println!(
        "Layout Tasks lists object {} showing field {} at {:?}",
        listed.object, listed.field, listed.bounds
    );
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    desktop
        .update_record(
            layout,
            1,
            [("Follow up".to_owned(), Some("call back".to_owned()))]
                .into_iter()
                .collect(),
        )
        .unwrap();
    desktop.close();
    let mut reader = common::provider(&restarted);
    let page = reader.record_by_id(layout, 1).unwrap();
    let column = page
        .fields
        .iter()
        .position(|f| f.name == "Follow up")
        .unwrap();
    println!(
        "Tasks record 1, Follow up: {:?}",
        page.records[0].values[column]
    );
    reader.close();
    restarted.stop();
}
