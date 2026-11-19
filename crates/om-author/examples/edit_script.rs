//! A script edit from start to finish, on a synthetic file: an OpenMaker
//! Server in this process hosts a copy of the Tasks test file (built in
//! memory, no customer data), and the authoring service gets a script as
//! text, edits it, plans the edit, applies it, reads it back and runs it on
//! the server at the revision it applied. Prints each JSON response.
//!
//! `cargo run -p om-author --example edit_script`

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;

use om_author::api::{ApplyParams, EditParams, RunScriptParams, ScriptParams};
use om_author::{Command, Service, Target};
use om_protocol::MachineInfo;
use om_provider::NativeConfig;
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};
use serde_json::Value;
use zeroize::Zeroizing;

fn show(response: &Value) {
    println!("{}", serde_json::to_string_pretty(response).unwrap());
    assert_eq!(response["ok"], true, "the command failed");
}

fn main() {
    // A file with one script, "Greeting", in a temporary directory.
    let greeting = fixture::script(
        5,
        "Greeting",
        vec![
            fixture::set_variable("$who", "\"world\""),
            fixture::exit_with("\"hello\""),
        ],
    );
    let file = fixture::TempFile::new("example", &[greeting], true);

    // OpenMaker Server on loopback, with a certificate the client trusts.
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca_pem = generated.cert.pem().into_bytes();
    let identity =
        Identity::from_pem(&ca_pem, generated.signing_key.serialize_pem().as_bytes()).unwrap();
    let server = Server::bind(
        "127.0.0.1:0",
        ServerConfig {
            identity: ServerIdentity {
                name: "example".into(),
                version_string: "OpenMaker Server example".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: vec![HostedFile {
                name: fixture::FILE.into(),
                path: file.0.clone(),
            }],
            insecure_files: false,
            record_dir: None,
        },
        identity,
    )
    .unwrap();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());

    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut service = Service::new(Target::new(
        "127.0.0.1",
        port,
        fixture::FILE,
        fixture::ACCOUNT,
        Zeroizing::new(fixture::PASSWORD.into()),
        NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.signing_key.serialize_pem().into_bytes(),
            additional_ca_pem: ca_pem,
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        },
    ));

    let got = service.handle(&Command::GetScript(ScriptParams {
        script: None,
        script_name: Some("Greeting".into()),
    }));
    show(&got);
    let script = got["result"]["script"]["ref"].as_str().unwrap().to_owned();
    let revision = got["result"]["revision"].as_str().unwrap().to_owned();
    let source = got["result"]["source"].as_str().unwrap();

    // The edit: a comment first, and a result that uses the variable.
    let edited = format!(
        "# greet the caller\n{}",
        source.replace("\"hello\"", "\"hello, \" & $who")
    );

    let plan = service.handle(&Command::Plan(EditParams {
        script: Some(script.clone()),
        script_name: None,
        source: edited.clone(),
        expected_revision: Some(revision.clone()),
    }));
    show(&plan);
    let after = plan["result"]["after_revision"]
        .as_str()
        .unwrap()
        .to_owned();

    let applied = service.handle(&Command::Apply(ApplyParams {
        script: Some(script.clone()),
        script_name: None,
        source: edited,
        expected_revision: revision,
        expected_after_revision: Some(after),
    }));
    show(&applied);

    show(&service.handle(&Command::GetScript(ScriptParams {
        script: Some(script.clone()),
        script_name: None,
    })));

    // Run what was applied, and see its result.
    let ran = service.handle(&Command::RunScript(RunScriptParams {
        script: Some(script),
        script_name: None,
        parameter: None,
        expected_revision: applied["result"]["revision"].as_str().map(str::to_owned),
    }));
    show(&ran);
    assert_eq!(ran["result"]["result"], "hello, world");
    assert_eq!(ran["result"]["last_error"], 0);
}
