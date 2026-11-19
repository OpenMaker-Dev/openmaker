//! End to end over HTTPS: signing in, the session cookie, the CSRF and
//! origin checks on state-changing requests, signing out, the lockout after
//! repeated failures, the connection limit per address, and shutdown
//! (`docs/admin-console.md`, "Security" and "Architecture"); the views'
//! actions reaching the server, the Databases view's client actions among
//! them.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use om_admin::{
    Alert, Backup, Backups, Console, ConsoleConfig, ConsoleHandle, LogEntry, LogFile, MockBackend,
    MockState, Notice, Schedule, ScheduleKind, Severity,
};
use rustls::crypto::ring;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const USER: &str = "admin";
const PASSWORD: &str = "correct horse battery staple";

struct Fixture {
    console: Option<ConsoleHandle>,
    backend: Arc<MockBackend>,
    tls: Arc<ClientConfig>,
    agent: ureq::Agent,
    base: String,
}

fn start() -> Fixture {
    start_with(MockState {
        alerts: vec![Alert {
            id: 7,
            time: SystemTime::now(),
            severity: Severity::Critical,
            message: "A hosted file could not be saved.".into(),
        }],
        block_new_users: Some(false),
        ..MockState::empty()
    })
}

fn start_with(state: MockState) -> Fixture {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let certificate = generated.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        generated.signing_key.serialize_der(),
    ));
    let backend = Arc::new(MockBackend::new(state));
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
        .tls_config(tls.clone())
        .redirects(0)
        .build();
    let base = format!("https://localhost:{}", console.local_addr().port());
    Fixture {
        console: Some(console),
        backend,
        tls,
        agent,
        base,
    }
}

/// The response whatever its status.
fn response(result: Result<ureq::Response, ureq::Error>) -> ureq::Response {
    match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(e) => panic!("{e}"),
    }
}

impl Fixture {
    /// A TLS connection of its own, its handshake done; None when the
    /// console closes it instead.
    fn connect(&self) -> Option<StreamOwned<ClientConnection, TcpStream>> {
        let port = self.console.as_ref().unwrap().local_addr().port();
        let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let conn =
            ClientConnection::new(self.tls.clone(), "localhost".try_into().unwrap()).unwrap();
        let mut stream = StreamOwned::new(conn, tcp);
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock).ok()?;
        }
        Some(stream)
    }

    fn get(&self, path: &str, cookie: Option<&str>) -> ureq::Response {
        let mut request = self.agent.get(&format!("{}{path}", self.base));
        if let Some(cookie) = cookie {
            request = request.set("Cookie", cookie);
        }
        response(request.call())
    }

    fn post(
        &self,
        path: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
        form: &[(&str, &str)],
    ) -> ureq::Response {
        let mut request = self.agent.post(&format!("{}{path}", self.base));
        if let Some(cookie) = cookie {
            request = request.set("Cookie", cookie);
        }
        if let Some(origin) = origin {
            request = request.set("Origin", origin);
        }
        response(request.send_form(form))
    }

    fn sign_in(&self, password: &str) -> ureq::Response {
        self.post(
            "/admin-console/signin",
            None,
            Some(&self.base),
            &[("username", USER), ("password", password)],
        )
    }
}

fn location(response: &ureq::Response) -> &str {
    response.header("location").unwrap_or_default()
}

/// The page's CSRF token.
fn csrf(page: &str) -> String {
    let start = page
        .find(r#"name="csrf" value=""#)
        .expect("a form with a CSRF token")
        + 19;
    page[start..start + 64].to_owned()
}

#[test]
fn signing_in_opens_the_console_and_signing_out_closes_it() {
    let f = start();
    let view = "/admin-console/app/configuration/generalsettings";

    // Signed out, the console sends the browser to the sign-in page, whose
    // response carries the security headers.
    let r = f.get("/admin-console/app/configuration", None);
    assert_eq!((r.status(), location(&r)), (303, "/admin-console/signin"));
    let r = f.get("/admin-console/signin", None);
    assert_eq!(r.status(), 200);
    let csp = r.header("content-security-policy").unwrap();
    assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"));
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(r.header("referrer-policy"), Some("same-origin"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let page = r.into_string().unwrap();
    let stylesheet = page
        .split('"')
        .find(|s| s.starts_with("/admin-console/assets/console.") && s.ends_with(".css"))
        .unwrap();
    let r = f.get(stylesheet, None);
    assert_eq!(r.status(), 200);
    assert_eq!(r.header("content-type"), Some("text/css; charset=utf-8"));
    assert!(r.header("cache-control").unwrap().contains("immutable"));

    // A wrong password opens nothing.
    let r = f.sign_in("wrong");
    assert_eq!(r.status(), 200);
    assert!(r.header("set-cookie").is_none());
    assert!(r
        .into_string()
        .unwrap()
        .contains("Enter a valid user name and password."));

    // The right one opens a session in a cookie scripts and other sites
    // cannot use.
    let r = f.sign_in(PASSWORD);
    assert_eq!(
        (r.status(), location(&r)),
        (303, "/admin-console/app/dashboard")
    );
    let set_cookie = r.header("set-cookie").unwrap().to_owned();
    for attribute in [
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/admin-console",
    ] {
        assert!(set_cookie.contains(attribute), "{set_cookie}");
    }
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    let cookie = Some(cookie.as_str());

    // A tab opens its first view, which lists the alerts in the bell.
    let r = f.get("/admin-console/app/configuration", cookie);
    assert_eq!((r.status(), location(&r)), (303, view));
    let r = f.get(view, cookie);
    assert_eq!(r.status(), 200);
    let page = r.into_string().unwrap();
    assert!(page.contains("General Settings"));
    assert!(page.contains("A hosted file could not be saved."));
    let token = csrf(&page);

    // State changes need the session's token, from the console's origin.
    let r = f.post(
        "/admin-console/signout",
        cookie,
        Some("https://evil.example"),
        &[("csrf", &token)],
    );
    assert_eq!(r.status(), 403);
    let r = f.post(
        "/admin-console/signout",
        cookie,
        None,
        &[("csrf", &"0".repeat(64))],
    );
    assert_eq!(r.status(), 403);
    let r = f.post(
        "/admin-console/alerts/7/acknowledge",
        cookie,
        Some(&f.base),
        &[("csrf", &token), ("back", view)],
    );
    assert_eq!((r.status(), location(&r)), (303, view));
    assert!(om_admin::Backend::alerts(&*f.backend).is_empty());

    // A view's action changes the server and returns to the page, which
    // shows its outcome once.
    let toggle = "/admin-console/app/configuration/generalsettings/blocknewusers";
    let r = f.post(
        toggle,
        cookie,
        Some(&f.base),
        &[("csrf", &token), ("back", view), ("on", "true")],
    );
    assert_eq!((r.status(), location(&r)), (303, view));
    assert_eq!(f.backend.state().block_new_users, Some(true));
    let page = f.get(view, cookie).into_string().unwrap();
    assert!(page.contains(r#"class="flash ok""#));
    assert!(page.contains(r#"aria-checked="true""#));
    let page = f.get(view, cookie).into_string().unwrap();
    assert!(!page.contains(r#"class="flash"#));

    // Signing out ends the session; its cookie opens nothing after.
    let r = f.post(
        "/admin-console/signout",
        cookie,
        Some(&f.base),
        &[("csrf", &token)],
    );
    assert_eq!((r.status(), location(&r)), (303, "/admin-console/signin"));
    assert!(r.header("set-cookie").unwrap().contains("Max-Age=0"));
    let r = f.get(view, cookie);
    assert_eq!((r.status(), location(&r)), (303, "/admin-console/signin"));
}

#[test]
fn run_now_and_the_enabled_toggle_reach_the_server() {
    let f = start();
    f.backend.state().schedules = Some(vec![Schedule {
        id: 4,
        name: "Nightly Totals".into(),
        kind: ScheduleKind::FileMakerScript,
        target: Some("file:Sample.fmp12".into()),
        account: Some("Admin".into()),
        enabled: true,
        next_run: None,
        last_run: None,
        running: false,
    }]);
    let schedule = |f: &Fixture| f.backend.state().schedules.as_ref().unwrap()[0].clone();
    let r = f.sign_in(PASSWORD);
    let cookie = r.header("set-cookie").unwrap().split(';').next().unwrap();
    let cookie = Some(cookie);
    let view = "/admin-console/app/configuration/schedules";
    let page = f.get(view, cookie).into_string().unwrap();
    assert!(page.contains("Nightly Totals"));
    let token = csrf(&page);

    // Run Now runs the checked schedules and returns to the view.
    let run = "/admin-console/app/configuration/schedules/run";
    let r = f.post(
        run,
        cookie,
        Some(&f.base),
        &[("csrf", &token), ("back", view), ("schedule", "4")],
    );
    assert_eq!((r.status(), location(&r)), (303, view));
    assert!(schedule(&f).last_run.is_some_and(|run| run.succeeded));

    // The grid's live region shows the run, and its toggles return to the
    // view rather than to the tab.
    let region = f
        .get("/admin-console/fragments/configuration/schedules", cookie)
        .into_string()
        .unwrap();
    assert!(region.contains("Succeeded"));
    assert!(region.contains(&format!(r#"name="back" value="{view}""#)));

    // A region the server cannot read answers an error, which the script
    // leaves the grid and its checked boxes alone for.
    let schedules = f.backend.state().schedules.take();
    let r = f.get("/admin-console/fragments/configuration/schedules", cookie);
    assert_eq!(r.status(), 503);
    f.backend.state().schedules = schedules;

    // The Enabled toggle saves at once.
    let r = f.post(
        "/admin-console/app/configuration/schedules/4/enabled",
        cookie,
        Some(&f.base),
        &[("csrf", &token), ("back", view), ("on", "false")],
    );
    assert_eq!((r.status(), location(&r)), (303, view));
    assert!(!schedule(&f).enabled);
    let page = f.get(view, cookie).into_string().unwrap();
    assert!(page.contains(r#"class="flash ok""#), "{page}");

    // A schedule that is gone is reported, not passed over.
    let r = f.post(
        run,
        cookie,
        Some(&f.base),
        &[("csrf", &token), ("back", view), ("schedule", "9")],
    );
    assert_eq!(r.status(), 303);
    let page = f.get(view, cookie).into_string().unwrap();
    assert!(page.contains(r#"class="flash error""#), "{page}");
}

#[test]
fn repeated_failures_lock_the_address_out() {
    let f = start();
    for _ in 1..5 {
        assert_eq!(f.sign_in("wrong").status(), 200);
    }
    // The fifth failure locks the address out, and then the right password
    // is refused too.
    assert_eq!(f.sign_in("wrong").status(), 429);
    let r = f.sign_in(PASSWORD);
    assert_eq!(r.status(), 429);
    assert!(r.header("set-cookie").is_none());
    assert!(r
        .into_string()
        .unwrap()
        .contains("Too many failed sign-in attempts"));
}

#[test]
fn one_address_cannot_take_every_connection() {
    let f = start();
    let mut held: Vec<_> = (0..8).map(|_| f.connect().expect("a connection")).collect();
    // The ninth from the same address is closed during its handshake...
    assert!(f.connect().is_none());
    // ...and gets in once one of the eight has gone.
    drop(held.pop());
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.connect().is_none() {
        assert!(
            Instant::now() < deadline,
            "the closed connection's place was not freed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_request_being_answered_finishes_during_shutdown() {
    let mut f = start();
    let port = f.console.as_ref().unwrap().local_addr().port();
    let mut stream = f.connect().unwrap();
    let body = "username=admin&password=wrong";
    let head = format!(
        "POST /admin-console/signin HTTP/1.1\r\nHost: localhost\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\
         Expect: 100-continue\r\n\r\n",
        body.len()
    );
    // The console answers "100 Continue" once it reads the request's body:
    // from then on the request is being answered, not an idle connection
    // (which a shutdown closes at once).
    stream.write_all(head.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        seen.push(byte[0]);
    }
    assert!(
        String::from_utf8_lossy(&seen).starts_with("HTTP/1.1 100"),
        "{}",
        String::from_utf8_lossy(&seen)
    );
    // All but the body's last byte, then the shutdown: wait until the
    // console accepts no more connections, so that it has begun.
    stream
        .write_all(&body.as_bytes()[..body.len() - 1])
        .unwrap();
    stream.flush().unwrap();
    let console = f.console.take().unwrap();
    let shutdown = std::thread::spawn(move || console.shutdown());
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < deadline, "the console kept accepting");
        std::thread::sleep(Duration::from_millis(5));
    }
    stream
        .write_all(&body.as_bytes()[body.len() - 1..])
        .unwrap();
    stream.flush().unwrap();
    let mut reply = Vec::new();
    let _ = stream.read_to_end(&mut reply);
    let reply = String::from_utf8_lossy(&reply);
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.contains("Enter a valid user name and password."));
    shutdown.join().unwrap();
}

/// A signed-in session's cookie and CSRF token.
fn signed_in(f: &Fixture) -> (String, String) {
    let r = f.sign_in(PASSWORD);
    let cookie = r
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let page = f
        .get("/admin-console/app/backups/allbackups", Some(&cookie))
        .into_string()
        .unwrap();
    let token = csrf(&page);
    (cookie, token)
}

#[test]
fn backup_and_log_actions_reach_the_server() {
    let now = SystemTime::now();
    let backup = |name: &str, preserved: bool| Backup {
        name: name.into(),
        schedule: "Daily".into(),
        time: now,
        label: "Auto-backup".into(),
        preserved,
        size: None,
    };
    // A log file whose name a header must not carry as it is, with 250
    // entries, newest first.
    let log = "Event \"1\".log";
    let entries = (0..250)
        .map(|i| LogEntry {
            timestamp: format!("2026-11-11 10:{:02}:{:02}.000 -0600", i / 60, i % 60),
            level: "Information".into(),
            code: Some(148),
            host: "server".into(),
            message: format!("Entry {i:03}."),
        })
        .rev()
        .collect();
    let f = start_with(MockState {
        schedules: Some(vec![Schedule {
            id: 3,
            name: "Daily".into(),
            kind: ScheduleKind::Backup,
            target: None,
            account: None,
            enabled: true,
            next_run: None,
            last_run: None,
            running: false,
        }]),
        backups: Some(Backups {
            items: vec![backup("Daily_A", false), backup("Daily_B", true)],
            running: None,
        }),
        logs: Some(vec![(
            LogFile {
                name: log.into(),
                size: 0,
                modified: now,
            },
            entries,
        )]),
        ..MockState::empty()
    });
    let (cookie, token) = signed_in(&f);
    let cookie = Some(cookie.as_str());
    let all = "/admin-console/app/backups/allbackups";
    let selected = "/admin-console/app/backups/actions/selected";
    let post = |path: &str, form: &[(&str, &str)]| {
        let mut fields = vec![("csrf", token.as_str()), ("back", all)];
        fields.extend_from_slice(form);
        let r = f.post(path, cookie, Some(&f.base), &fields);
        assert_eq!((r.status(), location(&r)), (303, all), "{path} {form:?}");
        f.get(all, cookie).into_string().unwrap()
    };
    let items = || f.backend.state().backups.clone().unwrap().items;
    let item = |name: &str| items().into_iter().find(|b| b.name == name);

    // Back Up Now runs the chosen schedule.
    let page = post(
        "/admin-console/app/backups/actions/backupnow",
        &[("schedule", "3")],
    );
    assert!(page.contains(r#"class="flash ok""#), "{page}");
    assert_eq!(items().len(), 3);
    assert_eq!(items()[0].label, "On-demand");
    let on_demand = items()[0].name.clone();

    // Preserve Backup and Edit Label act on every checked backup.
    post(selected, &[("backup", "Daily_A"), ("preserve", "1")]);
    assert!(item("Daily_A").unwrap().preserved);
    post(
        selected,
        &[
            ("backup", "Daily_A"),
            ("backup", "Daily_B"),
            ("relabel", "1"),
            ("new_label", "  Before upgrade "),
        ],
    );
    assert_eq!(item("Daily_A").unwrap().label, "Before upgrade");
    assert_eq!(item("Daily_B").unwrap().label, "Before upgrade");

    // A label that is too long or more than one line, a name that could
    // lead out of the backup folder, and deleting a backup that is not
    // preserved are refused, with the reason on the page.
    let long = "x".repeat(101);
    for form in [
        vec![
            ("backup", "Daily_A"),
            ("relabel", "1"),
            ("new_label", &long),
        ],
        vec![
            ("backup", "Daily_A"),
            ("relabel", "1"),
            ("new_label", "Before\nIsPreserved=true"),
        ],
        vec![("backup", "../Daily_A"), ("delete", "1")],
        vec![("backup", &on_demand), ("delete", "1")],
        vec![("delete", "1")],
    ] {
        let page = post(selected, &form);
        assert!(page.contains(r#"class="flash error""#), "{form:?}");
    }
    assert_eq!(item("Daily_A").unwrap().label, "Before upgrade");
    assert!(item(&on_demand).is_some());

    // Delete removes the checked preserved backups; a form without the
    // session's token changes nothing.
    let r = f.post(
        selected,
        cookie,
        Some(&f.base),
        &[
            ("csrf", &"0".repeat(64)),
            ("backup", "Daily_A"),
            ("delete", "1"),
        ],
    );
    assert_eq!(r.status(), 403);
    post(
        selected,
        &[
            ("backup", "Daily_A"),
            ("backup", "Daily_B"),
            ("delete", "1"),
        ],
    );
    assert_eq!(items().len(), 1);

    // The Log Viewer shows a Block Size of entries a page, newest first.
    let viewer = "/admin-console/app/logging/logviewer?file=Event%20%221%22.log";
    let page = f.get(viewer, cookie).into_string().unwrap();
    assert!(page.contains("Entry 249.") && page.contains("Entry 050."));
    assert!(!page.contains("Entry 049."));
    assert!(page.contains("11/11/2026 10:04:09"));
    let page = f
        .get(&format!("{viewer}&skip=200"), cookie)
        .into_string()
        .unwrap();
    assert!(page.contains("Entry 049.") && page.contains("Entry 000."));
    assert!(!page.contains("Entry 050."));

    // Download gives the file as text, oldest first, under a name a header
    // can carry; only a file the server lists.
    let download = "/admin-console/app/logging/logviewer/download?file=Event%20%221%22.log";
    let r = f.get(download, cookie);
    assert_eq!(r.status(), 200);
    assert_eq!(r.header("content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(
        r.header("content-disposition"),
        Some(r#"attachment; filename="Event__1_.log""#)
    );
    let text = r.into_string().unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 250);
    assert_eq!(
        lines[0],
        "2026-11-11 10:00:00.000 -0600\tInformation\t148\tserver\tEntry 000."
    );
    assert!(lines[249].ends_with("Entry 249."));
    let r = f.get(
        "/admin-console/app/logging/logviewer/download?file=..%2Fsecret.log",
        cookie,
    );
    assert_eq!(r.status(), 404);
    let r = f.get(download, None);
    assert_eq!((r.status(), location(&r)), (303, "/admin-console/signin"));

    // The live status says idle only while the backups can be read.
    let status = "/admin-console/fragments/backups/status";
    let region = f.get(status, cookie).into_string().unwrap();
    assert!(region.contains("No Backup Running"));
    f.backend.state().backups = None;
    let region = f.get(status, cookie).into_string().unwrap();
    assert!(region.contains("Not available yet") && !region.contains("No Backup Running"));
}

#[test]
fn client_actions_reach_the_server_only_with_valid_input() {
    let f = start();
    {
        let sample = MockState::sample();
        let mut state = f.backend.state();
        state.databases = sample.databases;
        state.clients = sample.clients;
    }
    let r = f.sign_in(PASSWORD);
    let cookie = r
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let cookie = Some(cookie.as_str());
    let page = f
        .get("/admin-console/app/activities", cookie)
        .into_string()
        .unwrap();
    assert!(page.contains("Sample User 1") && page.contains("Disconnect Client"));
    let token = csrf(&page);
    // The view's state comes back with the form, and the browser returns
    // to it.
    let back = "/admin-console/app/activities?db=1&csort=signin";
    let act = |fields: &[(&str, &str)]| {
        let mut form = vec![("csrf", token.as_str()), ("back", back)];
        form.extend_from_slice(fields);
        let r = f.post(
            "/admin-console/app/activities/clients",
            cookie,
            Some(&f.base),
            &form,
        );
        assert_eq!((r.status(), location(&r)), (303, back));
    };
    let flash = || f.get(back, cookie).into_string().unwrap();

    // Send Message reaches each checked client, with the browser's line
    // breaks as LF.
    act(&[
        ("client", "11"),
        ("client", "12"),
        ("send", "1"),
        ("send_message", "Saving now.\r\nBack soon."),
    ]);
    let sent = "Saving now.\nBack soon.".to_owned();
    assert_eq!(
        f.backend.state().messages,
        [(11, sent.clone()), (12, sent.clone())]
    );
    assert!(flash().contains("Sent the message to 2 clients."));
    // A message of only spaces, or over 200 characters, is refused.
    act(&[("client", "11"), ("send", "1"), ("send_message", "  \r\n ")]);
    assert!(flash().contains("Enter a message."));
    act(&[
        ("client", "11"),
        ("send", "1"),
        ("send_message", &"x".repeat(201)),
    ]);
    assert!(flash().contains("longer than 200 characters"));
    assert_eq!(f.backend.state().messages.len(), 2);

    // Disconnect with a message and a delay gives the server a notice with
    // the delay in seconds; a delay past an hour is refused.
    act(&[
        ("client", "12"),
        ("disconnect", "1"),
        ("disconnect_message", "Closing for the night."),
        ("disconnect_delay", "61"),
    ]);
    assert!(flash().contains("Enter a delay from 0 to 60 minutes."));
    act(&[
        ("client", "12"),
        ("disconnect", "1"),
        ("disconnect_message", "Closing for the night."),
        ("disconnect_delay", "5"),
    ]);
    {
        let state = f.backend.state();
        assert_eq!(
            state.notices,
            [Notice {
                message: "Closing for the night.".into(),
                grace: Duration::from_secs(300),
            }]
        );
        assert!(state.clients.iter().all(|c| c.id != 12));
    }
    // Without a message or a delay the client is disconnected at once.
    act(&[("client", "11"), ("disconnect", "1")]);
    assert_eq!(f.backend.state().notices.len(), 1);
    assert!(f.backend.state().clients.iter().all(|c| c.id != 11));

    // A live region is fetched with its page's query: with a file
    // selected, the clients show its account names.
    let region = f
        .get("/admin-console/fragments/activities/clients?db=1", cookie)
        .into_string()
        .unwrap();
    assert!(region.contains("Account Name") && region.contains("Sample User 3"));

    // Acknowledging an alert returns to the page with its view's state.
    let page = f.get(back, cookie).into_string().unwrap();
    let alert = &page[page.find("/admin-console/alerts/7/acknowledge").unwrap()..];
    let field = r#"name="back" value="/admin-console/app/activities?db=1&amp;csort=signin""#;
    assert!(alert[..alert.find("</form>").unwrap()].contains(field));
    let r = f.post(
        "/admin-console/alerts/7/acknowledge",
        cookie,
        Some(&f.base),
        &[("csrf", &token), ("back", back)],
    );
    assert_eq!((r.status(), location(&r)), (303, back));
}
