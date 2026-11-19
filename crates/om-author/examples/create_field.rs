//! Adding a field from start to finish, on a synthetic file: an OpenMaker
//! Server in this process hosts a copy of the Tasks test file (built in
//! memory, no customer data). The authoring service reads the Tasks
//! table's fields revision, plans a date field and creates it; the server
//! restarts, a new connection lists the field, and the desktop client
//! stores a value in it as a script's Set Field does (a user's typing is
//! limited to text fields there) and reads it back. Prints each JSON
//! response.
//!
//! `cargo run -p om-author --example create_field`

#[path = "../tests/common/mod.rs"]
mod common;

use common::{fixture, TempFile, ACCOUNT, PASSWORD};
use om_author::api::{CreateFieldParams, FieldParams, FieldType, InspectParams, Part};
use om_author::{Command, Service};
use om_data::DataProvider;
use serde_json::Value;

fn show(response: Value) -> Value {
    println!("{}", serde_json::to_string_pretty(&response).unwrap());
    assert_eq!(response["ok"], true, "the command failed");
    response["result"].clone()
}

/// The Tasks table as inspect lists it.
fn tasks(service: &mut Service) -> Value {
    let inspected = service.handle(&Command::Inspect(InspectParams {
        include: Some(vec![Part::Tables]),
    }));
    inspected["result"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Tasks")
        .unwrap()
        .clone()
}

fn main() {
    let file = TempFile::from_tree("example-field", &common::indexed_tree());
    let host = common::host(&file, "OpenMaker Server example");
    let mut service = Service::new(common::target(&host, ACCOUNT, PASSWORD));

    let table = tasks(&mut service);
    let table_ref = table["ref"].as_str().unwrap().to_owned();
    let revision = table["fields_revision"].as_str().unwrap().to_owned();
    println!("Tasks is {table_ref}, its fields at {revision}");

    show(service.handle(&Command::PlanField(FieldParams {
        table: Some(table_ref.clone()),
        table_name: None,
        name: "Follow up".into(),
        field_type: FieldType::Date,
        comment: Some("When to check in".into()),
        expected_revision: Some(revision.clone()),
    })));
    let created = show(service.handle(&Command::CreateField(CreateFieldParams {
        table: Some(table_ref),
        table_name: None,
        name: "Follow up".into(),
        field_type: FieldType::Date,
        comment: Some("When to check in".into()),
        expected_revision: revision,
    })));

    // The saved file, opened by a new server.
    host.stop();
    let restarted = common::host(&file, "OpenMaker Server example");
    let mut service = Service::new(common::target(&restarted, ACCOUNT, PASSWORD));
    println!(
        "{}",
        serde_json::to_string_pretty(&tasks(&mut service)["fields"]).unwrap()
    );

    // The desktop client's record writes use the new field.
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    let field = created["field"]["id"].as_u64().unwrap();
    let date = Some("11/30/2026".to_owned());
    let mut desktop = common::provider(&restarted);
    desktop
        .write_record(&om_data::ScriptWrite {
            layout,
            record: Some(1),
            cells: [(field, date.clone())].into_iter().collect(),
            fields: [("Follow up".to_owned(), date)].into_iter().collect(),
            ..Default::default()
        })
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
