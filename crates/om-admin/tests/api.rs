//! The Admin API end to end over HTTPS against the sample backend: tokens,
//! their cap and the lockout shared with the console's sign-in; the
//! envelope and FileMaker's field names; actions reaching the server; and
//! FileMaker's error codes (`docs/admin-console.md`, "Admin API").

use std::sync::Arc;
use std::time::Duration;

use base64::prelude::{Engine as _, BASE64_STANDARD};
use om_admin::{Console, ConsoleConfig, ConsoleHandle, MockBackend, Notice};
use rustls::crypto::ring;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, RootCertStore};
use serde_json::{json, Value};

const USER: &str = "admin";
const PASSWORD: &str = "correct horse battery staple";
const API: &str = "/fmi/admin/api/v2";

struct Fixture {
    _console: ConsoleHandle,
    backend: Arc<MockBackend>,
    agent: ureq::Agent,
    base: String,
}

/// A reply: its status, its JSON body and the token header.
struct Reply {
    status: u16,
    body: Value,
    access_token: Option<String>,
    www_authenticate: bool,
}

impl Reply {
    /// The envelope's first message code.
    fn code(&self) -> &str {
        self.body["messages"][0]["code"]
            .as_str()
            .unwrap_or_default()
    }

    fn response(&self) -> &Value {
        &self.body["response"]
    }
}

fn start() -> Fixture {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let certificate = generated.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        generated.signing_key.serialize_der(),
    ));
    let backend = Arc::new(MockBackend::sample());
    backend.state().certificate = Some(vec![certificate.to_vec()]);
    let console = Console::start(
        ConsoleConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            certificates: vec![certificate.clone()],
            key,
            user: USER.into(),
            password: PASSWORD.into(),
        },
        backend.clone(),
    )
    .unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(certificate).unwrap();
    let tls = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let agent = ureq::AgentBuilder::new()
        .tls_config(tls)
        .redirects(0)
        .build();
    let base = format!("https://localhost:{}", console.local_addr().port());
    Fixture {
        _console: console,
        backend,
        agent,
        base,
    }
}

fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        BASE64_STANDARD.encode(format!("{user}:{password}"))
    )
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

impl Fixture {
    /// Call `method` on API path `path` with an `Authorization` header and
    /// a JSON body.
    fn call(&self, method: &str, path: &str, auth: Option<&str>, body: Option<&str>) -> Reply {
        let mut request = self
            .agent
            .request(method, &format!("{}{API}{path}", self.base));
        if let Some(auth) = auth {
            request = request.set("Authorization", auth);
        }
        let result = match body {
            Some(body) => request
                .set("Content-Type", "application/json")
                .send_string(body),
            None => request.call(),
        };
        reply(result)
    }

    /// A new token.
    fn sign_in(&self) -> String {
        let r = self.call("POST", "/user/auth", Some(&basic(USER, PASSWORD)), None);
        assert_eq!(r.status, 200, "{}", r.body);
        r.response()["token"].as_str().unwrap().to_owned()
    }

    /// A failed sign-in to the console's own page.
    fn console_sign_in(&self, password: &str) -> u16 {
        let r = self
            .agent
            .post(&format!("{}/admin-console/signin", self.base))
            .send_form(&[("username", USER), ("password", password)]);
        match r {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r.status(),
            Err(e) => panic!("{e}"),
        }
    }
}

fn reply(result: Result<ureq::Response, ureq::Error>) -> Reply {
    let response = match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(e) => panic!("{e}"),
    };
    let status = response.status();
    let access_token = response.header("x-fm-access-token").map(str::to_owned);
    let www_authenticate = response.header("www-authenticate").is_some();
    assert!(
        response.header("access-control-allow-origin").is_none(),
        "the API must not let other sites' pages read it"
    );
    assert_eq!(response.content_type(), "application/json");
    let body = serde_json::from_str(&response.into_string().unwrap()).unwrap();
    Reply {
        status,
        body,
        access_token,
        www_authenticate,
    }
}

#[test]
fn tokens_are_opened_capped_closed_and_share_the_consoles_lockout() {
    let f = start();

    // Without credentials, or with wrong ones, the API answers 1703 and
    // never asks the browser for credentials.
    for auth in [
        None,
        Some(basic(USER, "wrong")),
        Some(bearer(&"0".repeat(64))),
    ] {
        let r = f.call("POST", "/user/auth", auth.as_deref(), None);
        assert_eq!((r.status, r.code()), (401, "1703"), "{auth:?}");
        assert_eq!(r.response(), &json!({}));
        assert!(!r.www_authenticate);
    }
    let r = f.call("GET", "/server/status", None, None);
    assert_eq!((r.status, r.code()), (401, "1703"));

    // A token opens the other calls.
    let first = f.sign_in();
    assert_eq!(first.len(), 64);
    let r = f.call("GET", "/server/status", Some(&bearer(&first)), None);
    assert_eq!(r.status, 200);
    assert_eq!(
        r.body,
        json!({ "response": { "status": "RUNNING" }, "messages": [{ "code": "0", "text": "OK" }] })
    );

    // Basic credentials on another call open a token too, returned in a
    // header.
    let r = f.call("GET", "/databases", Some(&basic(USER, PASSWORD)), None);
    assert_eq!(r.status, 200);
    let second = r.access_token.unwrap();
    assert_eq!(
        f.call("GET", "/clients", Some(&bearer(&second)), None)
            .status,
        200
    );

    // Five at once: a sixth ends the one idle longest, the first.
    let more: Vec<_> = (0..4).map(|_| f.sign_in()).collect();
    let r = f.call("GET", "/server/status", Some(&bearer(&first)), None);
    assert_eq!((r.status, r.code()), (401, "1703"));
    for token in more.iter().chain([&second]) {
        assert_eq!(
            f.call("GET", "/server/status", Some(&bearer(token)), None)
                .status,
            200
        );
    }

    // Signing out ends a token.
    let r = f.call("DELETE", &format!("/user/auth/{second}"), None, None);
    assert_eq!((r.status, r.code()), (200, "0"));
    let r = f.call("DELETE", &format!("/user/auth/{second}"), None, None);
    assert_eq!((r.status, r.code()), (401, "1703"));
    let r = f.call("GET", "/server/status", Some(&bearer(&second)), None);
    assert_eq!(r.status, 401);

    // Another site's page cannot change anything, even with a token, and
    // is told so in the API's envelope.
    let r = reply(
        f.agent
            .request(
                "PATCH",
                &format!("{}{API}/server/config/blocknewusers", f.base),
            )
            .set("Authorization", &bearer(&more[0]))
            .set("Origin", "https://evil.example")
            .set("Content-Type", "application/json")
            .send_string(r#"{"blockNewUsers": true}"#),
    );
    assert_eq!((r.status, r.code()), (403, "-1"));
    assert_eq!(f.backend.state().block_new_users, Some(false));
    // A sign-out path that does not decode names no token.
    let r = f.call("DELETE", "/user/auth/%FF", None, None);
    assert_eq!((r.status, r.code()), (401, "1703"));
}

#[test]
fn failed_api_and_console_sign_ins_count_together() {
    let f = start();
    let token = f.sign_in();
    for _ in 0..4 {
        let r = f.call("POST", "/user/auth", Some(&basic(USER, "wrong")), None);
        assert_eq!(r.status, 401);
    }
    // The fifth failure, on the console's page, locks the address out of
    // both.
    assert_eq!(f.console_sign_in("wrong"), 429);
    let r = f.call("POST", "/user/auth", Some(&basic(USER, PASSWORD)), None);
    assert_eq!((r.status, r.code()), (429, "1703"));
    let r = f.call("GET", "/clients", Some(&basic(USER, PASSWORD)), None);
    assert_eq!(r.status, 429);
    assert_eq!(f.console_sign_in(PASSWORD), 429);
    // A token opened before goes on working.
    let r = f.call("GET", "/server/metadata", Some(&bearer(&token)), None);
    assert_eq!(r.status, 200);
}

#[test]
fn reads_use_filemaker_server_names() {
    let f = start();
    let auth = bearer(&f.sign_in());
    let get = |path: &str| {
        let r = f.call("GET", path, Some(&auth), None);
        assert_eq!((r.status, r.code()), (200, "0"), "{path}: {}", r.body);
        assert_eq!(r.body["messages"][0]["text"], "OK");
        r.body["response"].clone()
    };
    let is_api_time = |v: &Value| {
        let t = v.as_str().unwrap_or_default().as_bytes();
        t.len() == 19 && t[4] == b'-' && t[10] == b'T' && t[16] == b':'
    };

    let server = get("/server/metadata");
    assert_eq!(server["serverName"], "sample-server");
    assert_eq!(server["serverID"], "5A3F0C2B9E7D41A8B6C4D2E0F1A3B5C7");
    assert_eq!(server["serverIP"], "192.0.2.10");
    assert_eq!(server["serverVersion"], "OpenMaker Server 0.0.0");
    assert!(is_api_time(&server["serverHostTime"]));
    assert!(server["offset"].is_i64());

    let databases = get("/databases");
    assert_eq!(databases["totalDBCount"], 4);
    // Open and paused files are open; the closed one is not.
    assert_eq!(databases["openDBCount"], 3);
    let contacts = &databases["databases"][0];
    assert_eq!(contacts["id"], "1");
    assert_eq!(contacts["filename"], "Contacts.fmp12");
    assert_eq!(contacts["status"], "NORMAL");
    assert_eq!(contacts["clients"], 3);
    assert_eq!(contacts["size"], 3_276_800);
    assert_eq!(contacts["isEncrypted"], false);
    if cfg!(target_os = "linux") {
        assert_eq!(contacts["folder"], "filelinux:/srv/openmaker/Databases/");
    }
    assert_eq!(databases["databases"][2]["status"], "PAUSED");

    let clients = get("/clients");
    assert_eq!(clients["fmproCount"], 2);
    assert_eq!(clients["fmgoCount"], 1);
    let client = &clients["clients"][0];
    assert_eq!(client["id"], "11");
    assert_eq!(client["userName"], "Sample User 1");
    assert_eq!(client["computerName"], "Sample-Laptop-1");
    assert_eq!(client["ipaddress"], "192.0.2.21");
    assert_eq!(client["appType"], "FMPRO");
    assert_eq!(client["appVersion"], "Pro 26.0.3");
    assert_eq!(client["status"], "NORMAL");
    assert!(is_api_time(&client["connectTime"]));
    assert!(client["connectDuration"]
        .as_str()
        .unwrap()
        .starts_with("03:0"));
    assert_eq!(
        client["guestFiles"],
        json!([{ "id": "1", "filename": "Contacts.fmp12", "accountName": "Admin" }])
    );

    let schedules = get("/schedules");
    let nightly = &schedules["schedules"][0];
    assert_eq!(nightly["id"], "2");
    assert_eq!(nightly["name"], "Nightly Totals");
    assert_eq!(nightly["status"], "IDLE");
    assert_eq!(nightly["lastError"], 0);
    assert!(is_api_time(&nightly["lastRun"]) && is_api_time(&nightly["nextRun"]));
    assert_eq!(
        nightly["filemakerScriptType"],
        json!({ "resource": "file:Invoices.fmp12", "fmScriptAccount": "Admin" })
    );
    assert_eq!(get("/schedules/3")["schedule"]["backupType"], json!({}));

    let folders = get("/server/currentfoldersettings");
    if cfg!(target_os = "linux") {
        assert_eq!(
            folders["defaultDatabaseFolder"],
            "filelinux:/srv/openmaker/Databases/"
        );
        assert_eq!(folders["backupPath"], "filelinux:/srv/openmaker/Backups/");
    }
    assert_eq!(folders["additionalDatabaseFolder"], json!(["disabled"]));

    let certificate = &get("/server/certificate/info")["certificate"];
    assert!(certificate["subjectName"].is_string());
    assert_eq!(certificate["subjectName"], certificate["issuerName"]);

    assert_eq!(get("/server/config/blocknewusers")["blockNewUsers"], false);
}

#[test]
fn actions_reach_the_server() {
    let f = start();
    let auth = bearer(&f.sign_in());
    let send = |method: &str, path: &str, body: Option<&str>| {
        let r = f.call(method, path, Some(&auth), body);
        assert_eq!((r.status, r.code()), (200, "0"), "{path}: {}", r.body);
        r.body["response"].clone()
    };

    let r = send(
        "DELETE",
        "/clients/12?messageText=Closing%20for%20maintenance&graceTime=30",
        None,
    );
    assert_eq!(r, json!({ "client": { "id": "12" } }));
    let r = send(
        "POST",
        "/clients/11/message",
        Some(r#"{"messageText": "Please save your work."}"#),
    );
    assert_eq!(r, json!({ "client": { "id": "11" } }));
    {
        let state = f.backend.state();
        assert!(state.clients.iter().all(|c| c.id != 12));
        assert_eq!(
            state.notices,
            [Notice {
                message: "Closing for maintenance".into(),
                grace: Duration::from_secs(30),
            }]
        );
        assert_eq!(state.messages, [(11, "Please save your work.".into())]);
    }

    let r = send("PATCH", "/databases/3", Some(r#"{"status": "RESUMED"}"#));
    assert_eq!(
        r,
        json!({ "database": { "id": "3", "filename": "Inventory.fmp12", "status": "NORMAL" } })
    );
    let r = send(
        "PATCH",
        "/databases/2",
        Some(r#"{"status": "CLOSED", "messageText": "Closing Invoices."}"#),
    );
    assert_eq!(r["database"]["status"], "CLOSED");
    assert_eq!(
        f.backend.state().notices[1],
        Notice {
            message: "Closing Invoices.".into(),
            grace: Duration::from_secs(90),
        }
    );
    // Opening every file opens the two closed ones and leaves the rest.
    let r = send("PATCH", "/databases", Some(r#"{"status": "OPENED"}"#));
    let statuses: Vec<_> = r["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["NORMAL"; 4]);

    let r = send(
        "PATCH",
        "/server/config/blocknewusers",
        Some(r#"{"blockNewUsers": true}"#),
    );
    assert_eq!(r, json!({ "blockNewUsers": true }));
    assert_eq!(f.backend.state().block_new_users, Some(true));

    let r = send("PATCH", "/schedules/3", Some(r#"{"enabled": false}"#));
    assert_eq!(r["schedule"]["enabled"], false);
    assert_eq!(r["schedule"]["nextRun"], "0000-00-00T00:00:00");
    let before = f.backend.state().schedules.as_ref().unwrap()[0]
        .last_run
        .clone();
    send("PATCH", "/schedules/2", Some(r#"{"status": "RUNNING"}"#));
    let after = f.backend.state().schedules.as_ref().unwrap()[0]
        .last_run
        .clone();
    assert_ne!(before, after);
}

#[test]
fn refusals_use_filemaker_server_codes() {
    let f = start();
    let auth = bearer(&f.sign_in());
    let refused = |method: &str, path: &str, body: Option<&str>, status: u16, code: &str| {
        let r = f.call(method, path, Some(&auth), body);
        assert_eq!(
            (r.status, r.code()),
            (status, code),
            "{method} {path}: {}",
            r.body
        );
        assert_eq!(r.response(), &json!({}));
    };

    refused("DELETE", "/clients/999", None, 404, "1700");
    refused("DELETE", "/clients/11?graceTime=3601", None, 400, "1708");
    refused("DELETE", "/clients/x", None, 400, "1708");
    // A path the router cannot decode is refused in the envelope too.
    refused("DELETE", "/clients/%FF", None, 400, "1708");
    refused("POST", "/clients/11/message", Some("{}"), 400, "1707");
    refused(
        "POST",
        "/clients/11/message",
        Some(r#"{"messageText": "  "}"#),
        400,
        "1708",
    );
    let long = json!({ "messageText": "x".repeat(201) }).to_string();
    refused("POST", "/clients/11/message", Some(&long), 400, "1708");
    refused(
        "POST",
        "/clients/11/message",
        Some("{\"messageText\""),
        400,
        "1710",
    );
    // Each status takes only its own fields: messageText is required with
    // CLOSED (an empty one passes, and the closed file then refuses), key
    // and saveKey go only with OPENED, force only with CLOSED.
    for (path, body, status, code) in [
        (
            "/databases/4",
            r#"{"status": "CLOSED", "messageText": ""}"#,
            409,
            "1709",
        ),
        ("/databases/1", r#"{"status": "CLOSED"}"#, 400, "1707"),
        (
            "/databases/1",
            r#"{"status": "PAUSED", "force": true}"#,
            400,
            "1708",
        ),
        (
            "/databases/1",
            r#"{"status": "PAUSED", "key": "x"}"#,
            400,
            "1708",
        ),
        (
            "/databases/4",
            r#"{"status": "OPENED", "messageText": "x"}"#,
            400,
            "1708",
        ),
        (
            "/databases/1",
            r#"{"status": "CLOSED", "messageText": "", "saveKey": true}"#,
            400,
            "1708",
        ),
        (
            "/databases",
            r#"{"status": "PAUSED", "force": true}"#,
            400,
            "1708",
        ),
    ] {
        refused("PATCH", path, Some(body), status, code);
    }
    let databases = f.backend.state().databases.clone();
    assert_eq!(databases[0].status, om_admin::DatabaseStatus::Normal);
    assert_eq!(databases[3].status, om_admin::DatabaseStatus::Closed);
    refused(
        "PATCH",
        "/databases/1",
        Some(r#"{"status": "FLUSHED"}"#),
        409,
        "1709",
    );
    refused(
        "PATCH",
        "/databases/1",
        Some(r#"{"status": "open"}"#),
        400,
        "1708",
    );
    refused(
        "PATCH",
        "/server/status",
        Some(r#"{"status": "STOPPED", "graceTime": 0}"#),
        409,
        "1709",
    );
    refused(
        "PATCH",
        "/schedules/2",
        Some(r#"{"name": "Renamed"}"#),
        409,
        "1709",
    );
    // FileMaker Server reserves schedule id 1.
    refused("GET", "/schedules/1", None, 400, "1708");
    refused(
        "PATCH",
        "/schedules/1",
        Some(r#"{"enabled": true}"#),
        400,
        "1708",
    );
    refused(
        "PATCH",
        "/schedules/9",
        Some(r#"{"enabled": true}"#),
        404,
        "1700",
    );
    let big = json!({ "messageText": "x".repeat(20_000) }).to_string();
    refused("POST", "/clients/11/message", Some(&big), 413, "1708");
    refused("GET", "/license/info", None, 404, "1700");
    // The prefix itself, with or without its slash, answers in the envelope;
    // the console's own pages keep their page.
    refused("GET", "", None, 404, "1700");
    refused("GET", "/", None, 404, "1700");
    match f
        .agent
        .get(&format!("{}/admin-console/nope", f.base))
        .call()
    {
        Err(ureq::Error::Status(404, page)) => assert_eq!(page.content_type(), "text/html"),
        other => panic!("{other:?}"),
    }
    refused("POST", "/server/metadata", None, 405, "1700");

    // A body must say it is JSON.
    let r = reply(
        f.agent
            .post(&format!("{}{API}/clients/11/message", f.base))
            .set("Authorization", &auth)
            .send_string(r#"{"messageText": "Hello"}"#),
    );
    assert_eq!((r.status, r.code()), (400, "1708"));

    // A feature the server lacks does not exist when read, and its changes
    // are refused.
    f.backend.state().schedules = None;
    f.backend.state().block_new_users = None;
    refused("GET", "/schedules", None, 404, "1700");
    refused("GET", "/server/config/blocknewusers", None, 404, "1700");
    refused(
        "PATCH",
        "/server/config/blocknewusers",
        Some(r#"{"blockNewUsers": true}"#),
        409,
        "1709",
    );
    assert!(f.backend.state().messages.is_empty());
}
