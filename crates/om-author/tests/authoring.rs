//! The authoring service end to end against an OpenMaker Server on
//! loopback hosting a synthetic file: inspect, get a script, edit its
//! text, validate, plan, apply, read it back after the server restarts
//! and run it; and the edits the host and service refuse, which leave the
//! file as it was.

mod common;

use common::*;
use om_author::api::{ApplyParams, EditParams, InspectParams, ScriptParams};
use om_author::{Command, Service};
use om_data::runtime::ServerScriptCall;
use om_data::DataProvider;
use om_hbam::serial;
use om_protocol::authoring::{self, content_revision, status, Reply, Request};
use om_scripts::Script;
use serde_json::Value;

const GREETING_REF: &str = "script:5@Tasks.fmp12";

fn get(service: &mut Service, script: &str) -> Value {
    ok(service.handle(&Command::GetScript(ScriptParams {
        script: Some(script.into()),
        script_name: None,
    })))
}

fn edit(source: &str, expected: Option<&str>) -> EditParams {
    EditParams {
        script: Some(GREETING_REF.into()),
        script_name: None,
        source: source.into(),
        expected_revision: expected.map(str::to_owned),
    }
}

fn apply(source: &str, expected: &str) -> Command {
    Command::Apply(ApplyParams {
        script: Some(GREETING_REF.into()),
        script_name: None,
        source: source.into(),
        expected_revision: expected.into(),
        expected_after_revision: None,
    })
}

/// The edit the tests make: a comment first, and a new result.
fn edited(source: &str) -> String {
    assert!(source.contains("\"hello\""), "{source}");
    format!(
        "# greet the caller\n{}",
        source.replace("\"hello\"", "\"hello, \" & $who")
    )
}

#[test]
fn a_script_edit_is_planned_applied_read_back_and_run() {
    let file = TempFile::new("author-edit", &scripts(), true);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));

    let capabilities = ok(service.handle(&Command::Capabilities));
    assert_eq!(capabilities["host"]["product"], "OpenMaker Server test");
    assert_eq!(
        capabilities["operations"]["apply_script_steps"]["available"],
        true
    );
    assert_eq!(
        capabilities["operations"]["create_script"]["available"],
        false
    );

    // Metadata only, with file-qualified references.
    let inspect = ok(service.handle(&Command::Inspect(InspectParams::default())));
    let scripts = inspect["scripts"].as_array().unwrap();
    let listed = scripts
        .iter()
        .find(|s| s["ref"] == GREETING_REF)
        .expect("Greeting listed");
    assert_eq!(listed["name"], "Greeting");
    assert_eq!(listed["in_menu"], false);
    assert_eq!(listed["full_access"], true);
    assert!(inspect["tables"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["ref"] == "table:32769@Tasks.fmp12" && t["name"] == "Tasks"));
    assert!(inspect["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["name"] == "Tasks" && l["occurrence"] == "occurrence:13631489@Tasks.fmp12"));
    let listed = inspect.to_string();
    for record_value in ["Write tests", "Ship", "Hi"] {
        assert!(!listed.contains(record_value), "{record_value} read");
    }

    let got = get(&mut service, GREETING_REF);
    let source = text(&got["source"]).to_owned();
    let revision = text(&got["revision"]).to_owned();
    assert_eq!(got["source_format"], "openmaker.steps-text/v1");
    assert_eq!(got["steps"].as_array().unwrap().len(), 2);
    assert_eq!(got["opaque"]["unreferenced_step_data"], 1);
    assert_eq!(got["apply"]["available"], true);

    let new_source = edited(&source);
    let validated = ok(service.handle(&Command::Validate(edit(&new_source, None))));
    assert_eq!(validated["valid"], true, "{validated:#}");
    assert_eq!(validated["steps"]["kept"], 1);
    assert_eq!(validated["steps"]["new"], 2);
    assert_eq!(validated["steps"]["deleted"], 1);

    let plan = ok(service.handle(&Command::Plan(edit(&new_source, Some(&revision)))));
    assert_eq!(plan["changed"], true);
    assert_eq!(plan["base_revision"], revision.as_str());
    let after = text(&plan["after_revision"]).to_owned();
    let diff = text(&plan["diff"]);
    assert!(diff.contains("+# greet the caller"), "{diff}");
    assert!(
        diff.contains("\"hello\"") && diff.contains("& $who"),
        "{diff}"
    );
    // A plan writes nothing.
    assert_eq!(
        get(&mut service, GREETING_REF)["revision"],
        revision.as_str()
    );

    let applied = ok(service.handle(&Command::Apply(ApplyParams {
        script: Some(GREETING_REF.into()),
        script_name: None,
        source: new_source.clone(),
        expected_revision: revision.clone(),
        expected_after_revision: Some(after.clone()),
    })));
    assert_eq!(applied["status"], "applied", "{applied:#}");
    assert_eq!(applied["revision"], after.as_str());
    assert_eq!(applied["verified"]["fresh_connection"], true);
    assert_eq!(applied["verified"]["revision"], after.as_str());

    // The same edit again, from the old revision: already there, so
    // nothing is written (an answer lost in transit is safe to check).
    let again = ok(service.handle(&apply(&new_source, &revision)));
    assert_eq!(again["status"], "already_applied");

    // The saved file, opened by a new server.
    host.stop();
    let reopened = common::host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&reopened, ACCOUNT, PASSWORD));
    let read_back = get(&mut service, GREETING_REF);
    assert_eq!(read_back["revision"], after.as_str());
    assert_eq!(read_back["source"], plan["after_source"]);
    assert_eq!(read_back["script"]["in_menu"], false);
    assert_eq!(read_back["script"]["full_access"], true);

    // Byte for byte: only the step list, step data and stamp changed; the
    // kept step is as stored, item UUID and all.
    let mut provider = provider(&reopened);
    let stored = provider.script_definitions().unwrap();
    let definition = &stored.definitions[&GREETING];
    let original = greeting().encode();
    for child in &original.children {
        if !matches!(child.segment.as_slice(), [4] | [5]) && !authoring::is_stamp(&child.segment) {
            assert_eq!(definition.child(&child.segment), Some(child));
        }
    }
    assert_eq!(content_revision(definition), {
        let mut expected = [0; 32];
        let hex = after.strip_prefix("sha256:").unwrap();
        for (i, b) in expected.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        expected
    });
    let script: &Script = stored.catalog.script(GREETING).unwrap();
    assert_eq!(script.steps[1], greeting().steps[0]);
    assert_eq!(script.unreferenced_data, greeting().unreferenced_data);
    assert_eq!(
        definition
            .child(authoring::MOD_COUNT)
            .unwrap()
            .value
            .as_deref(),
        Some(&[1, 1][..])
    );
    assert_eq!(
        definition
            .child(authoring::MOD_ACCOUNT)
            .map(|n| om_scripts::node_text(n).unwrap()),
        Some(ACCOUNT.to_owned())
    );

    // And it runs: on the server, as the desktop client asks it to.
    let result = provider
        .perform_script_on_server(&ServerScriptCall {
            file: 0,
            script: GREETING,
            parameter: None,
            wait: true,
        })
        .unwrap();
    assert!(!result.error.is_error(), "{result:?}");
    let value = result.result.expect("a waited-for result");
    let locale = om_data::calc::Locale::en_us();
    let ctx = om_data::calc::Ctx::new(&locale, om_data::calc::Date::from_ymd(2026, 11, 2).unwrap());
    assert_eq!(value.to_text(&ctx).into_string(), "hello, world");
    provider.close();
    reopened.stop();
}

#[test]
fn refused_edits_leave_the_file_as_it_was() {
    let file = TempFile::new("author-refusals", &scripts(), true);
    let host = host(&file, "OpenMaker Server test");
    let on_disk = std::fs::read(&file.0).unwrap();
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let got = get(&mut service, GREETING_REF);
    let source = text(&got["source"]).to_owned();
    let revision = text(&got["revision"]).to_owned();
    let new_source = edited(&source);

    // A revision the script is not at.
    let stale = format!("sha256:{}", "00".repeat(32));
    let conflict = error(service.handle(&apply(&new_source, &stale)));
    assert_eq!(conflict["kind"], "conflict");
    assert_eq!(conflict["details"]["current_revision"], revision.as_str());

    // Text that does not read, with the line it is on.
    let unreadable = format!("{new_source}Frobnicate [ 1 ]\n");
    let invalid = error(service.handle(&apply(&unreadable, &revision)));
    assert_eq!(invalid["kind"], "invalid_source");
    let diagnostic = &invalid["details"]["diagnostics"][0];
    assert_eq!(diagnostic["line"], 4, "{invalid:#}");
    assert_eq!(diagnostic["text"], "Frobnicate [ 1 ]");

    // Steps that do not nest.
    let unbalanced = format!("If [ 1 ]\n{new_source}");
    let invalid = error(service.handle(&apply(&unbalanced, &revision)));
    assert_eq!(invalid["kind"], "invalid_source");
    assert!(text(&invalid["details"]["diagnostics"][0]["message"]).contains("If"));

    // References: another file's id is never taken as this file's, and a
    // shared name is not resolved to either script.
    let wrong = error(service.handle(&Command::GetScript(ScriptParams {
        script: Some("script:5@Other.fmp12".into()),
        script_name: None,
    })));
    assert_eq!(wrong["kind"], "wrong_file");
    let twins = error(service.handle(&Command::GetScript(ScriptParams {
        script: None,
        script_name: Some("Twin".into()),
    })));
    assert_eq!(twins["kind"], "ambiguous_reference");
    assert_eq!(twins["details"]["candidates"].as_array().unwrap().len(), 2);

    // Another user has the script open, as FileMaker Pro's Script
    // Workspace holds it.
    let (mut other, _) = session(&host, "Other desk", ACCOUNT, PASSWORD);
    let key = serial::encode(GREETING);
    let lock = other.lock(&[&[17], &[5], &key], 77, true).unwrap();
    assert!(matches!(
        lock,
        om_protocol::write::LockReply::Granted { .. }
    ));
    let locked = error(service.handle(&apply(&new_source, &revision)));
    assert_eq!(locked["kind"], "locked", "{locked:#}");
    assert!(text(&locked["message"]).contains("Other desk"));
    other.kill_all_locks(77).unwrap();

    // An account the server does not know to have [Full Access]: refused
    // by the service, and by the host when a client skips the check.
    let mut clerk = Service::new(target(&host, CLERK, CLERK_PASSWORD));
    let capabilities = ok(clerk.handle(&Command::Capabilities));
    let apply_steps = &capabilities["operations"]["apply_script_steps"];
    assert_eq!(apply_steps["available"], false);
    assert_eq!(apply_steps["kind"], "permission_denied");
    let denied = error(clerk.handle(&apply(&new_source, &revision)));
    assert_eq!(denied["kind"], "permission_denied");
    let (mut session, file_id) = session(&host, "Clerk desk", CLERK, CLERK_PASSWORD);
    let request = Request::ReplaceScriptSteps(authoring::ReplaceScriptSteps {
        script: GREETING,
        expected: [0; 32],
        definition: greeting().encode(),
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

    // Nothing reached the file, on disk or as served.
    assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
    assert_eq!(
        get(&mut service, GREETING_REF)["revision"],
        revision.as_str()
    );
    host.stop();
}

/// A host that does not claim the authoring extension (`_is_a`), as
/// FileMaker Server does not, is only read: nothing is sent to change it.
#[test]
fn hosts_without_the_authoring_extension_are_read_only() {
    let file = TempFile::new("author-foreign", &scripts(), true);
    let host = host_configured(
        &file,
        "OpenMaker Server test",
        om_server::Server::without_authoring,
    );
    let on_disk = std::fs::read(&file.0).unwrap();
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let capabilities = ok(service.handle(&Command::Capabilities));
    let apply_steps = &capabilities["operations"]["apply_script_steps"];
    assert_eq!(apply_steps["available"], false);
    assert_eq!(apply_steps["kind"], "unsupported");
    // Reading works; writing is not attempted.
    let got = get(&mut service, GREETING_REF);
    let revision = text(&got["revision"]).to_owned();
    let new_source = edited(text(&got["source"]));
    let refused = error(service.handle(&apply(&new_source, &revision)));
    assert_eq!(refused["kind"], "unsupported");
    assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
    host.stop();
}

/// A save the host cannot write leaves the script as it was, in the file
/// and as served: the edit is taken back and reported, not kept in memory.
#[cfg(unix)]
#[test]
fn an_edit_the_host_cannot_save_is_taken_back() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("om-author-readonly-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = TempFile(dir.join(FILE));
    fixture::write(&file.0, &scripts(), true);
    let host = host(&file, "OpenMaker Server test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let got = get(&mut service, GREETING_REF);
    let revision = text(&got["revision"]).to_owned();
    let new_source = edited(text(&got["source"]));
    let on_disk = std::fs::read(&file.0).unwrap();
    // The save writes a new file beside the old one, which a read-only
    // directory refuses (unless the tests run as root, which may write).
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = std::fs::write(dir.join("probe"), b"").is_ok();
    let response = service.handle(&apply(&new_source, &revision));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        eprintln!("skipped: the directory stays writable for this user");
    } else {
        let failed = error(response);
        assert_eq!(failed["kind"], "save_failed", "{failed:#}");
        assert_eq!(std::fs::read(&file.0).unwrap(), on_disk);
        assert_eq!(
            get(&mut service, GREETING_REF)["revision"],
            revision.as_str()
        );
    }
    host.stop();
    drop(file);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn normalized_source_retries_keep_the_accepted_revision() {
    let file = TempFile::new("author-normalized-retry", &scripts(), true);
    let host = host(&file, "OpenMaker retry test");
    let mut service = Service::new(target(&host, ACCOUNT, PASSWORD));
    let got = get(&mut service, GREETING_REF);
    let revision = text(&got["revision"]).to_owned();
    let source = "# retry   \n";
    let plan = ok(service.handle(&Command::Plan(edit(source, Some(&revision)))));
    assert_eq!(plan["normalized"], true);
    let request = Command::Apply(ApplyParams {
        script: Some(GREETING_REF.into()),
        script_name: None,
        source: source.into(),
        expected_revision: revision,
        expected_after_revision: Some(text(&plan["after_revision"]).into()),
    });
    let applied = ok(service.handle(&request));
    assert_eq!(applied["status"], "applied");
    let disk = std::fs::read(&file.0).unwrap();
    let retry = ok(service.handle(&request));
    assert_eq!(retry["status"], "already_applied");
    assert_eq!(retry["revision"], applied["revision"]);
    assert_eq!(std::fs::read(&file.0).unwrap(), disk);
    host.stop();
}

#[test]
fn authoring_does_not_require_desktop_layouts_or_tables() {
    for tableless in [false, true] {
        let mut root = fixture::tree(&scripts(), false);
        if tableless {
            root.children.retain(|node| node.segment != [3]);
        }
        let file = TempFile::from_tree(
            if tableless {
                "author-tableless"
            } else {
                "author-layoutless"
            },
            &root,
        );
        let host = host(&file, "OpenMaker metadata test");
        let mut desktop = om_provider::NativeProvider::new(tls(&host));
        let target = target(&host, ACCOUNT, PASSWORD);
        let connection = om_data::Connection {
            host: "127.0.0.1".into(),
            port: host.port,
            file: FILE.into(),
            account: ACCOUNT.into(),
            password: PASSWORD.into(),
        };
        assert!(desktop.open(&connection).is_err());
        let mut service = Service::new(target);
        let caps = ok(service.handle(&Command::Capabilities));
        assert_eq!(caps["operations"]["apply_script_steps"]["available"], true);
        let inspected = ok(service.handle(&Command::Inspect(InspectParams::default())));
        assert!(inspected["layouts"].as_array().unwrap().is_empty());
        if tableless {
            assert!(inspected["tables"].as_array().unwrap().is_empty());
        }
        let got = get(&mut service, GREETING_REF);
        let source = "# works without Browse\n";
        let revision = text(&got["revision"]);
        assert_eq!(
            ok(service.handle(&Command::Plan(edit(source, Some(revision)))))["changed"],
            true
        );
        assert_eq!(
            ok(service.handle(&apply(source, revision)))["status"],
            "applied"
        );
        host.stop();
        let restarted = common::host(&file, "OpenMaker metadata restart");
        let mut service = Service::new(common::target(&restarted, ACCOUNT, PASSWORD));
        assert_eq!(get(&mut service, GREETING_REF)["source"], source);
        restarted.stop();
    }
}

#[test]
fn native_catalog_uploads_cannot_bypass_authoring_permissions() {
    use om_protocol::{write, Resume};
    let file = TempFile::new("author-native-denial", &scripts(), true);
    let host = host(&file, "OpenMaker upload test");
    let original = std::fs::read(&file.0).unwrap();
    let (mut clerk, file_id) = session(&host, "Clerk", CLERK, CLERK_PASSWORD);
    for dest in [
        vec![vec![17]],
        vec![vec![17], vec![5]],
        vec![vec![17], vec![5], serial::encode(GREETING)],
        vec![vec![17], vec![5], serial::encode(GREETING), vec![4]],
    ] {
        let upload = write::Upload {
            payload: Vec::new(),
            resume: Resume::start(),
            terminal: Vec::new(),
            flags: write::UPLOAD_VALUE,
            dest,
        };
        clerk
            .perform(&write::build_upload_to_host(9902, file_id, &upload))
            .unwrap();
        assert_eq!(std::fs::read(&file.0).unwrap(), original);
    }
    clerk.disconnect().unwrap();
    let mut fresh = provider(&host);
    assert_eq!(
        fresh
            .script_definitions()
            .unwrap()
            .catalog
            .script(GREETING)
            .unwrap()
            .steps
            .len(),
        2
    );
    fresh.close();
    host.stop();
    let restarted = common::host(&file, "OpenMaker upload restart");
    let mut fresh = provider(&restarted);
    assert_eq!(
        fresh
            .script_definitions()
            .unwrap()
            .catalog
            .script(GREETING)
            .unwrap()
            .steps
            .len(),
        2
    );
    fresh.close();
    restarted.stop();
    assert_eq!(std::fs::read(&file.0).unwrap(), original);
}

/// The file's security through the command the CLI and MCP run (#228):
/// Admin, the file's own account in its built-in [Full Access] set, reads
/// the accounts, privilege sets and extended privileges as the host decodes
/// them, and nothing on the wire holds an account's password verifier or
/// its record member `04`; Clerk, in [Data Entry Only], is refused.
#[test]
fn security_is_read_without_credentials_by_the_files_full_access_account() {
    let mut root = indexed_tree();
    fixture::add_local_accounts(&mut root);
    // Clerk's [Data Entry Only] whole again, so that the server admits it.
    fixture::put_data_entry_only(&mut root);
    // Admin's record member `04`, which no reply may carry.
    let member_04 = [0xa5; 16];
    root.put(
        &[&[23], &[1], &[5], &[2], &[4]],
        om_hbam::OwnedNode::leaf(Vec::new(), member_04.to_vec()),
    );
    // An extended privilege both sets have.
    root.put(
        &[&[23], &[3], &[5], &[1], &[16]],
        om_scripts::text_node(&[], "fmapp"),
    );
    for set in [1u8, 2] {
        root.put(
            &[&[23], &[3], &[5], &[1], &[5], &[set]],
            om_hbam::OwnedNode::leaf(Vec::new(), Vec::new()),
        );
    }
    let file = TempFile::from_tree("author-security", &root);
    let host = host(&file, "OpenMaker Server test");

    let mut admin = Service::new(target(&host, ACCOUNT, PASSWORD));
    let security = ok(admin.handle(&Command::GetSecurity));
    let names: Vec<&str> = security["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| account["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [ACCOUNT, CLERK, "Former", "Newcomer"]);
    let former = &security["accounts"][2];
    assert_eq!(former["active"], false);
    assert_eq!(former["privilege_set_name"], "[Full Access]");
    assert_eq!(security["accounts"][3]["must_change_password"], true);
    assert_eq!(
        security["accounts"][1]["privilege_set_name"],
        "[Data Entry Only]"
    );
    let full_access = &security["privilege_sets"][0];
    assert_eq!(full_access["name"], "[Full Access]");
    assert_eq!(full_access["record_flags"], "5503");
    assert_eq!(full_access["password_days"], 0);
    assert_eq!(full_access["extended_privileges"][0], "fmapp");
    assert_eq!(security["extended_privileges"][0]["keyword"], "fmapp");
    assert_eq!(
        security["extended_privileges"][0]["privilege_sets"][1]["name"],
        "[Data Entry Only]"
    );
    assert_eq!(security["editable"], false);

    // The reply itself, as any client receives it: no verifier, no `04`.
    let (mut session, file_id) = session(&host, "Admin desk", ACCOUNT, PASSWORD);
    let reply = session
        .perform(&authoring::build_request(9, file_id, &Request::DescribeSecurity).unwrap())
        .unwrap();
    assert!(matches!(
        authoring::parse_reply(&Request::DescribeSecurity, &reply).unwrap(),
        Reply::Security(_)
    ));
    for record in [2u8, 3, 4, 5] {
        let stored = root
            .at(&[&[23], &[1], &[5], &[record], &[6]])
            .and_then(|node| node.value.clone())
            .unwrap();
        // Its salt and its hash, each anywhere in the reply.
        for secret in [&stored[1..11], &stored[11..31]] {
            assert!(!reply.windows(secret.len()).any(|w| w == secret));
        }
    }
    assert!(!reply.windows(16).any(|w| w == member_04));

    let mut clerk = Service::new(target(&host, CLERK, CLERK_PASSWORD));
    let denied = error(clerk.handle(&Command::GetSecurity));
    assert_eq!(denied["kind"], "permission_denied", "{denied:#}");
    host.stop();
}
