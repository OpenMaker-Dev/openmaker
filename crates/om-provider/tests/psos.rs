//! Perform Script on Server through the native provider, against an
//! `om-server` on loopback hosting a file built for the test (om-server's
//! `tests/fixtures/tasks.rs`), over verified TLS: the provider calls
//! themselves, and a client-side script whose Perform Script on Server steps
//! reach the provider through its script host, as the desktop client's will.

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;

use std::sync::Arc;
use std::time::Duration;

use fixture::{TempFile, ACCOUNT, FILE, PASSWORD};
use om_data::calc::eval::{CalcHost, VarScope, Variables};
use om_data::calc::{Ctx, Date, Locale, Value};
use om_data::runtime::*;
use om_data::{Connection, DataProvider, Script};
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

/// A server hosting `file` on an ephemeral port, and the CA to trust.
fn host(file: &TempFile) -> (u16, Vec<u8>) {
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca_pem = generated.cert.pem().into_bytes();
    let identity =
        Identity::from_pem(&ca_pem, generated.key_pair.serialize_pem().as_bytes()).unwrap();
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path: file.0.clone(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    // Reads through views are answered through the test view too, and
    // checked against the file engine's (docs/server-storage.md 5, "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    (port, ca_pem)
}

fn open(port: u16, ca_pem: Vec<u8>) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca_pem,
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    provider
        .open(&Connection {
            host: "127.0.0.1".into(),
            port,
            file: FILE.into(),
            account: ACCOUNT.into(),
            password: PASSWORD.into(),
        })
        .unwrap();
    provider
}

fn call(script: u64, parameter: &str, wait: bool) -> ServerScriptCall {
    ServerScriptCall {
        file: 0,
        script,
        parameter: Some(Value::text(parameter)),
        wait,
    }
}

fn text(value: &Value) -> String {
    let locale = Locale::en_us();
    value
        .to_text(&Ctx::new(&locale, Date::from_ymd(2026, 10, 28).unwrap()))
        .into_string()
}

/// A client's script host whose Perform Script on Server steps go to the
/// provider; it has no window, so other steps are unavailable.
struct ClientHost<'a> {
    provider: &'a mut NativeProvider,
    scripts: Vec<Arc<Script>>,
    locale: Locale,
    locals: Variables,
    globals: Variables,
}

impl CalcHost for ClientHost<'_> {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    fn today(&self) -> Date {
        Date::from_ymd(2026, 10, 28).unwrap()
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        match scope {
            VarScope::Local => &mut self.locals,
            VarScope::Global => &mut self.globals,
        }
    }
}

impl ScriptHost for ClientHost<'_> {
    fn script(&mut self, _file: FileId, id: u64) -> Option<Arc<Script>> {
        self.scripts.iter().find(|s| s.id == id).cloned()
    }
}

impl Network for ClientHost<'_> {
    fn perform_script_on_server(
        &mut self,
        call: &ServerScriptCall,
    ) -> HostResult<ServerScriptResult> {
        self.provider
            .perform_script_on_server(call)
            .map_err(|_| HostError::Error(ErrorCode::UNAVAILABLE))
    }

    fn perform_script_on_server_with_callback(&mut self, call: &ServerCallbackCall) -> HostResult {
        let error = self
            .provider
            .perform_script_on_server_with_callback(&call.call, 7)
            .map_err(|_| HostError::Error(ErrorCode::UNAVAILABLE))?;
        if error.is_error() {
            Err(error.into())
        } else {
            Ok(())
        }
    }
}

impl Navigation for ClientHost<'_> {}
impl Records for ClientHost<'_> {}
impl Fields for ClientHost<'_> {}
impl FoundSets for ClientHost<'_> {}
impl Windows for ClientHost<'_> {}
impl Dialogs for ClientHost<'_> {}
impl Files for ClientHost<'_> {}
impl Accounts for ClientHost<'_> {}
impl Platform for ClientHost<'_> {}

#[test]
fn the_provider_performs_scripts_on_the_server() {
    let file = TempFile::new(
        "provider",
        &[
            fixture::server_work(),
            fixture::failing(),
            fixture::slow(0.2),
        ],
        true,
    );
    let (port, ca) = host(&file);
    let mut provider = open(port, ca);

    let done = provider
        .perform_script_on_server(&call(1, "provider", true))
        .unwrap();
    assert_eq!(done.error, ErrorCode::NONE);
    let result = text(done.result.as_ref().expect("a result when waiting"));
    assert!(
        result.starts_with("3|3|From provider|Admin|Server "),
        "{result}"
    );
    let failed = provider
        .perform_script_on_server(&call(2, "", true))
        .unwrap();
    assert_eq!(failed.error, ErrorCode(201));
    assert_eq!(failed.result.as_ref().map(text).as_deref(), Some("failed"));
    let quick = provider
        .perform_script_on_server(&call(3, "", false))
        .unwrap();
    assert_eq!(
        (quick.error, quick.result.is_none()),
        (ErrorCode::NONE, true)
    );

    // With a callback: the caller's id comes back with the result.
    assert_eq!(
        provider
            .perform_script_on_server_with_callback(&call(3, "", false), 99)
            .unwrap(),
        ErrorCode::NONE
    );
    // Polled without waiting, as a user interface's idle loop would.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let results = loop {
        let results = provider.server_script_results(Duration::ZERO).unwrap();
        if !results.is_empty() {
            break results;
        }
        assert!(std::time::Instant::now() < deadline, "no callback result");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(results.len(), 1);
    assert_eq!(
        (
            results[0].callback,
            results[0].error,
            results[0].result.as_str()
        ),
        (99, ErrorCode::NONE, "slow done")
    );
    // The provider still reads the file, with the script's record in it.
    let layout = provider.layouts().unwrap()[0].id;
    let page = provider.records(layout, 0, 10).unwrap();
    assert_eq!(page.total, 3);
    provider.close();
}

#[test]
fn a_client_script_performs_scripts_on_the_server() {
    let file = TempFile::new("wired", &[fixture::server_work(), fixture::failing()], true);
    let (port, ca) = host(&file);
    let mut provider = open(port, ca);
    // The client knows the server's scripts from the file's catalog.
    let catalog = provider.scripts().unwrap();
    assert_eq!(catalog.scripts.len(), 2);
    let client = fixture::script(
        10,
        "Client",
        vec![
            fixture::on(86, true),
            // Set Variable sets Get ( LastError ) itself: read it first.
            fixture::perform_on_server(1, "\"wired\"", true),
            fixture::set_variable("$$error", "Get ( LastError )"),
            fixture::set_variable("$$result", "Get ( ScriptResult )"),
            fixture::perform_on_server(2, "\"\"", true),
            fixture::set_variable("$$failed_error", "Get ( LastError )"),
            fixture::set_variable("$$failed", "Get ( ScriptResult )"),
            fixture::perform_on_server(1, "\"later\"", false),
            fixture::set_variable("$$async_error", "Get ( LastError )"),
            fixture::set_variable("$$async", "Get ( ScriptResult )"),
        ],
    );
    let mut scripts: Vec<Arc<Script>> = catalog.scripts.into_iter().map(Arc::new).collect();
    scripts.push(Arc::new(client));
    let mut host = ClientHost {
        provider: &mut provider,
        scripts,
        locale: Locale::en_us(),
        locals: Variables::new(),
        globals: Variables::new(),
    };
    let mut runtime = Runtime::new(Options::new(Client::Mac));
    runtime
        .perform_script(&mut host, ScriptCall::new(0, 10))
        .unwrap();
    assert_eq!(runtime.run(&mut host), RunState::Idle);
    // An empty value clears a variable: read it as empty text.
    let mut var = |name: &str| {
        runtime
            .globals(0)
            .get(name, 1)
            .map(text)
            .unwrap_or_default()
    };
    assert!(
        var("$$result").starts_with("3|3|From wired|Admin|Server "),
        "{}",
        var("$$result")
    );
    assert_eq!(var("$$error"), "0");
    assert_eq!(
        (var("$$failed").as_str(), var("$$failed_error").as_str()),
        ("failed", "201")
    );
    // Not waiting clears Get ( ScriptResult ).
    assert_eq!(
        (var("$$async").as_str(), var("$$async_error").as_str()),
        ("", "0")
    );
    provider.close();
}
