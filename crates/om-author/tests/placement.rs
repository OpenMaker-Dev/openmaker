//! Placing a field on a layout end to end against an OpenMaker Server on
//! loopback hosting a synthetic file with a simple Form layout: create a
//! field, plan and place it with a label, read the layout back after the
//! server restarts as the desktop client decodes it, and store a value in
//! the field through the client's record writes; and the placements the
//! host and service refuse, which leave the file as it was.

mod common;

use common::*;
use om_author::api::{
    ArrangeObjectsParams, ChangePartsParams, CreateFieldParams, CreateLayoutParams,
    DeleteLayoutParams, DeleteObjectsParams, DuplicateLayoutParams, FieldControlParams, FieldType,
    FrameParams, InspectParams, LabelParams, LayoutParams, LayoutSetupParams, LayoutWidthParams,
    MoveObjectsParams, ObjectFrameParams, ObjectFramesParams, PageMargins, Part, PartOptionsParams,
    PlaceFieldParams, PlacePortalParams, PlaceTextParams, PlacementParams, TextParams,
};
use om_author::{Command, Service};
use om_data::DataProvider;
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;
use om_protocol::authoring::{self, catalog_revision, status, Reply, Request};
use serde_json::{json, Value};

const TASKS_LAYOUT: &str = "layout:1@Tasks.fmp12";

fn frame(left: f64, top: f64, width: f64, height: f64) -> FrameParams {
    FrameParams {
        left,
        top,
        width,
        height,
    }
}

fn get_layout(service: &mut Service) -> Value {
    ok(service.handle(&Command::GetLayout(LayoutParams {
        layout: None,
        layout_name: Some("Tasks".into()),
    })))
}

/// The placement of `field_name` on the Tasks layout at `at`, labelled
/// "Follow up" to its left.
fn placement(field_name: &str, at: FrameParams, expected: Option<&str>) -> PlacementParams {
    PlacementParams {
        layout: Some(TASKS_LAYOUT.into()),
        layout_name: None,
        field: None,
        field_name: Some(field_name.into()),
        frame: at,
        label: Some(LabelParams {
            text: "Follow up".into(),
            frame: frame(20.0, 80.0, 80.0, 20.0),
        }),
        expected_revision: expected.map(str::to_owned),
    }
}

fn place(plan: &PlacementParams, expected: &str) -> Command {
    Command::PlaceField(PlaceFieldParams {
        layout: plan.layout.clone(),
        layout_name: plan.layout_name.clone(),
        field: plan.field.clone(),
        field_name: plan.field_name.clone(),
        frame: plan.frame,
        label: plan.label.clone(),
        expected_revision: expected.into(),
    })
}

/// An object `get_layout` lists, by key.
fn object(layout: &Value, key: u64) -> Value {
    layout["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["key"] == key)
        .unwrap_or_else(|| panic!("object {key} listed: {layout:#}"))
        .clone()
}

/// The first record's value of `name` on a page.
fn value(page: &om_data::RecordPage, name: &str) -> Option<String> {
    let index = page.fields.iter().position(|f| f.name == name)?;
    page.records.first()?.values[index].clone()
}

/// set_object_frames moves and resizes the Name field at the revision
/// get_layout gave: get_layout then shows it at its new frame, the other
/// objects where they were; repeated, it reports already_applied; at a stale
/// revision with another frame it is a conflict; a frame outside the body is
/// refused; and the field is where it was put after the server restarts.
#[test]
fn an_object_is_moved_and_resized_and_lasts_a_restart() {
    let file = TempFile::from_tree("author-move", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let before = get_layout(&mut service);
    let base = text(&before["revision"]).to_owned();
    let field = format!("layout_object:1.{NAME_FIELD}@Tasks.fmp12");
    let set = |at: FrameParams, expected: &str| {
        Command::SetObjectFrames(ObjectFramesParams {
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            objects: vec![ObjectFrameParams {
                object: field.clone(),
                frame: at,
            }],
            expected_revision: expected.into(),
        })
    };
    let moved = ok(service.handle(&set(frame(130.0, 80.0, 220.0, 24.0), &base)));
    assert_eq!(moved["status"], "moved", "{moved:#}");
    assert_eq!(moved["verified"]["fresh_connection"], true);
    let after = get_layout(&mut service);
    let shown = object(&after, NAME_FIELD);
    assert_eq!(
        shown["frame"],
        json!({"left": 130.0, "top": 80.0, "width": 220.0, "height": 24.0}),
        "{shown:#}"
    );
    for key in [NAME_LABEL, FOOT_RECTANGLE] {
        assert_eq!(object(&after, key)["frame"], object(&before, key)["frame"]);
    }
    let again = ok(service.handle(&set(frame(130.0, 80.0, 220.0, 24.0), &base)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let stale = error(service.handle(&set(frame(140.0, 80.0, 220.0, 24.0), &base)));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    let current = text(&after["revision"]).to_owned();
    let outside = error(service.handle(&set(frame(130.0, 230.0, 220.0, 24.0), &current)));
    assert_eq!(outside["kind"], "invalid_request", "{outside:#}");

    // arrange_objects aligns the label and the moved field's left edges to
    // the leftmost through the same write; arranged already, it changes
    // nothing.
    let label = format!("layout_object:1.{NAME_LABEL}@Tasks.fmp12");
    let arrange = |arrangement: &str, objects: &[&str], expected: &str| {
        Command::ArrangeObjects(ArrangeObjectsParams {
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            objects: objects.iter().map(|o| o.to_string()).collect(),
            arrangement: arrangement.into(),
            expected_revision: expected.into(),
        })
    };
    let left = |layout: &Value, key: u64| object(layout, key)["frame"]["left"].as_f64().unwrap();
    let leftmost = left(&after, NAME_LABEL).min(130.0);
    let aligned = ok(service.handle(&arrange("align_left_edges", &[&label, &field], &current)));
    assert_eq!(aligned["status"], "moved", "{aligned:#}");
    assert_eq!(aligned["arrangement"], "align_left_edges");
    let lined = get_layout(&mut service);
    assert_eq!(
        (left(&lined, NAME_LABEL), left(&lined, NAME_FIELD)),
        (leftmost, leftmost)
    );
    assert_eq!(
        object(&lined, FOOT_RECTANGLE)["frame"],
        object(&after, FOOT_RECTANGLE)["frame"]
    );
    let lined_revision = text(&lined["revision"]).to_owned();
    let unchanged = ok(service.handle(&arrange(
        "align_left_edges",
        &[&label, &field],
        &lined_revision,
    )));
    assert_eq!(unchanged["status"], "unchanged", "{unchanged:#}");
    // At the revision it was asked on (a retry after a lost answer) the
    // objects are as asked; a revision that is none is refused.
    let retried = ok(service.handle(&arrange("align_left_edges", &[&label, &field], &current)));
    assert_eq!(retried["status"], "already_applied", "{retried:#}");
    let malformed = error(service.handle(&arrange("align_left_edges", &[&label, &field], "r")));
    assert_eq!(malformed["kind"], "invalid_request", "{malformed:#}");
    let unknown = error(service.handle(&arrange(
        "align_middles",
        &[&label, &field],
        &lined_revision,
    )));
    assert_eq!(unknown["kind"], "invalid_request", "{unknown:#}");
    let alone = error(service.handle(&arrange("align_left_edges", &[&label], &lined_revision)));
    assert_eq!(alone["kind"], "invalid_request", "{alone:#}");
    let shown = object(&lined, NAME_FIELD).clone();
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let kept = get_layout(&mut service);
    assert_eq!(object(&kept, NAME_FIELD)["frame"], shown["frame"]);
    reopened.stop();
}

#[test]
fn relative_move_clamps_the_selection_and_refuses_to_apply_twice() {
    let file = TempFile::from_tree("author-relative-move", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let before = get_layout(&mut service);
    let command = Command::MoveObjects(MoveObjectsParams {
        layout: Some(TASKS_LAYOUT.into()),
        layout_name: None,
        objects: [NAME_FIELD, NAME_LABEL]
            .map(|key| format!("layout_object:1.{key}@Tasks.fmp12"))
            .to_vec(),
        dx: -100.0,
        dy: 1.6,
        expected_revision: text(&before["revision"]).into(),
    });
    let moved = ok(service.handle(&command));
    assert_eq!(moved["status"], "moved");
    assert_eq!(moved["verified"]["fresh_connection"], true);
    let after = get_layout(&mut service);
    for key in [NAME_FIELD, NAME_LABEL] {
        let mut expected = object(&before, key)["frame"].clone();
        expected["left"] = json!(expected["left"].as_f64().unwrap() - 20.0);
        expected["top"] = json!(expected["top"].as_f64().unwrap() + 2.0);
        assert_eq!(object(&after, key)["frame"], expected);
    }
    let retry = error(service.handle(&command));
    assert_eq!(retry["kind"], "conflict");
    assert_eq!(get_layout(&mut service)["revision"], after["revision"]);
}

/// delete_objects deletes the Name field at the revision get_layout gave:
/// get_layout then lists the other objects only, as they were; repeated, it
/// reports already_applied; at a stale revision another object's deletion
/// is a conflict; and the field stays deleted after the server restarts.
#[test]
fn an_object_is_deleted_and_stays_deleted_after_a_restart() {
    let file = TempFile::from_tree("author-delete", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let before = get_layout(&mut service);
    let base = text(&before["revision"]).to_owned();
    let delete = |key: u64, expected: &str| {
        Command::DeleteObjects(DeleteObjectsParams {
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            objects: vec![format!("layout_object:1.{key}@Tasks.fmp12")],
            expected_revision: expected.into(),
        })
    };
    let deleted = ok(service.handle(&delete(NAME_FIELD, &base)));
    assert_eq!(deleted["status"], "deleted", "{deleted:#}");
    assert_eq!(deleted["verified"]["fresh_connection"], true);
    let after = get_layout(&mut service);
    let keys = |layout: &Value| -> Vec<Value> {
        layout["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["key"].clone())
            .collect()
    };
    assert!(!keys(&after).contains(&json!(NAME_FIELD)), "{after:#}");
    assert_eq!(keys(&after).len() + 1, keys(&before).len());
    // The others keep their frames and identities (only their stacking
    // numbers close the gap).
    for key in [NAME_LABEL, FOOT_RECTANGLE] {
        for property in ["frame", "kind", "uuid"] {
            assert_eq!(
                object(&after, key)[property],
                object(&before, key)[property]
            );
        }
    }
    let again = ok(service.handle(&delete(NAME_FIELD, &base)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let stale = error(service.handle(&delete(NAME_LABEL, &base)));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let kept = get_layout(&mut service);
    assert_eq!(keys(&kept), keys(&after));
    reopened.stop();
}

/// place_text puts a text on the layout's body at the revision get_layout
/// gave: get_layout then lists it on top; repeated, it reports
/// already_applied; another text at a stale revision is a conflict; and the
/// text is there after the server restarts.
#[test]
fn a_text_is_placed_on_its_own_and_lasts_a_restart() {
    let file = TempFile::from_tree("author-text", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let before = get_layout(&mut service);
    let base = text(&before["revision"]).to_owned();
    let place = |words: &str, expected: &str| {
        Command::PlaceText(PlaceTextParams {
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            text: words.into(),
            frame: frame(20.0, 120.0, 100.0, 20.0),
            expected_revision: expected.into(),
        })
    };
    let placed = ok(service.handle(&place("Notes", &base)));
    assert_eq!(placed["status"], "placed", "{placed:#}");
    assert_eq!(placed["verified"]["fresh_connection"], true);
    let after = get_layout(&mut service);
    let objects = after["objects"].as_array().unwrap();
    let top = objects.last().unwrap();
    assert_eq!(top["ref"], placed["object"], "{after:#}");
    assert_eq!(top["kind"], "text");
    assert_eq!(
        top["frame"],
        json!({"left": 20.0, "top": 120.0, "width": 100.0, "height": 20.0})
    );
    assert_eq!(
        objects.len(),
        before["objects"].as_array().unwrap().len() + 1
    );
    let again = ok(service.handle(&place("Notes", &base)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let stale = error(service.handle(&place("Other", &base)));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let kept = get_layout(&mut service);
    assert_eq!(kept["objects"].as_array().unwrap().last(), Some(top));
    reopened.stop();
}

/// duplicate_layout copies the Tasks layout at the revision inspect gave:
/// the copy, "Tasks Copy", lists the source's objects at their frames;
/// repeated, it reports already_applied; at another revision it is a
/// conflict; and the copy is there after the server restarts.
#[test]
fn a_duplicated_layout_keeps_its_objects_and_lasts_a_restart() {
    let mut tree = form_tree();
    fixture::index_layouts(&mut tree);
    let file = TempFile::from_tree("author-duplicate", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let inspected = ok(service.handle(&Command::Inspect(InspectParams {
        include: Some(vec![Part::Layouts]),
    })));
    let base = text(&inspected["layouts_revision"]).to_owned();
    let duplicate = |expected: &str| {
        Command::DuplicateLayout(DuplicateLayoutParams {
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            expected_revision: expected.into(),
        })
    };
    let copied = ok(service.handle(&duplicate(&base)));
    assert_eq!(copied["status"], "created", "{copied:#}");
    assert_eq!(copied["layout"]["name"], "Tasks Copy");
    assert_eq!(copied["verified"]["fresh_connection"], true);
    let copy_ref = text(&copied["layout"]["ref"]).to_owned();
    let source = get_layout(&mut service);
    let frames = |layout: &Value| -> Vec<(Value, Value, Value)> {
        layout["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| (o["key"].clone(), o["kind"].clone(), o["frame"].clone()))
            .collect()
    };
    let get = |service: &mut Service, reference: &str| {
        ok(service.handle(&Command::GetLayout(LayoutParams {
            layout: Some(reference.into()),
            layout_name: None,
        })))
    };
    assert_eq!(frames(&get(&mut service, &copy_ref)), frames(&source));
    let again = ok(service.handle(&duplicate(&base)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    assert_eq!(again["layout"]["ref"], copy_ref);
    let stale = error(service.handle(&duplicate(&format!("sha256:{}", "0".repeat(64)))));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    assert_eq!(frames(&get(&mut service, &copy_ref)), frames(&source));
    reopened.stop();
}

/// delete_layout deletes the Notes layout at the revision inspect gave:
/// get_layout no longer finds it; repeated, it reports already_applied;
/// another layout's deletion at the old revision is a conflict; and it
/// stays deleted after the server restarts.
#[test]
fn a_deleted_layout_is_gone_and_stays_gone_after_a_restart() {
    let mut tree = form_tree();
    fixture::index_layouts(&mut tree);
    let file = TempFile::from_tree("author-delete-layout", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let inspected = ok(service.handle(&Command::Inspect(InspectParams {
        include: Some(vec![Part::Layouts]),
    })));
    let base = text(&inspected["layouts_revision"]).to_owned();
    let delete = |name: &str, expected: &str| {
        Command::DeleteLayout(DeleteLayoutParams {
            layout: None,
            layout_name: Some(name.into()),
            expected_revision: expected.into(),
        })
    };
    let notes = |service: &mut Service| {
        service.handle(&Command::GetLayout(LayoutParams {
            layout: None,
            layout_name: Some("Notes".into()),
        }))
    };
    let notes_ref = text(&ok(notes(&mut service))["layout"]["ref"]).to_owned();
    let deleted = ok(service.handle(&delete("Notes", &base)));
    assert_eq!(deleted["status"], "deleted", "{deleted:#}");
    assert_eq!(deleted["verified"]["fresh_connection"], true);
    assert_eq!(error(notes(&mut service))["kind"], "not_found");
    let again = ok(service.handle(&Command::DeleteLayout(DeleteLayoutParams {
        layout: Some(notes_ref),
        layout_name: None,
        expected_revision: base.clone(),
    })));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let stale = error(service.handle(&delete("Tasks", &base)));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    assert_eq!(error(notes(&mut service))["kind"], "not_found");
    reopened.stop();
}

/// layout_setup renames a layout, sets its views and default view, leaves
/// it out of the layout menus and prints it within fixed page margins (in
/// one commit, operation 23), at the revision get_layout gave; repeated, it
/// reports already_applied; at a stale revision with other values it is
/// refused, though its margins are already there; a refused setup changes
/// no margins; and the layout is so after the server restarts.
#[test]
fn a_set_up_layout_keeps_its_name_views_and_menus_after_a_restart() {
    let mut tree = form_tree();
    fixture::index_layouts(&mut tree);
    // Notes (layout 2) Form only, in its member and its entry, as every
    // layout FileMaker makes has view flags.
    let member: [&[u8]; 4] = [&[4], &[5], &[2], &[0xf1]];
    let entry: [&[u8]; 5] = [&[4], &[1], &[7], &[2], &[0xf1]];
    for path in [&member[..], &entry[..]] {
        tree.put(
            path,
            OwnedNode::leaf(Vec::new(), 0x10f3_080d_u64.to_be_bytes().to_vec()),
        );
    }
    let file = TempFile::from_tree("author-layout-setup", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let get = |service: &mut Service, name: &str| {
        service.handle(&Command::GetLayout(LayoutParams {
            layout: None,
            layout_name: Some(name.into()),
        }))
    };
    let notes = ok(get(&mut service, "Notes"));
    let notes_ref = text(&notes["layout"]["ref"]).to_owned();
    let base = text(&notes["revision"]).to_owned();
    let setup = |expected: &str, name: &str| {
        Command::LayoutSetup(LayoutSetupParams {
            layout: Some(notes_ref.clone()),
            layout_name: None,
            expected_revision: expected.into(),
            name: Some(name.into()),
            views: Some(vec!["form".into(), "list".into()]),
            default_view: Some("list".into()),
            include_in_layout_menus: Some(false),
            // From Pro's flags above: the indicator's bit is set (off) and
            // Quick Find's clear (on).
            options: Some(
                [
                    ("show_record_indicator".to_owned(), true),
                    ("enable_quick_find".to_owned(), false),
                ]
                .into(),
            ),
            fixed_page_margins: Some(true),
            // Layout Setup's top, bottom, left and right, as FileMaker Pro
            // stored them in #609's runs.
            page_margins: Some(PageMargins {
                top: 50.0,
                bottom: 60.0,
                left: 70.0,
                right: 80.0,
            }),
        })
    };
    let set = ok(service.handle(&setup(&base, "Meeting notes")));
    assert_eq!(set["status"], "set_up", "{set:#}");
    assert_eq!(set["verified"]["fresh_connection"], true);
    assert_eq!(set["views"], json!(["form", "list"]));
    assert_eq!(set["default_view"], "list");
    assert_eq!(set["options"]["show_record_indicator"], true);
    assert_eq!(set["options"]["enable_quick_find"], false);
    assert_eq!(set["options"]["save_record_changes"], true);
    assert_eq!(set["fixed_page_margins"], true);
    assert_eq!(error(get(&mut service, "Notes"))["kind"], "not_found");
    let again = ok(service.handle(&setup(&base, "Meeting notes")));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let stale = error(service.handle(&setup(&base, "Other notes")));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    // Other margins with a name Layout Setup refuses: nothing is written.
    let Command::LayoutSetup(mut unnamed) = setup(text(&set["revision"]), "") else {
        unreachable!()
    };
    unnamed.page_margins = Some(PageMargins {
        top: 90.0,
        bottom: 90.0,
        left: 90.0,
        right: 90.0,
    });
    error(service.handle(&Command::LayoutSetup(unnamed)));
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let renamed = ok(get(&mut service, "Meeting notes"));
    assert_eq!(text(&renamed["revision"]), text(&set["revision"]));
    assert_eq!(renamed["fixed_page_margins"], true);
    assert_eq!(
        renamed["page_margins"],
        json!({"top": 50.0, "bottom": 60.0, "left": 70.0, "right": 80.0})
    );
    reopened.stop();
}

/// A rename keeps what a layout's flags hold only in its catalog index
/// entry (#977): "Use fixed page margins" (`f1` bit `0x40`) and "Include in
/// layout menus" cleared (`f0` bit `0x08`) stay as they are, and both
/// `layout_setup` and `get_layout` report them.
#[test]
fn a_rename_keeps_flags_the_layout_holds_only_in_its_index_entry() {
    let mut tree = form_tree();
    fixture::index_layouts(&mut tree);
    for key in [0xf1u8, 0xf0] {
        tree.remove(&[&[4], &[5], &[2], &[key]]);
    }
    for (key, value) in [(0xf1u8, 0x40u64), (0xf0, 0x08)] {
        tree.put(
            &[&[4], &[1], &[7], &[2], &[key]],
            OwnedNode::leaf(Vec::new(), value.to_be_bytes().to_vec()),
        );
    }
    let file = TempFile::from_tree("author-layout-setup-index", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let get = |service: &mut Service, name: &str| {
        ok(service.handle(&Command::GetLayout(LayoutParams {
            layout: None,
            layout_name: Some(name.into()),
        })))
    };
    let notes = get(&mut service, "Notes");
    assert_eq!(notes["fixed_page_margins"], true, "{notes:#}");
    let set = ok(service.handle(&Command::LayoutSetup(LayoutSetupParams {
        layout: Some(text(&notes["layout"]["ref"]).to_owned()),
        layout_name: None,
        expected_revision: text(&notes["revision"]).to_owned(),
        name: Some("Meeting notes".into()),
        views: None,
        default_view: None,
        include_in_layout_menus: None,
        options: None,
        fixed_page_margins: None,
        page_margins: None,
    })));
    assert_eq!(set["status"], "set_up", "{set:#}");
    assert_eq!(set["verified"]["fresh_connection"], true, "{set:#}");
    assert_eq!(set["fixed_page_margins"], true);
    assert_eq!(set["include_in_layout_menus"], false);
    let renamed = get(&mut service, "Meeting notes");
    assert_eq!(renamed["fixed_page_margins"], true, "{renamed:#}");
    host.stop();
}

/// set_field_control shows the Name field as a pop-up menu with a value
/// list, then as an edit box again; repeated, it reports already_applied; a
/// calendar on the text field and a list control without a value list are
/// refused; the control lasts a restart.
#[test]
fn a_fields_control_is_set_and_lasts_a_restart() {
    let mut tree = form_tree();
    // A value list for the pop-up: the host checks that the file has it.
    tree.put(
        &[&[0x21], &[5], &[1]],
        OwnedNode::list(vec![1], vec![om_scripts::text_node(&[0x10], "Statuses")]),
    );
    let file = TempFile::from_tree("author-field-control", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let object = format!("layout_object:1.{NAME_FIELD}@Tasks.fmp12");
    let set = |expected: &str, control: &str, list: Option<&str>| {
        Command::SetFieldControl(FieldControlParams {
            object: object.clone(),
            expected_revision: expected.into(),
            control: control.into(),
            value_list: list.map(Into::into),
        })
    };
    let base = text(&get_layout(&mut service)["revision"]).to_owned();
    let list = Some("value_list:1@Tasks.fmp12");
    let popup = ok(service.handle(&set(&base, "pop_up_menu", list)));
    assert_eq!(popup["status"], "set", "{popup:#}");
    assert_eq!(popup["verified"]["fresh_connection"], true);
    let again = ok(service.handle(&set(&base, "pop_up_menu", list)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let current = text(&popup["revision"]).to_owned();
    for (control, list) in [
        ("drop_down_calendar", None),
        ("checkbox_set", Some("value_list:9@Tasks.fmp12")),
    ] {
        let refused = error(service.handle(&set(&current, control, list)));
        assert_eq!(refused["kind"], "invalid_request", "{refused:#}");
    }
    // At an old revision an object the layout lacks (another client
    // deleted it) is a conflict, as any change there is, for the client to
    // read the layout again.
    let gone = error(
        service.handle(&Command::SetFieldControl(FieldControlParams {
            object: "layout_object:1.99@Tasks.fmp12".into(),
            expected_revision: base.clone(),
            control: "pop_up_menu".into(),
            value_list: list.map(Into::into),
        })),
    );
    assert_eq!(gone["kind"], "conflict", "{gone:#}");
    let edit = ok(service.handle(&set(&current, "edit_box", None)));
    assert_eq!(edit["status"], "set", "{edit:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    assert_eq!(
        text(&get_layout(&mut service)["revision"]),
        text(&edit["revision"])
    );
    reopened.stop();
}

/// set_layout_width sets a layout's width at the revision get_layout gave;
/// repeated, it reports already_applied; at a stale revision with another
/// width it is refused; and the width is kept after the server restarts.
#[test]
fn a_layouts_width_is_set_and_lasts_a_restart() {
    let file = TempFile::from_tree("author-layout-width", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let before = get_layout(&mut service);
    let base = text(&before["revision"]).to_owned();
    let set = |expected: &str, width: f64| {
        Command::SetLayoutWidth(LayoutWidthParams {
            layout: None,
            layout_name: Some("Tasks".into()),
            expected_revision: expected.into(),
            width,
        })
    };
    let done = ok(service.handle(&set(&base, 900.0)));
    assert_eq!(done["status"], "set", "{done:#}");
    assert_eq!(done["verified"]["fresh_connection"], true);
    let again = ok(service.handle(&set(&base, 900.0)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let stale = error(service.handle(&set(&base, 800.0)));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    let refused = error(service.handle(&set(text(&done["revision"]), 0.0)));
    assert_eq!(refused["kind"], "invalid_request", "{refused:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let after = get_layout(&mut service);
    assert_eq!(text(&after["revision"]), text(&done["revision"]));
    reopened.stop();
}

/// set_part_options sets a body's page options and a sub-summary's sort
/// field at the revision get_layout gave; repeated, it reports
/// already_applied; a header's options and an option of another name are
/// refused; both parts keep their options after the server restarts.
#[test]
fn a_parts_options_are_set_and_last_a_restart() {
    use om_layout::encode::{double, uint};
    let mut tree = form_tree();
    let mut summary = Vec::new();
    uint(1, 3, &mut summary);
    double(4, 240.0, &mut summary);
    double(5, 20.0, &mut summary);
    let part: [&[u8]; 5] = [&[4], &[5], &[1], &[3], &[3]];
    tree.put(
        &[&part[..], &[&[2]]].concat(),
        OwnedNode::leaf(Vec::new(), summary),
    );
    tree.put(
        &[&part[..], &[&[7]]].concat(),
        OwnedNode::leaf(Vec::new(), vec![0; 4]),
    );
    let file = TempFile::from_tree("author-part-options", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let base = text(&get_layout(&mut service)["revision"]).to_owned();
    let set = |expected: &str, part: u64, options: &[(&str, bool)], sorted_by: Option<&str>| {
        Command::SetPartOptions(PartOptionsParams {
            layout: None,
            layout_name: Some("Tasks".into()),
            expected_revision: expected.into(),
            part,
            options: Some(
                options
                    .iter()
                    .map(|(n, on)| ((*n).to_owned(), *on))
                    .collect(),
            ),
            page_break_after: (part == 2).then_some(3),
            sorted_by: sorted_by.map(Into::into),
        })
    };
    // A break every 3 occurrences turns the break before off, as Pro's
    // Part Definition does.
    let options = [("page_break_before", true), ("break_across_pages", true)];
    let body = ok(service.handle(&set(&base, 2, &options, None)));
    assert_eq!(body["status"], "set", "{body:#}");
    assert_eq!(body["verified"]["fresh_connection"], true);
    assert_eq!(body["options"]["break_across_pages"], true);
    assert_eq!(body["options"]["page_break_before"], false);
    assert_eq!(body["page_break_after"], 3);
    let again = ok(service.handle(&set(&base, 2, &options, None)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let current = text(&body["revision"]).to_owned();
    let name = format!("field:{}.{}@Tasks.fmp12", fixture::TABLE, fixture::NAME);
    let sorted = ok(service.handle(&set(&current, 3, &[], Some(&name))));
    assert_eq!(sorted["status"], "set", "{sorted:#}");
    let current = text(&sorted["revision"]).to_owned();
    let header = error(service.handle(&set(&current, 1, &options, None)));
    assert_eq!(header["kind"], "unsupported", "{header:#}");
    let unknown =
        error(service.handle(&set(&current, 2, &[("page_break_after_every", true)], None)));
    assert_eq!(unknown["kind"], "invalid_request", "{unknown:#}");
    // The host refuses a sort field whose occurrence no longer shows the
    // table the client chose it from, as a client that skips the service's
    // checks sends it.
    let (mut other, other_file) = session(&host, "Other desk", ACCOUNT, PASSWORD);
    // At the current revision it is invalid; at an old one, though the
    // part holds that occurrence and field, it is no retry but a conflict.
    for (expected, refusal) in [(&current, status::INVALID), (&base, status::CONFLICT)] {
        let sent = Request::PartOptions(authoring::PartOptions {
            expected: revision(expected),
            layout: 1,
            part: 3,
            flags: 0,
            page_break_after: 0,
            break_field: Some((fixture::OCCURRENCE, fixture::TABLE + 1, fixture::NAME)),
        });
        let reply = other
            .perform(&authoring::build_request(9, other_file, &sent).unwrap())
            .unwrap();
        assert!(matches!(
            authoring::parse_reply(&sent, &reply).unwrap(),
            Reply::Refused { status, .. } if status == refusal
        ));
    }
    other.disconnect().unwrap();
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    assert_eq!(text(&get_layout(&mut service)["revision"]), current);
    reopened.stop();
}

/// set_text changes a label's words at the revision get_layout gave;
/// repeated, it reports already_applied; a field object and a merge field
/// are refused; the words last a restart.
#[test]
fn a_labels_words_are_set_and_last_a_restart() {
    let file = TempFile::from_tree("author-set-text", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let base = text(&get_layout(&mut service)["revision"]).to_owned();
    let set = |expected: &str, object: u64, words: &str| {
        Command::SetText(TextParams {
            object: format!("layout_object:1.{object}@Tasks.fmp12"),
            expected_revision: expected.into(),
            text: words.into(),
        })
    };
    let done = ok(service.handle(&set(&base, NAME_LABEL, "Task name")));
    assert_eq!(done["status"], "set", "{done:#}");
    assert_eq!(done["verified"]["fresh_connection"], true);
    let again = ok(service.handle(&set(&base, NAME_LABEL, "Task name")));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let current = text(&done["revision"]).to_owned();
    let field = error(service.handle(&set(&current, NAME_FIELD, "Task name")));
    assert_eq!(field["kind"], "invalid_request", "{field:#}");
    let merge = error(service.handle(&set(&current, NAME_LABEL, "<<Name>>")));
    assert_eq!(merge["kind"], "unsupported", "{merge:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    assert_eq!(text(&get_layout(&mut service)["revision"]), current);
    reopened.stop();
}

/// place_portal puts a portal of the related Notes on the Tasks layout with
/// Notes::Body in its first row; repeated, it reports already_applied; the
/// layout's own occurrence and one the relationship graph does not join to
/// it are refused; and the portal is there after the server restarts.
#[test]
fn a_portal_is_placed_with_its_field_and_lasts_a_restart() {
    let mut tree = form_tree();
    fixture::add_relationship(
        &mut tree,
        1,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (fixture::NOTES_OCCURRENCE, fixture::BODY),
    );
    fixture::add_occurrence(
        &mut tree,
        fixture::OTHER_NOTES,
        "Loose notes",
        fixture::NOTES,
    );
    let file = TempFile::from_tree("author-place-portal", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let before = get_layout(&mut service);
    let base = text(&before["revision"]).to_owned();
    let place = |occurrence: &str, rows: u32, expected: &str| {
        let field = if occurrence == "Tasks" {
            "Name"
        } else {
            "Body"
        };
        Command::PlacePortal(PlacePortalParams {
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            occurrence: None,
            occurrence_name: Some(occurrence.into()),
            field_names: vec![field.into()],
            rows,
            frame: frame(20.0, 80.0, 300.0, 90.0),
            expected_revision: expected.into(),
        })
    };
    let own = error(service.handle(&place("Tasks", 3, &base)));
    assert_eq!(own["kind"], "unsupported", "{own:#}");
    let unrelated = error(service.handle(&place("Loose notes", 3, &base)));
    assert_eq!(unrelated["kind"], "unsupported", "{unrelated:#}");
    let placed = ok(service.handle(&place("Notes", 3, &base)));
    assert_eq!(placed["status"], "placed", "{placed:#}");
    assert_eq!(placed["verified"]["fresh_connection"], true);
    let after = get_layout(&mut service);
    let objects = after["objects"].as_array().unwrap();
    let top = objects.last().unwrap();
    assert_eq!(top["ref"], placed["portal"], "{after:#}");
    assert_eq!(top["kind"], "portal");
    assert_eq!(
        top["frame"],
        json!({"left": 20.0, "top": 80.0, "width": 300.0, "height": 90.0})
    );
    assert_eq!(placed["fields"].as_array().map(Vec::len), Some(1));
    let again = ok(service.handle(&place("Notes", 3, &base)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let stale = error(service.handle(&place("Notes", 2, &base)));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let kept = get_layout(&mut service);
    assert_eq!(kept["revision"], placed["revision"]);
    assert_eq!(kept["objects"].as_array().unwrap().last(), Some(top));
    reopened.stop();
}

/// change_parts adds a title header above the Tasks layout's header, which
/// moves the body and its objects down; repeated, it reports
/// already_applied; the body is made shorter but not above its lowest
/// object; the empty header is deleted and the body with its objects is
/// not; and the parts are as left after the server restarts.
#[test]
fn parts_are_added_resized_and_deleted_and_last_a_restart() {
    let file = TempFile::from_tree("author-parts", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let change = |action: &str,
                  part: Option<u64>,
                  kind: Option<&str>,
                  height: Option<f64>,
                  expected: &str| {
        Command::ChangeParts(ChangePartsParams {
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            expected_revision: expected.into(),
            action: action.into(),
            part,
            kind: kind.map(Into::into),
            height,
        })
    };
    let parts = |layout: &Value| -> Vec<(String, f64, f64)> {
        layout["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    text(&p["kind"]).to_owned(),
                    p["top"].as_f64().unwrap(),
                    p["height"].as_f64().unwrap(),
                )
            })
            .collect()
    };
    let field_top = |layout: &Value| object(layout, NAME_FIELD)["frame"]["top"].as_f64().unwrap();
    let base = text(&get_layout(&mut service)["revision"]).to_owned();
    let added = ok(service.handle(&change(
        "add",
        None,
        Some("title_header"),
        Some(30.0),
        &base,
    )));
    assert_eq!(added["status"], "changed", "{added:#}");
    assert_eq!(added["verified"]["fresh_connection"], true);
    let layout = get_layout(&mut service);
    assert_eq!(
        parts(&layout),
        [
            ("title_header".to_owned(), 0.0, 30.0),
            ("header".to_owned(), 30.0, 40.0),
            ("body".to_owned(), 70.0, 200.0)
        ]
    );
    assert_eq!(field_top(&layout), 80.0);
    let again = ok(service.handle(&change(
        "add",
        None,
        Some("title_header"),
        Some(30.0),
        &base,
    )));
    assert_eq!(again["status"], "already_applied", "{again:#}");

    // The rectangle ends at 270: the body (from 70) keeps it.
    let current = text(&layout["revision"]).to_owned();
    let short = error(service.handle(&change("resize", Some(3), None, Some(150.0), &current)));
    assert_eq!(short["kind"], "invalid_request", "{short:#}");
    let resized = ok(service.handle(&change("resize", Some(3), None, Some(210.0), &current)));
    let current = text(&resized["revision"]).to_owned();
    let full = error(service.handle(&change("delete", Some(3), None, None, &current)));
    assert_eq!(full["kind"], "unsupported", "{full:#}");
    let deleted = ok(service.handle(&change("delete", Some(2), None, None, &current)));
    assert_eq!(deleted["status"], "changed", "{deleted:#}");
    // Repeated after a lost answer, naming the kind (the body is part 2
    // now): already applied, nothing deleted.
    let again = ok(service.handle(&change("delete", Some(2), Some("header"), None, &current)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    let layout = get_layout(&mut service);
    let left = parts(&layout);
    assert_eq!(
        left,
        [
            ("title_header".to_owned(), 0.0, 30.0),
            ("body".to_owned(), 30.0, 210.0)
        ]
    );
    assert_eq!(field_top(&layout), 40.0);
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let kept = get_layout(&mut service);
    assert_eq!(parts(&kept), left);
    assert_eq!(field_top(&kept), 40.0);
    reopened.stop();
}

/// create_layout adds a Form layout with an empty body on the occurrence
/// named, in the theme and width of the layout named, at the layouts
/// revision inspect gave; repeated after the answer, it reports
/// already_applied; at a stale revision with another name it is refused.
/// A field is then placed on the new layout, and both are there after the
/// server restarts.
#[test]
fn a_new_layout_takes_a_field_and_lasts_a_restart() {
    let mut tree = form_tree();
    fixture::index_layouts(&mut tree);
    let file = TempFile::from_tree("author-new-layout", &tree);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let capabilities = ok(service.handle(&Command::Capabilities));
    assert_eq!(
        capabilities["operations"]["create_layout"]["available"], true,
        "{capabilities:#}"
    );
    let inspected = ok(service.handle(&Command::Inspect(InspectParams {
        include: Some(vec![Part::Layouts]),
    })));
    let base = text(&inspected["layouts_revision"]).to_owned();
    let create = |name: &str, expected: &str| {
        Command::CreateLayout(CreateLayoutParams {
            name: name.into(),
            occurrence: None,
            occurrence_name: Some("Tasks".into()),
            layout: Some(TASKS_LAYOUT.into()),
            layout_name: None,
            expected_revision: expected.into(),
        })
    };
    let created = ok(service.handle(&create("Task Entry", &base)));
    assert_eq!(created["status"], "created", "{created:#}");
    assert_eq!(created["layout"]["ref"], "layout:3@Tasks.fmp12");
    assert_eq!(created["occurrence"], "occurrence:13631489@Tasks.fmp12");
    assert_eq!(created["verified"]["fresh_connection"], true);
    let again = ok(service.handle(&create("Task Entry", &base)));
    assert_eq!(again["status"], "already_applied", "{again:#}");
    assert_eq!(again["layout"]["ref"], "layout:3@Tasks.fmp12");
    let stale = error(service.handle(&create("Other", &base)));
    assert_eq!(stale["kind"], "conflict", "{stale:#}");
    // Another request for the same layout from the same revision, with its
    // own identity, is a conflict, not the first one's layout.
    let mut other = provider(&host);
    let independent = other.create_layout_on_host(authoring::CreateLayout {
        expected: revision(&base),
        occurrence: fixture::OCCURRENCE,
        like: 1,
        name: "Task Entry".into(),
        uuid: [7; 16],
    });
    other.close();
    assert!(
        matches!(
            independent,
            Err(om_provider::AuthoringError::Refused {
                status: status::CONFLICT,
                ..
            })
        ),
        "{independent:?}"
    );

    let shown = ok(service.handle(&Command::GetLayout(LayoutParams {
        layout: Some("layout:3@Tasks.fmp12".into()),
        layout_name: None,
    })));
    assert_eq!(shown["table"], "table:32769@Tasks.fmp12");
    assert_eq!(shown["width"], 480.0);
    assert_eq!(shown["parts"].as_array().unwrap().len(), 1);
    assert_eq!(shown["parts"][0]["kind"], "body");
    let mut wanted = placement("Name", frame(110.0, 80.0, 200.0, 20.0), None);
    wanted.layout = Some("layout:3@Tasks.fmp12".into());
    let placed = ok(service.handle(&place(&wanted, text(&shown["revision"]))));
    assert_eq!(placed["status"], "placed", "{placed:#}");
    host.stop();

    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let after = ok(service.handle(&Command::GetLayout(LayoutParams {
        layout: None,
        layout_name: Some("Task Entry".into()),
    })));
    assert_eq!(after["objects"].as_array().unwrap().len(), 2, "{after:#}");
    reopened.stop();
}

#[test]
fn a_new_field_is_placed_on_a_form_layout_and_used_after_a_restart() {
    let file = TempFile::from_tree("author-place", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));

    let capabilities = ok(service.handle(&Command::Capabilities));
    let place_field = &capabilities["operations"]["place_field"];
    assert_eq!(place_field["available"], true, "{capabilities:#}");
    assert_eq!(place_field["forms"]["parts"], json!(["body"]));

    // A text field made with create_field (A02), not yet on any layout.
    let inspected = ok(service.handle(&Command::Inspect(InspectParams {
        include: Some(vec![Part::Tables]),
    })));
    let tasks = inspected["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["ref"] == "table:32769@Tasks.fmp12")
        .unwrap();
    let created = ok(service.handle(&Command::CreateField(CreateFieldParams {
        table: Some("table:32769@Tasks.fmp12".into()),
        table_name: None,
        name: "Follow up".into(),
        field_type: FieldType::Text,
        comment: None,
        expected_revision: text(&tasks["fields_revision"]).into(),
    })));
    assert_eq!(created["field"]["ref"], "field:32769.8@Tasks.fmp12");

    let before = get_layout(&mut service);
    let base = text(&before["revision"]).to_owned();
    assert_eq!(before["table"], "table:32769@Tasks.fmp12");
    assert_eq!(before["width"], 480.0);
    assert_eq!(before["parts"][1]["kind"], "body");
    assert_eq!(
        before["root_stack"],
        json!([
            "layout_object:1.1@Tasks.fmp12",
            "layout_object:1.2@Tasks.fmp12",
            "layout_object:1.3@Tasks.fmp12",
        ])
    );
    let name_field = object(&before, NAME_FIELD);
    assert_eq!(name_field["field"]["ref"], "field:32769.1@Tasks.fmp12");
    assert_eq!(name_field["uuid"], "22222222-2222-2222-2222-222222222222");

    let wanted = placement("Follow up", frame(110.0, 80.0, 200.0, 20.0), Some(&base));
    let plan = ok(service.handle(&Command::PlanFieldPlacement(wanted.clone())));
    assert_eq!(plan["already_applied"], false);
    assert_eq!(plan["base_revision"], base.as_str());
    assert_eq!(plan["field"]["ref"], "field:32769.8@Tasks.fmp12");
    assert_eq!(plan["objects"][0]["ref"], "layout_object:1.4@Tasks.fmp12");
    assert_eq!(plan["objects"][1]["ref"], "layout_object:1.5@Tasks.fmp12");
    assert_eq!(plan["part"]["kind"], "body");
    assert_eq!(plan["stacking"], 3);
    assert_eq!(plan["overlaps"], json!([]));
    assert_eq!(plan["changes"].as_array().unwrap().len(), 8, "{plan:#}");
    let on_disk = std::fs::read(&file.0).unwrap();

    let placed = ok(service.handle(&place(&wanted, &base)));
    assert_eq!(placed["status"], "placed", "{placed:#}");
    assert_eq!(placed["previous_revision"], base.as_str());
    let after = text(&placed["revision"]).to_owned();
    assert_ne!(after, base);
    let verified = &placed["verified"];
    assert_eq!(verified["fresh_connection"], true);
    assert_eq!(verified["revision"], after.as_str());
    assert_eq!(verified["listed_among_layout_fields"], true);
    assert_ne!(std::fs::read(&file.0).unwrap(), on_disk, "saved first");

    // The same request again (an answer lost in transit): already there,
    // nothing written; another placement from the old revision conflicts.
    let saved = std::fs::read(&file.0).unwrap();
    let retried = ok(service.handle(&place(&wanted, &base)));
    assert_eq!(retried["status"], "already_applied", "{retried:#}");
    assert_eq!(retried["objects"][0]["key"], 4);
    assert_eq!(retried["verified"]["fresh_connection"], true);
    let elsewhere = placement("Follow up", frame(110.0, 110.0, 200.0, 20.0), None);
    let conflict = error(service.handle(&place(&elsewhere, &base)));
    assert_eq!(conflict["kind"], "conflict");
    assert_eq!(conflict["details"]["current_revision"], after.as_str());
    assert_eq!(std::fs::read(&file.0).unwrap(), saved);

    // The saved file, opened by a new server.
    host.stop();
    let reopened = common::host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let read = get_layout(&mut service);
    assert_eq!(read["revision"], after.as_str());
    let field = object(&read, 4);
    assert_eq!(field["kind"], "field");
    assert_eq!(field["control"], "edit_box");
    assert_eq!(field["field"]["ref"], "field:32769.8@Tasks.fmp12");
    assert_eq!(field["field"]["name"], "Follow up");
    assert_eq!(
        field["frame"],
        json!({"left": 110.0, "top": 80.0, "width": 200.0, "height": 20.0})
    );
    assert_eq!(field["stacking"], 3);
    assert_eq!(field["uuid"], verified["field_object_uuid"]);
    let label = object(&read, 5);
    assert_eq!(
        (label["kind"].clone(), label["text"].clone()),
        ("text".into(), "Follow up".into())
    );
    assert_eq!(label["stacking"], 4);
    for key in [NAME_LABEL, NAME_FIELD, FOOT_RECTANGLE] {
        assert_eq!(object(&read, key), object(&before, key));
    }

    // As stored: appended to the stored objects, UUIDs, the field
    // reference index, the stamp, every other key as it was.
    let mut desktop = provider(&reopened);
    let member = desktop.layout_member(1).unwrap().unwrap();
    assert_eq!(catalog_revision(&member), revision(&after));
    let original = form_tree();
    let original = original.at(&[&[4], &[5], &[1]]).unwrap();
    let stored = member.child(&[7]).unwrap().assembled().unwrap();
    let old = original.child(&[7]).unwrap().value.clone().unwrap();
    assert!(stored.starts_with(&old));
    for key in [&[UNKNOWN_LAYOUT_KEY][..], &[2], &[0x10]] {
        assert_eq!(member.child(key), original.child(key), "{key:02x?}");
    }
    // The field and its label end the body's list of its objects, after
    // its stored entries; every other key of the parts is as it was
    // (FileMaker Pro's Browse did not show an edit box missing from it,
    // issue #166).
    let list = |node: &OwnedNode, part: u8| node.at(&[&[3], &[part], &[7]]).cloned();
    assert_eq!(list(&member, 1), list(original, 1));
    assert_eq!(
        list(&member, 2).unwrap().value.unwrap(),
        [0, 0, 0, 5, 2, 1, 1, 2, 1, 2, 2, 1, 3, 2, 1, 4, 2, 1, 5]
    );
    for part in [1u8, 2] {
        assert_eq!(
            member.at(&[&[3], &[part], &[2]]),
            original.at(&[&[3], &[part], &[2]])
        );
    }
    for record in &original.child(&[5]).unwrap().children {
        if record.segment != [0] {
            assert_eq!(member.at(&[&[5], &record.segment]), Some(record));
        }
    }
    // The last object key issued is the label's, and the edit box joins
    // the Name field in the object index, after the three stored objects
    // in pre-order.
    assert_eq!(
        member.at(&[&[5], &[0]]).unwrap().value.as_deref(),
        Some(&object_key_string(5)[..])
    );
    let objects = member.child(&[0x0a]).unwrap();
    assert_eq!(objects.children.len(), 2);
    let name_entry: &[u8] = &[0, 50, 0, 110, 0, 1];
    assert_eq!(
        objects.child(name_entry),
        original.at(&[&[0x0a], name_entry])
    );
    assert_eq!(
        objects
            .child(&[0, 80, 0, 110, 0, 3])
            .unwrap()
            .value
            .as_deref(),
        Some(&[0, 2, 1, 4, 0, 0, 0, 0, 1][..])
    );
    // The layout stored no tab order, so the edit box is its first stop:
    // the object index's reference at position 1, recorded in the edit
    // box's attribute `07`
    // ([#316](https://github.com/OpenMaker-Dev/openmaker/issues/316)).
    let tab = member.child(&[0x15]).unwrap();
    assert_eq!(tab.children.len(), 1);
    assert_eq!(
        tab.child(&[0, 1]).unwrap().value.as_deref(),
        Some(&[0, 2, 1, 4, 0, 0, 0, 0, 1][..])
    );
    assert_eq!(
        member.at(&[&[5], &[4], &[7]]).unwrap().value.as_deref(),
        Some(&[0, 1][..])
    );
    for uuid in [0xd8u8, 0xd9] {
        assert_eq!(
            member
                .at(&[&[5], &[4], &[uuid]])
                .unwrap()
                .value
                .as_ref()
                .unwrap()
                .len(),
            16
        );
    }
    let index = member.child(&[1]).unwrap();
    let occurrence = uint_segment(fixture::OCCURRENCE);
    assert!(index.at(&[&occurrence, &[1], &[2]]).is_some());
    assert!(index.at(&[&occurrence, &[8], &[4]]).is_some());
    assert_eq!(
        member.child(authoring::MOD_COUNT).unwrap().value.as_deref(),
        Some(&[1, 4][..])
    );

    // The desktop client lists it among the layout's fields, and its record
    // writes store a value in it.
    let names = desktop.calc_schema().unwrap().unwrap();
    let entry = names.layouts().iter().find(|l| l.id == 1).unwrap();
    assert!(entry.fields.iter().any(|f| f.object == 4 && f.field == 8));
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
    let mut fresh = provider(&reopened);
    let page = fresh.record_by_id(layout, 1).unwrap();
    assert_eq!(value(&page, "Follow up").as_deref(), Some("call back"));
    assert_eq!(value(&page, "Name").as_deref(), Some("Write tests"));
    fresh.close();
    reopened.stop();
}

#[test]
fn refused_placements_leave_the_layout_as_it_was() {
    let file = TempFile::from_tree("author-place-refusals", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let on_disk = std::fs::read(&file.0).unwrap();
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let base = text(&get_layout(&mut service)["revision"]).to_owned();
    let count = |at: FrameParams| placement("Count", at, Some(&base));

    let stale = format!("sha256:{}", "00".repeat(32));
    let conflict = error(service.handle(&place(&count(frame(110.0, 80.0, 100.0, 20.0)), &stale)));
    assert_eq!(conflict["kind"], "conflict");
    assert_eq!(conflict["details"]["current_revision"], base.as_str());

    // In the header, past the layout's width, beside a related field, from
    // another file.
    for at in [
        frame(110.0, 10.0, 100.0, 20.0),
        frame(400.0, 80.0, 100.0, 20.0),
    ] {
        let refused = error(service.handle(&Command::PlanFieldPlacement(count(at))));
        assert_eq!(refused["kind"], "invalid_request", "{refused:#}");
    }
    let mut related = count(frame(110.0, 80.0, 100.0, 20.0));
    related.field_name = None;
    related.field = Some("field:32770.1@Tasks.fmp12".into());
    assert_eq!(
        error(service.handle(&place(&related, &base)))["kind"],
        "unsupported"
    );
    let mut foreign = count(frame(110.0, 80.0, 100.0, 20.0));
    foreign.layout = Some("layout:1@Other.fmp12".into());
    assert_eq!(
        error(service.handle(&place(&foreign, &base)))["kind"],
        "wrong_file"
    );

    // Another desk holds the layout, as one editing it would.
    let (mut other, other_file) = session(&host, "Other desk", ACCOUNT, PASSWORD);
    let layout_path: [&[u8]; 3] = [&[4], &[5], &[1]];
    assert!(matches!(
        other.lock(&layout_path, 77, true).unwrap(),
        om_protocol::write::LockReply::Granted { .. }
    ));
    let locked = error(service.handle(&place(&count(frame(110.0, 80.0, 100.0, 20.0)), &base)));
    assert_eq!(locked["kind"], "locked", "{locked:#}");
    assert!(text(&locked["message"]).contains("Other desk"));
    other.kill_all_locks(77).unwrap();

    // The host refuses what a client that skips the service's checks
    // sends: a missing layout or field, a frame outside the body, a label
    // over the field.
    let request = |layout: u64, field: u64, frame: [f64; 4], label| {
        Request::PlaceField(authoring::PlaceField {
            layout,
            expected: revision(&base),
            table: fixture::TABLE,
            occurrence: fixture::OCCURRENCE,
            field,
            frame,
            label,
        })
    };
    let inside = [80.0, 110.0, 100.0, 210.0];
    for (sent, refusal) in [
        (request(9, 2, inside, None), status::LAYOUT_MISSING),
        (request(1, 99, inside, None), status::FIELD_MISSING),
        (
            request(1, 2, [10.0, 110.0, 30.0, 210.0], None),
            status::INVALID,
        ),
        (
            request(
                1,
                2,
                inside,
                Some(("Count".into(), [85.0, 100.0, 95.0, 160.0])),
            ),
            status::INVALID,
        ),
    ] {
        let reply = other
            .perform(&authoring::build_request(9, other_file, &sent).unwrap())
            .unwrap();
        assert!(
            matches!(
                authoring::parse_reply(&sent, &reply).unwrap(),
                Reply::Refused { status, .. } if status == refusal
            ),
            "{sent:?}"
        );
    }
    other.disconnect().unwrap();

    // An account the server does not know to have [Full Access]: refused
    // by the service, and by the host when a client skips the check.
    let mut clerk = Service::new(target(&host, CLERK, CLERK_PASSWORD));
    let capabilities = ok(clerk.handle(&Command::Capabilities));
    assert_eq!(
        capabilities["operations"]["place_field"]["kind"],
        "permission_denied"
    );
    let denied = error(clerk.handle(&place(&count(frame(110.0, 80.0, 100.0, 20.0)), &base)));
    assert_eq!(denied["kind"], "permission_denied");
    let (mut session, file_id) = session(&host, "Clerk desk", CLERK, CLERK_PASSWORD);
    let sent = request(1, 2, inside, None);
    let reply = session
        .perform(&authoring::build_request(9, file_id, &sent).unwrap())
        .unwrap();
    assert!(matches!(
        authoring::parse_reply(&sent, &reply).unwrap(),
        Reply::Refused {
            status: status::PERMISSION_DENIED,
            ..
        }
    ));
    session.disconnect().unwrap();

    // Nothing reached the file, on disk or as served.
    assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
    assert_eq!(get_layout(&mut service)["revision"], base.as_str());
    host.stop();

    // A host that does not claim the extension, as FileMaker Server does
    // not, is sent nothing.
    let foreign = host_configured(
        &file,
        "OpenMaker Server test",
        om_server::Server::without_authoring,
    );
    let mut service = Service::new(target(&foreign, ACCOUNT, PASSWORD));
    let refused = error(service.handle(&place(&count(frame(110.0, 80.0, 100.0, 20.0)), &base)));
    assert_eq!(refused["kind"], "unsupported");
    assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
    foreign.stop();
}

#[test]
fn layout_names_and_revisions_share_one_downloaded_snapshot() {
    let before = form_tree();
    let old_catalog = before.child(&[4]).unwrap().to_wire().unwrap();
    let old_revision = catalog_revision(before.at(&[&[4], &[5], &[1]]).unwrap());
    let mut current = before.clone();
    current.put(
        &[&[4], &[5], &[1], &[0x10]],
        om_scripts::text_node(&[], "Renamed Tasks"),
    );
    let file = TempFile::from_tree("placement-name-snapshot", &current);
    let bytes = std::fs::read(&file.0).unwrap();
    // Metadata can be an earlier observation, but its name and revision
    // must be from that observation together. The host has the rename.
    let host = host_configured(&file, "OpenMaker Server test", |server| {
        server.overriding_download_slots(&[&[4]], old_catalog)
    });
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let observed = get_layout(&mut service);
    assert_eq!(observed["layout"]["name"], "Tasks");
    assert_eq!(revision(text(&observed["revision"])), old_revision);
    let request = placement("Count", frame(110.0, 80.0, 100.0, 20.0), None);
    let refused = error(service.handle(&place(&request, text(&observed["revision"]))));
    assert_eq!(refused["kind"], "conflict");
    let mut reader = provider(&host);
    let actual = reader.layout_member(1).unwrap().unwrap();
    assert_eq!(
        om_scripts::node_text(actual.child(&[0x10]).unwrap()).as_deref(),
        Some("Renamed Tasks")
    );
    assert_ne!(catalog_revision(&actual), old_revision);
    reader.close();
    host.stop();
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
}

#[test]
fn a_retargeted_occurrence_refuses_placement_before_any_write() {
    let before = form_tree();
    let old_schema = before.child(&[3]).unwrap().to_wire().unwrap();
    let mut current = before.clone();
    fixture::add_occurrence(&mut current, fixture::OCCURRENCE, "Tasks", fixture::NOTES);
    // The occurrence mapping changed; the layout's CAS token did not.
    assert_eq!(
        catalog_revision(current.at(&[&[4], &[5], &[1]]).unwrap()),
        catalog_revision(before.at(&[&[4], &[5], &[1]]).unwrap())
    );
    let file = TempFile::from_tree("placement-retargeted-owner", &current);
    let bytes = std::fs::read(&file.0).unwrap();
    let host = host_configured(&file, "OpenMaker Server test", |server| {
        server.overriding_download_slots(&[&[3]], old_schema)
    });
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let observed = get_layout(&mut service);
    let request = PlacementParams {
        layout: Some(TASKS_LAYOUT.into()),
        layout_name: None,
        field: Some("field:32769.1@Tasks.fmp12".into()),
        field_name: None,
        frame: frame(110.0, 80.0, 100.0, 20.0),
        label: None,
        expected_revision: None,
    };
    let refused = error(service.handle(&place(&request, text(&observed["revision"]))));
    assert_eq!(refused["kind"], "conflict");
    // The desktop's placement (`DataProvider::place_layout_field`) carries
    // the table its field was chosen from, Tasks. Read with the stale
    // mapping, it reaches the host, which reads the occurrence's current
    // table and refuses it rather than placing Notes's field of that id.
    let desktop = |member: &OwnedNode| om_data::layout_placement::LayoutFieldPlacement {
        layout: 1,
        table: fixture::TABLE,
        placement: om_data::layout_placement::Placement {
            occurrence: fixture::OCCURRENCE,
            field: fixture::NAME,
            frame: om_layout::Rect {
                left: 110.0,
                top: 80.0,
                right: 210.0,
                bottom: 100.0,
                ..Default::default()
            },
            label: None,
        },
        expected: catalog_revision(member),
    };
    let mut window = provider(&host);
    let member = window.layout_member(1).unwrap().unwrap();
    let held = window.place_layout_field(desktop(&member)).unwrap_err();
    assert_eq!(held.code, Some(i32::from(status::CONFLICT)), "{held:?}");
    window.close();
    host.stop();
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
    let reopened = common::host(&file, "OpenMaker Server test restart");
    // With the current mapping the provider refuses it before sending.
    let mut window = provider(&reopened);
    let member = window.layout_member(1).unwrap().unwrap();
    let refused = window.place_layout_field(desktop(&member)).unwrap_err();
    assert_eq!(
        refused.code,
        Some(om_data::layout_placement::CONFLICT),
        "{refused:?}"
    );
    assert!(
        refused.message.contains("no longer shows the table"),
        "{refused:?}"
    );
    window.close();
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let actual = get_layout(&mut service);
    assert_eq!(actual["table"], "table:32770@Tasks.fmp12");
    assert_eq!(actual["objects"].as_array().unwrap().len(), 3);
    reopened.stop();
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
}

#[test]
fn a_retry_of_a_placement_rechecks_the_owning_table_on_the_host() {
    use om_data::layout_placement::{self, Placement};
    use om_layout::Rect;
    use om_protocol::authoring::PlaceField;
    use om_provider::AuthoringError;

    let before = form_tree();
    let old_schema = before.child(&[3]).unwrap().to_wire().unwrap();
    let base_member = before.at(&[&[4], &[5], &[1]]).unwrap();
    let expected = catalog_revision(base_member);
    let placement = Placement {
        occurrence: fixture::OCCURRENCE,
        field: fixture::NAME,
        frame: Rect {
            left: 110.0,
            top: 80.0,
            right: 210.0,
            bottom: 100.0,
            ..Default::default()
        },
        label: None,
    };
    let planned = layout_placement::plan(base_member, &placement).unwrap();
    assert!(layout_placement::already_placed(&planned.member, &placement).is_some());
    assert_ne!(catalog_revision(&planned.member), expected);
    let object_key = planned.field_object;
    let mut current = before.clone();
    current.put(&[&[4], &[5], &[1]], planned.member);
    fixture::add_occurrence(&mut current, fixture::OCCURRENCE, "Tasks", fixture::NOTES);
    let file = TempFile::from_tree("placement-retry-retarget", &current);
    let bytes = std::fs::read(&file.0).unwrap();
    let host = host_configured(&file, "OpenMaker Server test", |server| {
        server.overriding_download_slots(&[&[3]], old_schema)
    });
    let mut reader = provider(&host);
    let direct = reader.place_field(PlaceField {
        layout: 1,
        expected,
        table: fixture::TABLE,
        occurrence: fixture::OCCURRENCE,
        field: fixture::NAME,
        frame: [80.0, 110.0, 100.0, 210.0],
        label: None,
    });
    assert!(matches!(
        direct,
        Err(AuthoringError::Refused { status: 306, .. })
    ));
    reader.close();
    let revision_text = format!(
        "sha256:{}",
        expected
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let response = error(service.handle(&Command::PlaceField(PlaceFieldParams {
        layout: Some("layout:1@Tasks.fmp12".into()),
        layout_name: None,
        field: Some("field:32769.1@Tasks.fmp12".into()),
        field_name: None,
        frame: FrameParams {
            left: 110.0,
            top: 80.0,
            width: 100.0,
            height: 20.0,
        },
        label: None,
        expected_revision: revision_text,
    })));
    assert_eq!(response["kind"], "conflict");
    drop(service);
    host.stop();
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
    let reopened = common::host(&file, "OpenMaker Server test restart");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let actual = ok(service.handle(&Command::GetLayout(LayoutParams {
        layout: Some("layout:1@Tasks.fmp12".into()),
        layout_name: None,
    })));
    let object = actual["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["key"] == object_key)
        .unwrap();
    assert_eq!(actual["table"], "table:32770@Tasks.fmp12");
    assert_eq!(object["field"]["ref"], "field:32770.1@Tasks.fmp12");
    assert_eq!(object["field"]["name"], "Body");
    assert_eq!(actual["objects"].as_array().unwrap().len(), 4);
    drop(service);
    reopened.stop();
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
}
