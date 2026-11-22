//! The `om-author` binary: its CLI and its MCP server over stdio, both
//! reaching the same service and host as the library. MCP is spoken as a
//! client of each implemented revision would: 2026-07-28 with
//! `server/discover` and per-request metadata, 2025-11-25 with
//! `initialize`.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::*;
use serde_json::{json, Value};

/// Scratch directory, removed when dropped.
struct Secrets {
    dir: PathBuf,
}

impl Secrets {
    fn new(tag: &str, host: &Host) -> Self {
        let secrets = Self::scratch(tag);
        std::fs::write(secrets.dir.join("ca.pem"), &host.ca_pem).unwrap();
        std::fs::write(secrets.dir.join("password"), format!("{PASSWORD}\n")).unwrap();
        secrets
    }

    fn scratch(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("om-author-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Secrets { dir }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_om-author"));
        for (variable, _) in
            std::env::vars_os().filter(|(v, _)| v.to_string_lossy().starts_with("OPENMAKER_"))
        {
            command.env_remove(variable);
        }
        command
            .env("HOME", &self.dir)
            .env("XDG_CONFIG_HOME", &self.dir)
            .env("APPDATA", &self.dir);
        command
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn om_author(host: &Host, secrets: &Secrets) -> Command {
    let mut command = secrets.command();
    command
        .env("OPENMAKER_HOST", "127.0.0.1")
        .env("OPENMAKER_PORT", host.port.to_string())
        .env("OPENMAKER_FILE", FILE)
        .env("OPENMAKER_ACCOUNT", ACCOUNT)
        .env("OPENMAKER_PASSWORD_FILE", secrets.dir.join("password"))
        .env("OPENMAKER_CA_CERT", secrets.dir.join("ca.pem"));
    command
}

#[test]
fn cli_and_mcp_resolve_user_config_and_send_the_selected_license() {
    let file = TempFile::from_tree("author-config", &form_tree());
    let scratch = Secrets::scratch("user-config");
    let records = scratch.dir.join("recordings");
    let host = host_recording(&file, &records);
    let config = scratch.command().arg("config-path").output().unwrap();
    assert!(config.status.success());
    let config = PathBuf::from(std::str::from_utf8(&config.stdout).unwrap().trim_end());
    assert!(config.starts_with(&scratch.dir));
    assert_eq!(config.file_name().unwrap(), "author.json");
    let directory = config.parent().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("ca.pem"), &host.ca_pem).unwrap();
    std::fs::write(directory.join("password"), format!("{PASSWORD}\n")).unwrap();
    std::fs::write(
        directory.join("license"),
        " synthetic-license-from-file \r\n",
    )
    .unwrap();
    let mut profile = json!({
        "host": "127.0.0.1", "port": host.port, "file": FILE, "account": ACCOUNT,
        "password_file": "password", "ca_cert": "ca.pem", "license_file": "license",
    });
    std::fs::write(&config, profile.to_string()).unwrap();

    let legacy = Secrets::new("legacy-license-file", &host);
    let license_path = legacy.dir.join("license");
    std::fs::write(
        &license_path,
        " synthetic-license-from-env-file \r\nignored\n",
    )
    .unwrap();
    let output = om_author(&host, &legacy)
        .env("OPENMAKER_LICENSE_FILE", &license_path)
        .arg("capabilities")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    ok(serde_json::from_slice(&output.stdout).unwrap());

    let output = scratch
        .command()
        .args(["inspect", "--include", "tables"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    ok(serde_json::from_slice(&output.stdout).unwrap());
    for stream in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(stream).contains("synthetic-license"));
    }

    let env_override = scratch
        .command()
        .env("OPENMAKER_LICENSE_FILE", &license_path)
        .env("OPENMAKER_LICENSE_KEY", "synthetic-license-from-env")
        .arg("capabilities")
        .output()
        .unwrap();
    assert!(env_override.status.success(), "{env_override:?}");

    let mut command = scratch.command();
    command
        .arg("--config")
        .arg(&config)
        .env("OPENMAKER_AUTHOR_CONFIG", scratch.dir.join("missing.json"))
        .env("OPENMAKER_FILE", "Wrong.fmp12")
        .env("OPENMAKER_PORT", "1")
        .env("OPENMAKER_PASSWORD", "synthetic-password-for-another-file")
        .env("OPENMAKER_CA_CERT", scratch.dir.join("missing.pem"))
        .env(
            "OPENMAKER_CLIENT_CERT",
            scratch.dir.join("missing-client.pem"),
        )
        .env(
            "OPENMAKER_CLIENT_KEY",
            scratch.dir.join("missing-client-key.pem"),
        )
        .env(
            "OPENMAKER_LICENSE_KEY",
            "synthetic-license-for-another-file",
        )
        .env(
            "OPENMAKER_LICENSE_FILE",
            scratch.dir.join("missing-license"),
        );
    let mut mcp = Mcp::from_command(command);
    let call = mcp.request(
        1,
        "tools/call",
        modern(json!({
            "name": "openmaker_inspect", "arguments": { "include": ["tables"] },
        })),
    );
    ok(tool_result(&call));
    drop(mcp);

    // A profile without a saved license must still ignore a license file
    // inherited from a different endpoint.
    profile.as_object_mut().unwrap().remove("license_file");
    let unlicensed_profile = directory.join("unlicensed.json");
    std::fs::write(&unlicensed_profile, profile.to_string()).unwrap();
    let unlicensed = scratch
        .command()
        .arg("--config")
        .arg(&unlicensed_profile)
        .env("OPENMAKER_PORT", "1")
        .env("OPENMAKER_LICENSE_FILE", &license_path)
        .arg("capabilities")
        .output()
        .unwrap();
    assert!(unlicensed.status.success(), "{unlicensed:?}");
    ok(serde_json::from_slice(&unlicensed.stdout).unwrap());

    let override_host = scratch
        .command()
        .args(["--port", "1", "inspect"])
        .output()
        .unwrap();
    assert_eq!(override_host.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&override_host.stderr)
        .contains("belongs to a different connection"));
    let too_long = "q".repeat(256);
    let refused = scratch
        .command()
        .env("OPENMAKER_LICENSE_KEY", &too_long)
        .arg("inspect")
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("255 encoded bytes"));
    assert!(!error.contains(&too_long));

    host.stop();
    for (index, expected) in [
        "synthetic-license-from-env-file",
        "synthetic-license-from-file",
        "synthetic-license-from-env",
        "synthetic-license-from-file",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        let mut stream =
            std::fs::File::open(records.join(format!("conn-{index:03}/c2s.bin"))).unwrap();
        let mut reader = om_giop::MessageReader::new(64 * 1024);
        reader.read_logical(&mut stream).unwrap(); // _is_a
        let connect = reader.read_logical(&mut stream).unwrap();
        assert_eq!(
            om_protocol::parse_connect_request(&connect)
                .unwrap()
                .machine
                .install_code,
            expected
        );
    }
}

#[test]
fn the_cli_prints_one_json_response_and_takes_no_password_argument() {
    let file = TempFile::from_tree("author-cli", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let secrets = Secrets::new("cli", &host);
    let output = om_author(&host, &secrets)
        .args(["get-script", "--script-name", "Greeting"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let result = ok(response);
    assert_eq!(result["script"]["ref"], "script:5@Tasks.fmp12");
    for stream in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(stream).contains(PASSWORD));
    }

    // A script run from the command line, waited for.
    let ran = om_author(&host, &secrets)
        .args([
            "run-script",
            "--script-name",
            "Greeting",
            "--parameter",
            "x",
        ])
        .output()
        .unwrap();
    assert!(ran.status.success(), "{ran:?}");
    let ran = ok(serde_json::from_slice(&ran.stdout).unwrap());
    assert_eq!(ran["result"], "hello", "{ran:#}");
    assert_eq!(ran["last_error"], 0);

    // A placement planned from the command line: frames as numbers.
    let planned = om_author(&host, &secrets)
        .args([
            "plan-placement",
            "--layout-name",
            "Tasks",
            "--field-name",
            "Count",
            "--frame",
            "110,80,120,20",
            "--label",
            "Count",
            "--label-frame",
            "20,80,80,20",
        ])
        .output()
        .unwrap();
    assert!(planned.status.success(), "{planned:?}");
    let planned = ok(serde_json::from_slice(&planned.stdout).unwrap());
    assert_eq!(planned["objects"][1]["text"], "Count");
    let bad = om_author(&host, &secrets)
        .args([
            "plan-placement",
            "--layout-name",
            "Tasks",
            "--frame",
            "1,2,3",
        ])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));

    // Names completed against the file's names (printed for --nocapture
    // runs). The replaced range counts characters, past one outside the
    // BMP, and the field comes before the function of the same name.
    let completed = om_author(&host, &secrets)
        .args([
            "complete-calculation",
            "--text",
            "\"😀\" & Cou ; 1",
            "--cursor",
            "9",
        ])
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&completed.stdout));
    assert!(completed.status.success(), "{completed:?}");
    let completed = ok(serde_json::from_slice(&completed.stdout).unwrap());
    assert_eq!(
        completed["replace"],
        json!({"start": 6, "end": 9}),
        "{completed:#}"
    );
    let candidates = &completed["candidates"];
    assert_eq!(candidates[0]["insert"], "Tasks::Count", "{completed:#}");
    assert_eq!(candidates[0]["field"], "field:32769.2@Tasks.fmp12");
    assert_eq!(
        (&candidates[1]["kind"], &candidates[1]["name"]),
        (&json!("function"), &json!("Count"))
    );
    let functions = om_author(&host, &secrets)
        .args(["complete-calculation", "--text", "Le"])
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&functions.stdout));
    assert!(functions.status.success(), "{functions:?}");
    let functions = ok(serde_json::from_slice(&functions.stdout).unwrap());
    assert!(
        functions["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["insert"] == "Length ( text )"),
        "{functions:#}"
    );

    let refused = om_author(&host, &secrets)
        .args(["--password", "guess", "capabilities"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--password-file"));
    host.stop();

    // The command catalog needs no host, account or password.
    let mut bare = Command::new(env!("CARGO_BIN_EXE_om-author"));
    for (variable, _) in std::env::vars().filter(|(v, _)| v.starts_with("OPENMAKER_")) {
        bare.env_remove(variable);
    }
    let listed = bare.arg("commands").output().unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let listed = ok(serde_json::from_slice(&listed.stdout).unwrap());
    let ids: Vec<&str> = listed["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        om_author::catalog::ALL.map(|spec| spec.id),
        "{listed:#}"
    );
}

/// An MCP client on the binary's stdio.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Mcp {
    fn start(host: &Host, secrets: &Secrets) -> Self {
        Self::from_command(om_author(host, secrets))
    }

    fn from_command(mut command: Command) -> Self {
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Mcp {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// The response to a request; every line on stdout must be a JSON-RPC
    /// message.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "stdout closed"
            );
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carried a non-JSON line {line:?}: {e}"));
            assert_eq!(message["jsonrpc"], "2.0", "{message}");
            if message["id"] == json!(id) {
                return message;
            }
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A 2026-07-28 request's parameters: `params` with the per-request
/// metadata.
fn modern(mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "om-author-test", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    params
}

/// A tool call's structured response.
fn tool_result(response: &Value) -> Value {
    let result = &response["result"];
    assert!(result.is_object(), "{response:#}");
    result["structuredContent"].clone()
}

#[test]
fn mcp_discovers_commands_before_connection_settings_are_available() {
    let scratch = Secrets::scratch("mcp-without-config");
    let command = scratch.command();
    let mut mcp = Mcp::from_command(command);
    let discovered = mcp.request(1, "server/discover", modern(json!({})));
    assert!(discovered["result"]["capabilities"]["tools"].is_object());
    let listed = mcp.request(2, "tools/list", modern(json!({})));
    assert!(listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "openmaker_commands"));

    let call = |name: &str| modern(json!({ "name": name, "arguments": {} }));
    let catalog = ok(tool_result(&mcp.request(
        3,
        "tools/call",
        call("openmaker_commands"),
    )));
    assert_eq!(
        catalog["commands"].as_array().unwrap().len(),
        om_author::catalog::ALL.len()
    );

    let refused = mcp.request(4, "tools/call", call("openmaker_inspect"));
    assert_eq!(refused["result"]["isError"], true, "{refused:#}");
    let error = tool_result(&refused);
    assert_eq!(error["error"]["kind"], "invalid_request");
    assert!(text(&error["error"]["message"]).contains("No host"));

    // A file command's configuration error does not end discovery.
    ok(tool_result(&mcp.request(
        5,
        "tools/call",
        call("openmaker_commands"),
    )));
}

#[test]
fn mcp_2026_07_28_discovers_lists_and_applies_through_the_same_service() {
    let file = TempFile::from_tree("author-mcp", &form_tree());
    let host = host(&file, "OpenMaker Server test");
    let secrets = Secrets::new("mcp", &host);
    let mut mcp = Mcp::start(&host, &secrets);

    let discovered = mcp.request(1, "server/discover", modern(json!({})));
    let versions = &discovered["result"]["supportedVersions"];
    assert_eq!(
        versions,
        &json!(["2025-11-25", "2026-07-28"]),
        "{discovered:#}"
    );
    assert!(discovered["result"]["capabilities"]["tools"].is_object());

    let listed = mcp.request(2, "tools/list", modern(json!({})));
    let tools: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        [
            "openmaker_capabilities",
            "openmaker_inspect",
            "openmaker_get_script",
            "openmaker_validate_script",
            "openmaker_plan_script_edit",
            "openmaker_apply_script_edit",
            "openmaker_plan_field",
            "openmaker_create_field",
            "openmaker_get_layout",
            "openmaker_plan_field_placement",
            "openmaker_place_field",
            "openmaker_set_object_frames",
            "openmaker_arrange_objects",
            "openmaker_delete_objects",
            "openmaker_place_text",
            "openmaker_place_portal",
            "openmaker_create_layout",
            "openmaker_duplicate_layout",
            "openmaker_delete_layout",
            "openmaker_layout_setup",
            "openmaker_set_layout_width",
            "openmaker_set_field_control",
            "openmaker_set_part_options",
            "openmaker_change_parts",
            "openmaker_set_text",
            "openmaker_run_script",
            "openmaker_list_choices",
            "openmaker_choose_value_list_item",
            "openmaker_enter_other_value",
            "openmaker_get_value_list_values",
            "openmaker_set_value_list_values",
            "openmaker_get_security",
            "openmaker_get_value_lists",
            "openmaker_complete_calculation",
            "openmaker_create_file",
            "openmaker_commands",
        ]
    );
    // A run may change anything and repeats its effects: never read-only,
    // never to be retried by the client on its own.
    let run_tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "openmaker_run_script")
        .unwrap();
    assert_eq!(
        run_tool["annotations"]["readOnlyHint"], false,
        "{run_tool:#}"
    );
    assert_eq!(run_tool["annotations"]["destructiveHint"], true);
    assert_eq!(run_tool["annotations"]["idempotentHint"], false);

    let call =
        |name: &str, arguments: Value| modern(json!({ "name": name, "arguments": arguments }));
    let got = tool_result(&mcp.request(
        3,
        "tools/call",
        call(
            "openmaker_get_script",
            json!({ "script": "script:5@Tasks.fmp12" }),
        ),
    ));
    let got = ok(got);
    let revision = text(&got["revision"]).to_owned();
    let source = text(&got["source"]).replace("\"hello\"", "\"hi, \" & Get ( ScriptParameter )");

    // Arguments outside the schema are the tool's error, for the caller.
    let bad = mcp.request(
        4,
        "tools/call",
        call(
            "openmaker_apply_script_edit",
            json!({ "script": "script:5@Tasks.fmp12" }),
        ),
    );
    assert_eq!(bad["result"]["isError"], true, "{bad:#}");
    assert_eq!(tool_result(&bad)["error"]["kind"], "invalid_request");

    let applied = mcp.request(
        5,
        "tools/call",
        call(
            "openmaker_apply_script_edit",
            json!({
                "script": "script:5@Tasks.fmp12",
                "source": source,
                "expected_revision": revision,
            }),
        ),
    );
    let applied = ok(tool_result(&applied));
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["verified"]["fresh_connection"], true);

    // The library sees what the MCP tool wrote.
    let mut service = om_author::Service::new(target(&host, ACCOUNT, PASSWORD));
    let read_back = ok(service.handle(&om_author::Command::GetScript(
        om_author::api::ScriptParams {
            script: Some("script:5@Tasks.fmp12".into()),
            script_name: None,
        },
    )));
    assert_eq!(read_back["revision"], applied["revision"]);
    assert!(text(&read_back["source"]).contains("Get ( ScriptParameter )"));

    // The edited script runs on the host, at the revision just applied, and
    // its result comes back: the edit, apply, run loop.
    let ran = ok(tool_result(&mcp.request(
        20,
        "tools/call",
        call(
            "openmaker_run_script",
            json!({
                "script": "script:5@Tasks.fmp12",
                "parameter": "agent",
                "expected_revision": applied["revision"],
            }),
        ),
    )));
    assert_eq!(ran["status"], "completed", "{ran:#}");
    assert_eq!(ran["result"], "hi, agent");
    assert_eq!(ran["last_error"], 0);
    assert_eq!(ran["revision"], applied["revision"]);
    // From the revision before the edit, nothing runs.
    let stale = mcp.request(
        21,
        "tools/call",
        call(
            "openmaker_run_script",
            json!({ "script": "script:5@Tasks.fmp12", "expected_revision": revision }),
        ),
    );
    assert_eq!(stale["result"]["isError"], true, "{stale:#}");
    assert_eq!(tool_result(&stale)["error"]["kind"], "conflict");

    // A field, created with the revision inspect gives.
    let inspected = ok(tool_result(&mcp.request(
        6,
        "tools/call",
        call("openmaker_inspect", json!({ "include": ["tables"] })),
    )));
    let notes = inspected["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Notes")
        .unwrap();
    let created = ok(tool_result(&mcp.request(
        7,
        "tools/call",
        call(
            "openmaker_create_field",
            json!({
                "table_name": "Notes",
                "name": "Mood",
                "type": "text",
                "expected_revision": notes["fields_revision"],
            }),
        ),
    )));
    assert_eq!(created["status"], "created", "{created:#}");
    assert_eq!(created["verified"]["fresh_connection"], true);
    let listed = ok(service.handle(&om_author::Command::Inspect(
        om_author::api::InspectParams::default(),
    )));
    assert!(listed["tables"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "Notes" && t["fields_revision"] == created["revision"]));

    // A field placed on the Form layout with the revision get_layout gives.
    let layout = ok(tool_result(&mcp.request(
        8,
        "tools/call",
        call("openmaker_get_layout", json!({ "layout_name": "Tasks" })),
    )));
    let placed = ok(tool_result(&mcp.request(
        9,
        "tools/call",
        call(
            "openmaker_place_field",
            json!({
                "layout": "layout:1@Tasks.fmp12",
                "field": "field:32769.2@Tasks.fmp12",
                "frame": { "left": 110, "top": 80, "width": 120, "height": 20 },
                "expected_revision": layout["revision"],
            }),
        ),
    )));
    assert_eq!(placed["status"], "placed", "{placed:#}");
    assert_eq!(placed["objects"][0]["ref"], "layout_object:1.4@Tasks.fmp12");
    assert_eq!(placed["verified"]["fresh_connection"], true);
    let read_back = ok(service.handle(&om_author::Command::GetLayout(
        om_author::api::LayoutParams {
            layout: Some("layout:1@Tasks.fmp12".into()),
            layout_name: None,
        },
    )));
    assert_eq!(read_back["revision"], placed["revision"]);
    drop(mcp);
    host.stop();
}

#[test]
fn mcp_2025_11_25_initializes_and_calls_tools() {
    let file = TempFile::new("author-mcp-init", &scripts(), true);
    let host = host(&file, "OpenMaker Server test");
    let secrets = Secrets::new("mcp-init", &host);
    let mut mcp = Mcp::start(&host, &secrets);
    let initialized = mcp.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "om-author-test", "version": "0" },
        }),
    );
    assert_eq!(
        initialized["result"]["protocolVersion"], "2025-11-25",
        "{initialized:#}"
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "om-author");
    mcp.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let capabilities = mcp.request(
        2,
        "tools/call",
        json!({ "name": "openmaker_capabilities", "arguments": {} }),
    );
    let capabilities = ok(tool_result(&capabilities));
    assert_eq!(
        capabilities["operations"]["apply_script_steps"]["available"],
        true
    );
    // A revision this server does not implement is refused, not guessed.
    let mut old = Mcp::start(&host, &secrets);
    let refused = old.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "om-author-test", "version": "0" },
        }),
    );
    assert_ne!(
        refused["result"]["protocolVersion"], "2024-11-05",
        "{refused:#}"
    );
    drop(old);
    drop(mcp);
    host.stop();
}
