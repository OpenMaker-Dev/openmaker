//! Adding a field end to end against an OpenMaker Server on loopback
//! hosting a synthetic file: plan it, create it, read it back after the
//! server restarts and store a value in it through the desktop client's
//! record writes, add another through Manage Database's save (the same
//! host operation) and list it here; and the creations the host and
//! service refuse, which leave the file as it was.

mod common;

use common::*;
use om_author::api::{CreateFieldParams, FieldParams, FieldType, InspectParams, Part};
use om_author::{Command, Service};
use om_data::{field_catalog, DataProvider, FieldKind, SchemaChange};
use om_hbam::slot::uint_segment;
use om_protocol::authoring::{self, catalog_revision, status, Reply, Request};
use serde_json::Value;

const TASKS_REF: &str = "table:32769@Tasks.fmp12";

fn tasks(service: &mut Service) -> Value {
    let inspected = ok(service.handle(&Command::Inspect(InspectParams {
        include: Some(vec![Part::Tables]),
    })));
    inspected["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["ref"] == TASKS_REF)
        .expect("Tasks listed")
        .clone()
}

fn create(name: &str, field_type: FieldType, expected: &str) -> Command {
    Command::CreateField(CreateFieldParams {
        table: Some(TASKS_REF.into()),
        table_name: None,
        name: name.into(),
        field_type,
        comment: None,
        expected_revision: expected.into(),
    })
}

/// The first record's value of `name` on a page.
fn value(page: &om_data::RecordPage, name: &str) -> Option<String> {
    let index = page.fields.iter().position(|f| f.name == name)?;
    page.records.first()?.values[index].clone()
}

#[test]
fn inspected_fields_and_revision_stay_together_across_a_concurrent_create() {
    let file = TempFile::from_tree("author-field-snapshot", &indexed_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut inspector = provider(&host);
    let snapshot = inspector.database_schema_snapshot().unwrap();
    let mut writer = Service::new(target(&host, ACCOUNT, PASSWORD));
    let before = tasks(&mut writer);
    let base = text(&before["fields_revision"]).to_owned();
    let table = snapshot
        .schema
        .tables
        .iter()
        .find(|t| t.id == 0x8001)
        .unwrap();
    assert!(!table.fields.iter().any(|f| f.name == "Concurrent"));
    assert_eq!(
        catalog_revision(&snapshot.field_catalogs[&0x8001]),
        revision(&base)
    );

    // Another author commits after the inspector's schema download but
    // before it uses the token. A later host read now sees a new catalog.
    let added = ok(writer.handle(&create("Concurrent", FieldType::Text, &base)));
    let latest = text(&added["revision"]).to_owned();
    let current = inspector.field_catalog(0x8001).unwrap().unwrap();
    assert_eq!(catalog_revision(&current), revision(&latest));
    assert_ne!(latest, base);

    // The observation stays at its original fields AND revision. Using
    // that token detects the unseen addition instead of accepting it.
    assert_eq!(
        catalog_revision(&snapshot.field_catalogs[&0x8001]),
        revision(&base)
    );
    assert!(!table.fields.iter().any(|f| f.name == "Concurrent"));
    let refused = error(writer.handle(&create("InspectorAdded", FieldType::Text, &base)));
    assert_eq!(refused["kind"], "conflict");
    assert_eq!(refused["details"]["current_revision"], latest);
    let fresh = tasks(&mut writer);
    assert_eq!(fresh["fields_revision"], latest);
    assert!(fresh["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["name"] == "Concurrent"));
    assert!(!fresh["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["name"] == "InspectorAdded"));
}

#[test]
fn a_field_is_planned_created_and_used_after_a_restart() {
    let file = TempFile::from_tree("author-field", &indexed_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));

    let capabilities = ok(service.handle(&Command::Capabilities));
    let create_field = &capabilities["operations"]["create_field"];
    assert_eq!(create_field["available"], true, "{capabilities:#}");
    assert!(create_field["types"]
        .as_array()
        .unwrap()
        .contains(&"timestamp".into()));
    assert_eq!(
        capabilities["operations"]["apply_script_steps"]["available"],
        true
    );

    let before = tasks(&mut service);
    let base = text(&before["fields_revision"]).to_owned();
    let plan = ok(service.handle(&Command::PlanField(FieldParams {
        table: None,
        table_name: Some("tasks".into()),
        name: "Priority".into(),
        field_type: FieldType::Text,
        comment: Some("Set by agents".into()),
        expected_revision: Some(base.clone()),
    })));
    assert_eq!(plan["already_applied"], false);
    assert_eq!(plan["base_revision"], base.as_str());
    // Fields 1 to 7 were issued; the definition is FileMaker's default.
    assert_eq!(plan["field"]["ref"], "field:32769.8@Tasks.fmp12");
    assert_eq!(
        plan["field"]["definition"],
        "0101000000000015000000000000000000000001000000000001"
    );
    assert_eq!(plan["changes"].as_array().unwrap().len(), 6, "{plan:#}");
    assert_eq!(plan["create"]["available"], true);
    let on_disk = std::fs::read(&file.0).unwrap();

    let created = ok(service.handle(&Command::CreateField(CreateFieldParams {
        table: Some(TASKS_REF.into()),
        table_name: None,
        name: "Priority".into(),
        field_type: FieldType::Text,
        comment: Some("Set by agents".into()),
        expected_revision: base.clone(),
    })));
    assert_eq!(created["status"], "created", "{created:#}");
    assert_eq!(created["field"]["ref"], "field:32769.8@Tasks.fmp12");
    assert_eq!(created["previous_revision"], base.as_str());
    let after = text(&created["revision"]).to_owned();
    assert_ne!(after, base);
    assert_eq!(created["verified"]["fresh_connection"], true);
    assert_eq!(created["verified"]["revision"], after.as_str());
    assert_eq!(created["verified"]["other_fields_kept"], 7);
    assert_ne!(
        std::fs::read(&file.0).unwrap(),
        on_disk,
        "saved before the reply"
    );

    // The same request again (an answer lost in transit): already there,
    // nothing written.
    let saved = std::fs::read(&file.0).unwrap();
    let mut again = Command::CreateField(CreateFieldParams {
        table: Some(TASKS_REF.into()),
        table_name: None,
        name: "Priority".into(),
        field_type: FieldType::Text,
        comment: Some("Set by agents".into()),
        expected_revision: base.clone(),
    });
    let retried = ok(service.handle(&again));
    assert_eq!(retried["status"], "already_applied");
    assert_eq!(retried["field"]["ref"], "field:32769.8@Tasks.fmp12");
    // As the host answers it when a client sends it anyway.
    let (mut session, file_id) = session(&host, "Retry desk", ACCOUNT, PASSWORD);
    let request = Request::CreateField(authoring::CreateField {
        table: 0x8001,
        expected: revision(&base),
        data_type: 1,
        name: "Priority".into(),
        comment: "Set by agents".into(),
    });
    let reply = session
        .perform(&authoring::build_request(9, file_id, &request).unwrap())
        .unwrap();
    assert_eq!(
        authoring::parse_reply(&request, &reply).unwrap(),
        Reply::Created(authoring::Created {
            changed: false,
            field: 8,
            revision: revision(&after),
        })
    );
    session.disconnect().unwrap();
    assert_eq!(std::fs::read(&file.0).unwrap(), saved);
    // A different field from the old revision is a conflict.
    if let Command::CreateField(params) = &mut again {
        params.name = "Other".into();
    }
    let conflict = error(service.handle(&again));
    assert_eq!(conflict["kind"], "conflict");
    assert_eq!(conflict["details"]["current_revision"], after.as_str());

    // The saved file, opened by a new server.
    host.stop();
    let reopened = common::host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let listed = tasks(&mut service);
    assert_eq!(listed["fields_revision"], after.as_str());
    let field = listed["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Priority")
        .expect("Priority listed");
    assert_eq!(field["ref"], "field:32769.8@Tasks.fmp12");
    assert_eq!(field["kind"], "normal");
    assert_eq!(field["type"], "text");

    // As stored: the member with UUIDs and who made it (no count, it is
    // new), the index's lists, count and stamp, every other field and the
    // index's other keys as they were.
    let mut desktop = provider(&reopened);
    let catalog = desktop.field_catalog(0x8001).unwrap().unwrap();
    assert_eq!(catalog_revision(&catalog), revision(&after));
    let member = catalog.at(&[&[5], &[8]]).unwrap();
    assert_eq!(
        field_catalog::member_comment(member).as_deref(),
        Some("Set by agents")
    );
    for uuid in [0xd8u8, 0xd9] {
        assert_eq!(
            member.child(&[uuid]).unwrap().value.as_ref().unwrap().len(),
            16
        );
    }
    assert!(member.child(authoring::MOD_COUNT).is_none());
    assert_eq!(
        member
            .child(authoring::MOD_ACCOUNT)
            .and_then(om_scripts::node_text)
            .as_deref(),
        Some(ACCOUNT)
    );
    let index = catalog.child(&[1]).unwrap();
    assert_eq!(
        index.child(&[0]).unwrap().value.as_deref(),
        Some(&[1, 8][..])
    );
    assert_eq!(
        index.child(&[4]).unwrap().value.as_deref(),
        Some(&[0, 0, 0, 8][..])
    );
    assert_eq!(
        index
            .at(&[&[3], &8u32.to_be_bytes()])
            .unwrap()
            .value
            .as_deref(),
        Some(&[1, 8][..])
    );
    let mut typed = vec![1, 4];
    typed.extend(field_catalog::name_key("Priority"));
    typed.extend([0, 0]);
    assert!(index.at(&[&[6], &typed]).is_some());
    assert_eq!(
        index.child(authoring::MOD_COUNT).unwrap().value.as_deref(),
        Some(&[1, 1][..])
    );
    let original = indexed_tree();
    let original = original.at(&[&uint_segment(0x8001), &[3]]).unwrap();
    for key in [&[UNKNOWN_INDEX_KEY][..], &[0xd8]] {
        assert_eq!(index.child(key), original.at(&[&[1], key]));
    }
    for old in field_catalog::members(original) {
        assert_eq!(
            catalog.at(&[&[5], &old.segment]).unwrap().children,
            old.children
        );
    }

    // The desktop client's record writes use it like any other field.
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    desktop
        .update_record(
            layout,
            1,
            [("Priority".to_owned(), Some("high".to_owned()))]
                .into_iter()
                .collect(),
        )
        .unwrap();
    desktop.close();
    let mut fresh = provider(&reopened);
    let page = fresh.record_by_id(layout, 1).unwrap();
    assert_eq!(value(&page, "Priority").as_deref(), Some("high"));
    assert_eq!(value(&page, "Name").as_deref(), Some("Write tests"));
    fresh.close();

    // Manage Database saves with FileMaker Pro's own schema transaction:
    // opening it takes the schema lock (another desk is told the schema is
    // in use), changes are staged, refused ones change nothing, and OK
    // saves the rest, which read back with the ids the draft gave them.
    let mut manage = provider(&reopened);
    let schema = manage.begin_schema_edit().unwrap();
    assert!(schema.capabilities.fields && schema.capabilities.tables);
    let mut other = provider(&reopened);
    let in_use = other.begin_schema_edit().unwrap_err();
    assert_eq!(
        in_use.code,
        Some(i32::from(status::SCHEMA_IN_USE)),
        "{in_use}"
    );
    other.close();
    let staged = manage
        .apply_schema_change(SchemaChange::CreateField {
            table_id: 0x8001,
            name: "Due Date".into(),
            data_type: FieldKind::Date,
            comment: String::new(),
        })
        .unwrap();
    let due = staged
        .table(0x8001)
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "Due Date");
    assert_eq!(due.map(|f| (f.id, f.data_type)), Some((9, FieldKind::Date)));
    let on_disk = std::fs::read(&file.0).unwrap();
    for (refused, code) in [
        (
            SchemaChange::CreateField {
                table_id: 0x8001,
                name: "due date".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            },
            status::NAME_EXISTS,
        ),
        (
            SchemaChange::CreateField {
                table_id: 0x8001,
                name: "Cost + Tax".into(),
                data_type: FieldKind::Number,
                comment: String::new(),
            },
            status::NAME_UNUSABLE,
        ),
    ] {
        let error = manage.apply_schema_change(refused).unwrap_err();
        assert_eq!(error.code, Some(i32::from(code)), "{error}");
    }
    // Nothing is sent before OK.
    assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
    let saved = manage.commit_schema_changes().unwrap();
    assert!(saved
        .table(0x8001)
        .unwrap()
        .fields
        .iter()
        .any(|f| f.id == 9 && f.name == "Due Date"));
    // The schema lock went with the save.
    let mut other = provider(&reopened);
    other.begin_schema_edit().unwrap();
    other.cancel_schema_changes().unwrap();
    other.close();
    manage.close();
    let listed = tasks(&mut service);
    let due = listed["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Due Date")
        .expect("Due Date listed");
    assert_eq!(due["ref"], "field:32769.9@Tasks.fmp12");
    assert_eq!(due["type"], "date");
    reopened.stop();
}

#[test]
fn refused_fields_leave_the_file_as_it_was() {
    let file = TempFile::from_tree("author-field-refusals", &indexed_tree());
    let host = host(&file, "OpenMaker Server test");
    let on_disk = std::fs::read(&file.0).unwrap();
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let base = text(&tasks(&mut service)["fields_revision"]).to_owned();

    let stale = format!("sha256:{}", "00".repeat(32));
    let conflict = error(service.handle(&create("Due date", FieldType::Date, &stale)));
    assert_eq!(conflict["kind"], "conflict");
    assert_eq!(conflict["details"]["current_revision"], base.as_str());

    // FileMaker's names are compared without case: "name" is Name's.
    let taken = error(service.handle(&create("name", FieldType::Text, &base)));
    assert_eq!(taken["kind"], "duplicate_name", "{taken:#}");
    assert_eq!(taken["details"]["field"], "field:32769.1@Tasks.fmp12");
    let unusable = error(service.handle(&create("Cost + Tax", FieldType::Number, &base)));
    assert_eq!(unusable["kind"], "invalid_name");

    let wrong = error(service.handle(&Command::PlanField(FieldParams {
        table: Some("table:32769@Other.fmp12".into()),
        table_name: None,
        name: "Due date".into(),
        field_type: FieldType::Date,
        comment: None,
        expected_revision: None,
    })));
    assert_eq!(wrong["kind"], "wrong_file");

    // Manage Database open on another desk, or a record of the table being
    // edited there, holds the schema.
    let (mut other, other_file) = session(&host, "Other desk", ACCOUNT, PASSWORD);
    for path in [
        vec![vec![8u8]],
        vec![uint_segment(0x8001), vec![5], vec![1]],
    ] {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        assert!(matches!(
            other.lock(&path, 77, true).unwrap(),
            om_protocol::write::LockReply::Granted { .. }
        ));
        let locked = error(service.handle(&create("Due date", FieldType::Date, &base)));
        assert_eq!(locked["kind"], "locked", "{locked:#}");
        assert!(text(&locked["message"]).contains("Other desk"));
        other.kill_all_locks(77).unwrap();
    }

    // The host refuses what a client that skips the service's checks
    // sends: a taken or unusable name, a type that is not scalar.
    for (name, data_type, refusal) in [
        ("NAME", 1, status::NAME_EXISTS),
        ("1st", 1, status::NAME_UNUSABLE),
        ("Photo", 6, status::INVALID),
    ] {
        let request = Request::CreateField(authoring::CreateField {
            table: 0x8001,
            expected: revision(&base),
            data_type,
            name: name.into(),
            comment: String::new(),
        });
        let reply = other
            .perform(&authoring::build_request(9, other_file, &request).unwrap())
            .unwrap();
        assert!(
            matches!(
                authoring::parse_reply(&request, &reply).unwrap(),
                Reply::Refused { status, .. } if status == refusal
            ),
            "{name}"
        );
    }
    other.disconnect().unwrap();

    // An account the server does not know to have [Full Access]: refused
    // by the service, and by the host when a client skips the check.
    let mut clerk = Service::new(target(&host, CLERK, CLERK_PASSWORD));
    let capabilities = ok(clerk.handle(&Command::Capabilities));
    let create_field = &capabilities["operations"]["create_field"];
    assert_eq!(create_field["available"], false);
    assert_eq!(create_field["kind"], "permission_denied");
    let denied = error(clerk.handle(&create("Due date", FieldType::Date, &base)));
    assert_eq!(denied["kind"], "permission_denied");
    let (mut session, file_id) = session(&host, "Clerk desk", CLERK, CLERK_PASSWORD);
    let request = Request::CreateField(authoring::CreateField {
        table: 0x8001,
        expected: revision(&base),
        data_type: 3,
        name: "Due date".into(),
        comment: String::new(),
    });
    let reply = session
        .perform(&authoring::build_request(9, file_id, &request).unwrap())
        .unwrap();
    assert!(matches!(
        authoring::parse_reply(&request, &reply).unwrap(),
        Reply::Refused {
            status: status::PERMISSION_DENIED,
            ..
        }
    ));
    session.disconnect().unwrap();

    // Nothing reached the file, on disk or as served.
    assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
    assert_eq!(tasks(&mut service)["fields_revision"], base.as_str());
    host.stop();

    // A host that does not claim the extension, as FileMaker Server does
    // not, is sent nothing.
    let foreign = host_configured(
        &file,
        "OpenMaker Server test",
        om_server::Server::without_authoring,
    );
    let mut service = Service::new(target(&foreign, ACCOUNT, PASSWORD));
    let capabilities = ok(service.handle(&Command::Capabilities));
    assert_eq!(
        capabilities["operations"]["create_field"]["kind"],
        "unsupported"
    );
    let refused = error(service.handle(&create("Due date", FieldType::Date, &base)));
    assert_eq!(refused["kind"], "unsupported");
    assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
    foreign.stop();
}
