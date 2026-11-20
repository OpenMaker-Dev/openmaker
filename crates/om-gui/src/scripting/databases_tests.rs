//! Independent two-file desktop fixtures. Both files deliberately reuse all
//! native script/layout/table/record ids; only the owning session differs.
use super::*;
use crate::licensing::{LicenseKey, LicenseStore};
use crate::scripting::databases::OpenFiles;
use crate::scripting::{PortalIdentity, PortalRow, WindowState};
use om_data::calc::{Ctx, Date, Locale, ValueType};
use om_data::found::{FoundSet, FoundSetChange, MemoryFoundSet};
use om_data::host::{FieldDef, MemorySource, Privileges, RecordValues, Recorder};
use om_data::runtime::{OwnedNode, TransactionOptions};
use om_data::{
    file_refs::{FileDataSource, FilePath},
    *,
};
use std::collections::BTreeMap;
use std::sync::{mpsc, Mutex};

#[derive(Default)]
struct Server {
    refresh_render: Option<RenderLayout>,
    pdf_rows: Vec<i64>,
    /// The page setup the file stores (`[2, 87]`).
    page_format: Option<Vec<u8>>,
    report_grands: bool,
    /// A Region field (10) grouping the report rows, and the found set's
    /// sort field ids (empty: unsorted).
    report_groups: bool,
    report_sort: Vec<u64>,
    /// Street (11), Suite (12, empty for even ids) and Notes (13, `id % 4`
    /// lines) of each report row, for sliding address blocks.
    report_slides: bool,
    /// Signed (7), an unstored calculation of a `$$` and a `$` variable,
    /// and Queried (8), which reads a row's Signed through ExecuteSQL.
    report_variables: bool,
    scripts: BTreeMap<String, Vec<Script>>,
    rows: BTreeMap<String, String>,
    events: Vec<String>,
    deny: bool,
    fail_rows_after_refresh: bool,
    cancel: bool,
    startup: bool,
    /// Peer's opening layout names an external occurrence that its render
    /// resolves; its initial calculation schema still has it pending.
    peer_pending_table: bool,
    peer_table_loaded: bool,
    source_paths: Vec<String>,
    lose: bool,
    related: bool,
    /// Which file Root's import of Peer says it is
    /// (`FileSchemaContext::identity`): `None` an import that does not say.
    related_identity: Option<om_data::file_refs::FileIdentity>,
    peer_layout: Option<u32>,
    peer_wrong_table: bool,
    /// Peer's found-set changes fail (with 802).
    peer_found_fails: bool,
    closes: om_data::CloseRequests,
    triggers: BTreeMap<String, Vec<om_layout::ScriptTrigger>>,
    /// Both files' Rows have the container field Photo (20), empty;
    /// uploads into it are recorded as events.
    containers: bool,
    /// The custom set the account is held to, when not full access.
    set_access: Option<om_data::host::security::SetAccess>,
    /// A `Peer.fmp12` on another host, `other.invalid:5100`: the provider
    /// asks for an account for it (`fmnet:/` names, #976), and the
    /// password `there` opens it.
    other_host: bool,
    /// The window's record session is set aside for another window's
    /// transaction (#892): held, not open.
    session_apart: bool,
    /// While the window's commit or a computation entered its set-aside
    /// session (`enter_record_session`).
    session_entered: bool,
    /// What `compute_pending` answers for Text, only inside the window's
    /// own record session (#1104); `pending_panics`: it panics instead.
    pending_text: Option<String>,
    pending_panics: bool,
}
struct Provider {
    server: Arc<Mutex<Server>>,
    file: String,
    window: u32,
    working: Option<String>,
    lost: bool,
    found: BTreeMap<u32, MemoryFoundSet>,
    /// The other host and port a file signed in to there is on: its
    /// `fmnet` names there are its own host's files.
    there: Option<(String, u16)>,
}
impl Provider {
    fn new(server: Arc<Mutex<Server>>, file: &str) -> Self {
        Self {
            server,
            file: file.into(),
            window: 1,
            working: None,
            lost: false,
            found: BTreeMap::new(),
            there: None,
        }
    }
    fn value(&self) -> String {
        self.working.clone().unwrap_or_else(|| {
            self.server
                .lock()
                .unwrap()
                .rows
                .get(&self.file)
                .cloned()
                .unwrap_or_default()
        })
    }
    fn note(&self, event: &str) {
        self.server
            .lock()
            .unwrap()
            .events
            .push(format!("{}:{event}", self.file));
    }
    fn layout(&self) -> u32 {
        if self.file == "Peer.fmp12" {
            self.server.lock().unwrap().peer_layout.unwrap_or(1)
        } else {
            1
        }
    }
    fn schema(&self) -> Arc<CalcSchema> {
        let mut schema = CalcSchema::new(self.file.trim_end_matches(".fmp12"), Locale::en_us());
        schema.add_table(5, "Rows");
        schema.add_occurrence(5, "Rows", 5);
        schema.add_field(5, FieldDef::data(1, "Text", ValueType::Text));
        schema.authenticated_account.set_access = self.server.lock().unwrap().set_access.clone();
        schema.page_format = self.server.lock().unwrap().page_format.clone();
        if self.server.lock().unwrap().containers {
            schema.add_field(5, FieldDef::data(20, "Photo", ValueType::Container));
        }
        if !self.server.lock().unwrap().pdf_rows.is_empty() {
            schema.add_field(5, FieldDef::data(3, "Amount", ValueType::Number));
            let mut cx = om_data::formula::Context::new(schema.names());
            cx.occurrence = Some(5);
            let compiled =
                om_data::formula::compile("Text & \" / record \" & Get ( RecordNumber )", &cx)
                    .unwrap();
            let formula = om_data::formula::Formula {
                context: None,
                formatted: Some(compiled.tokens),
                polish: compiled.expr,
            };
            schema.add_field(
                5,
                FieldDef::calculation(4, "Caption", ValueType::Text, formula, false),
            );
        }
        if !self.server.lock().unwrap().pdf_rows.is_empty() {
            let cx = om_data::formula::Context::new(schema.names());
            let compiled = om_data::formula::compile("Get ( PageNumber )", &cx).unwrap();
            schema.add_field(
                5,
                FieldDef::calculation(
                    6,
                    "Page",
                    ValueType::Number,
                    om_data::formula::Formula {
                        context: None,
                        formatted: Some(compiled.tokens),
                        polish: compiled.expr,
                    },
                    false,
                ),
            );
        }
        if self.server.lock().unwrap().report_grands {
            pdf_tests::add_grand_fields(&mut schema);
        }
        if self.server.lock().unwrap().report_variables {
            for (id, name, source) in [
                (7, "Signed", "$$signer & \" / \" & $who"),
                (
                    8,
                    "Queried",
                    r#"GetValue ( ExecuteSQL ( "SELECT \"Signed\" FROM \"Rows\"" ; "" ; "" ) ; 1 )"#,
                ),
            ] {
                let mut cx = om_data::formula::Context::new(schema.names());
                cx.occurrence = Some(5);
                let compiled = om_data::formula::compile(source, &cx).unwrap();
                let formula = om_data::formula::Formula {
                    context: None,
                    formatted: Some(compiled.tokens),
                    polish: compiled.expr,
                };
                schema.add_field(
                    5,
                    FieldDef::calculation(id, name, ValueType::Text, formula, false),
                );
            }
        }
        if self.server.lock().unwrap().report_groups {
            schema.add_field(5, FieldDef::data(10, "Region", ValueType::Text));
        }
        if self.server.lock().unwrap().report_slides {
            for (id, name) in [(11, "Street"), (12, "Suite"), (13, "Notes")] {
                schema.add_field(5, FieldDef::data(id, name, ValueType::Text));
            }
        }
        schema.add_field(
            5,
            FieldDef {
                global: true,
                ..FieldDef::data(2, "Setting", ValueType::Text)
            },
        );
        let pending_peer =
            self.file == "Peer.fmp12" && self.server.lock().unwrap().peer_pending_table;
        schema.add_layout(
            self.layout(),
            "Entry",
            Some(if pending_peer { 90 } else { 5 }),
        );
        if pending_peer {
            schema.add_external_occurrence(om_data::host::ExternalOccurrence {
                id: 90,
                name: "Related Rows".into(),
                source: "Related source".into(),
            });
            schema.set_pending(90);
            if self.server.lock().unwrap().peer_table_loaded {
                schema.add_table(6, "Related Rows");
                schema.add_field(6, FieldDef::data(1, "Text", ValueType::Text));
                schema.bind_external_occurrence(90, 6);
            }
        }
        if self.server.lock().unwrap().peer_wrong_table && self.file == "Peer.fmp12" {
            schema.add_table(6, "Other");
            schema.add_occurrence(6, "Other", 6);
        }
        if self.file == "Root.fmp12" && self.server.lock().unwrap().related {
            let peer = Provider::new(self.server.clone(), "Peer.fmp12").schema();
            let identity = self.server.lock().unwrap().related_identity.clone();
            let map = schema.import_owned_file_schema(&peer, None, identity);
            schema.add_external_occurrence(om_data::host::ExternalOccurrence {
                id: 90,
                name: "Remote Rows".into(),
                source: "Peer source".into(),
            });
            schema.bind_external_occurrence(90, map.tables[&5]);
            schema.add_relationship(om_data::host::RelationshipDef::new(
                1,
                5,
                90,
                vec![om_data::host::JoinPredicate {
                    op: om_data::host::JoinOp::Cartesian,
                    left: 1,
                    right: 1,
                }],
            ));
        }
        if self.file == "Peer.fmp12" && self.server.lock().unwrap().startup {
            schema.file_options = Some(om_data::meta::FileOptions {
                first_window_open: Some(2),
                window_open: Some(3),
                window_transaction: pending_peer.then_some(4),
                ..Default::default()
            });
        }
        Arc::new(schema)
    }
}
impl DataProvider for Provider {
    fn open(&mut self, c: &Connection) -> Result<(), DataError> {
        self.file = c.file.clone();
        Ok(())
    }
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![])
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: self.layout(),
            name: "Entry".into(),
        }])
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(ScriptCatalog {
            scripts: self
                .server
                .lock()
                .unwrap()
                .scripts
                .get(&self.file)
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        })
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        let server = self.server.lock().unwrap();
        if server.fail_rows_after_refresh && server.events.iter().any(|e| e.contains(":refresh:")) {
            return Err(DataError::new("synthetic refresh row read failure"));
        }
        let containers = server.containers;
        drop(server);
        let mut fields = vec![Field {
            name: "Text".into(),
            kind: FieldKind::Text,
        }];
        if containers {
            fields.push(Field {
                name: "Photo".into(),
                kind: FieldKind::Container,
            });
        }
        let rows = self.server.lock().unwrap().pdf_rows.clone();
        if !rows.is_empty() {
            self.note(&format!("records:{offset}:{limit}"));
        }
        let all = if rows.is_empty() {
            let mut values = vec![Some(self.value())];
            if containers {
                values.push(None);
            }
            vec![Record { id: 1, values }]
        } else {
            rows.iter()
                .map(|id| Record {
                    id: *id,
                    values: vec![Some(format!("Row {id:02} - café"))],
                })
                .collect()
        };
        Ok(self
            .found
            .entry(self.window)
            .or_default()
            .page(fields, &all, offset, limit))
    }
    fn can_write_records(&self) -> bool {
        true
    }
    fn delete_record(&mut self, _: u32, record: i64) -> Result<(), DataError> {
        if !self.server.lock().unwrap().peer_pending_table {
            return Err(DataError::new(
                "This provider does not support deleting records.",
            ));
        }
        self.server.lock().unwrap().rows.remove(&self.file);
        self.note(&format!("delete:{record}"));
        Ok(())
    }
    fn container_reference(
        &mut self,
        _: u32,
        field: &str,
        value: &om_data::calc::Container,
    ) -> Result<Option<String>, DataError> {
        let name = value.text.as_str().to_owned();
        self.note(&format!("upload:{field}:{name}:{}", value.data().len()));
        Ok(Some(om_data::containers::reference_text(&[1])))
    }
    fn adjust_container(&mut self, _: u32, reference: &str, delta: i32) -> Result<(), DataError> {
        self.note(&format!(
            "adjust:{}:{delta}",
            reference.trim_end_matches('\u{e000}')
        ));
        Ok(())
    }
    fn update_record(&mut self, _: u32, _: i64, fields: FieldChanges) -> Result<Record, DataError> {
        let value = fields
            .get("Text")
            .cloned()
            .flatten()
            .unwrap_or_else(|| self.value());
        if let Some(working) = &mut self.working {
            *working = value.clone();
        } else {
            self.server
                .lock()
                .unwrap()
                .rows
                .insert(self.file.clone(), value.clone());
        }
        self.note("write");
        Ok(Record {
            id: 1,
            values: vec![Some(value)],
        })
    }
    fn refresh_window(&mut self, joins: bool, external: bool) -> Result<(), DataError> {
        self.note(&format!("refresh:{joins}:{external}"));
        Ok(())
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        Ok(Some(self.schema()))
    }
    fn calc_data(&mut self, needs: &[om_data::host::CalcNeed]) -> Result<CalcData, DataError> {
        let schema = self.schema();
        let mut source = MemorySource::new();
        let mut row = RecordValues::default();
        row.set(1, 1, Value::text(self.value()));
        source.put_record(5, 1, row);
        let rows = self.server.lock().unwrap().pdf_rows.clone();
        if !rows.is_empty() {
            for &id in &rows {
                let mut row = RecordValues::default();
                row.set(1, 1, Value::text(format!("Row {id:02} - café")));
                row.set(3, 1, Value::number(id * 125));
                row.set(10, 1, Value::text(pdf_tests::region(id)));
                let (street, suite, notes) = pdf_tests::address(id);
                row.set(11, 1, Value::text(street));
                row.set(12, 1, Value::text(suite));
                row.set(13, 1, Value::text(notes));
                source.put_record(5, id as u64, row);
            }
            let sort = self.server.lock().unwrap().report_sort.clone();
            source.set_found_set(
                5,
                &rows.iter().map(|id| *id as u64).collect::<Vec<_>>(),
                Some(&sort),
            );
        }
        if let Some(table) = schema.table_of(90) {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text("peer row"));
            source.put_record(table.id, 1, row);
        }
        let mut globals = RecordValues::default();
        globals.set(2, 1, Value::text(format!("{}-setting", self.file)));
        source.set_globals(5, globals);
        let recorder = Recorder::new(&source);
        let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 1, 1).unwrap());
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(e.to_string()))?;
        }
        Ok(recorder.take())
    }
    fn render_layout(&mut self, _: u32) -> Result<Option<RenderLayout>, DataError> {
        let pending_peer =
            self.file == "Peer.fmp12" && self.server.lock().unwrap().peer_pending_table;
        if pending_peer {
            self.server.lock().unwrap().peer_table_loaded = true;
        }
        if let Some(render) = &self.server.lock().unwrap().refresh_render {
            return Ok(Some(render.clone()));
        }
        if !self.server.lock().unwrap().pdf_rows.is_empty() {
            return Ok(Some(pdf_tests::document()));
        }
        let triggers = self
            .server
            .lock()
            .unwrap()
            .triggers
            .get(&self.file)
            .cloned()
            .unwrap_or_default();
        Ok(Some(RenderLayout {
            layout: om_layout::Layout {
                id: self.layout().into(),
                name: "Entry".into(),
                triggers,
                def: Some(om_layout::LayoutDef {
                    table_alias_key: if pending_peer {
                        90
                    } else if self.file == "Peer.fmp12"
                        && self.server.lock().unwrap().peer_wrong_table
                    {
                        6
                    } else {
                        5
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
            field_bindings: Default::default(),
        }))
    }
    fn use_window(&mut self, window: u32) {
        self.window = window;
    }
    fn change_found_set(&mut self, _: u32, change: FoundSetChange) -> Result<FoundSet, DataError> {
        if self.file == "Peer.fmp12" && self.server.lock().unwrap().peer_found_fails {
            return Err(DataError::with_code(
                802,
                "The found set could not be read.",
            ));
        }
        self.note(&format!("found:{}:{change:?}", self.window));
        let all = vec![Record {
            id: 1,
            values: vec![Some(self.value())],
        }];
        self.found.entry(self.window).or_default().apply(
            &[Field {
                name: "Text".into(),
                kind: FieldKind::Text,
            }],
            &all,
            change,
        )
    }
    fn database_sources(&mut self) -> Result<Vec<FileDataSource>, DataError> {
        let target = if self.file == "Root.fmp12" {
            "Peer"
        } else {
            "Root"
        };
        Ok(vec![FileDataSource {
            id: 129,
            name: "Peer source".into(),
            paths: {
                let paths = self.server.lock().unwrap().source_paths.clone();
                let paths = if paths.is_empty() {
                    vec![target.into()]
                } else {
                    paths
                };
                paths
                    .into_iter()
                    .map(|name| {
                        // An `fmnet:/` path is one to a host (#991).
                        FilePath::calculated(&name)
                            .filter(|path| path.scheme == "fmnet")
                            .unwrap_or(FilePath {
                                scheme: "file".into(),
                                location: "".into(),
                                name,
                            })
                    })
                    .collect()
            },
        }])
    }
    fn open_peer(&mut self, name: &str) -> Result<Box<dyn DataProvider>, DataError> {
        // Another host's file refuses the account this file was opened
        // with; a file there opens that host's other files.
        let own;
        let name = match om_data::file_refs::fmnet_target(name) {
            Some((host, port, file)) if self.there == Some((host.clone(), port)) => {
                own = om_data::file_refs::hosted_name(&file);
                own.as_str()
            }
            Some(_) => {
                self.note(&format!("ask:{name}"));
                return Err(DataError::with_code(212, "Synthetic peer on another host."));
            }
            None => name,
        };
        self.note(&format!("open:{name}"));
        let server = self.server.lock().unwrap();
        if server.deny {
            return Err(DataError::with_code(212, "Denied by synthetic peer."));
        }
        if !server.scripts.contains_key(name) {
            return Err(DataError::with_code(100, "Missing synthetic file."));
        }
        if server.cancel {
            server.closes.queued();
        }
        let lost = server.lose;
        drop(server);
        let mut peer = Provider::new(self.server.clone(), name);
        peer.lost = lost;
        peer.there = self.there.clone();
        Ok(Box::new(peer))
    }
    fn open_peer_as(
        &mut self,
        name: &str,
        account: &str,
        password: &str,
    ) -> Result<Box<dyn DataProvider>, DataError> {
        let Some((host, port, file)) = om_data::file_refs::fmnet_target(name) else {
            return Err(DataError::with_code(
                3,
                "Synthetic sign-ins are for other hosts.",
            ));
        };
        self.note(&format!("open-as:{host}:{port}:{file}:{account}"));
        if !self.server.lock().unwrap().other_host || password != "there" {
            return Err(DataError::with_code(212, "Refused by the synthetic host."));
        }
        if om_data::file_refs::file_stem(&file) != "Peer" {
            return Err(DataError::with_code(100, "Missing synthetic file."));
        }
        let mut peer = Provider::new(self.server.clone(), "Peer.fmp12");
        peer.there = Some((host, port));
        Ok(Box::new(peer))
    }
    fn peer_refused(&self, name: &str, credentials: Option<(&str, &str)>) -> bool {
        self.server.lock().unwrap().other_host
            && om_data::file_refs::fmnet_target(name).is_some()
            && credentials.is_none_or(|(_, password)| password != "there")
    }
    fn session_account(&self) -> Option<String> {
        Some(format!("{}-account", self.file))
    }
    fn account_privileges(&self) -> Option<Privileges> {
        Some(Privileges {
            account_set_name: format!("{}-set", self.file),
            ..Default::default()
        })
    }
    fn close_requests(&self) -> Option<om_data::CloseRequests> {
        Some(self.server.lock().unwrap().closes.clone())
    }
    fn connection_lost(&self) -> Option<DataError> {
        self.lost
            .then(|| DataError::connection_lost("Synthetic loss"))
    }
    fn begin_transaction(&mut self, _: TransactionOptions) -> Result<(), DataError> {
        self.working = Some(self.value());
        self.note("begin");
        Ok(())
    }
    fn commit_transaction(&mut self, _: bool) -> Result<(), DataError> {
        if let Some(value) = self.working.take() {
            self.server
                .lock()
                .unwrap()
                .rows
                .insert(self.file.clone(), value);
        }
        self.note("commit");
        Ok(())
    }
    fn revert_transaction(&mut self) -> Result<(), DataError> {
        self.working = None;
        self.note("revert");
        Ok(())
    }
    fn record_session_held(&self) -> bool {
        self.server.lock().unwrap().session_apart
    }
    fn enter_record_session(&mut self) -> bool {
        let mut server = self.server.lock().unwrap();
        server.session_entered = server.session_apart;
        server.session_entered
    }
    fn leave_record_session(&mut self) {
        self.server.lock().unwrap().session_entered = false;
    }
    fn compute_pending(
        &mut self,
        _write: &om_data::ScriptWrite,
        _others: &[om_data::PendingRecord],
    ) -> Result<Option<om_data::PendingValues>, DataError> {
        let server = self.server.lock().unwrap();
        if server.pending_panics {
            drop(server);
            panic!("a computation that panics");
        }
        Ok(server.session_entered.then(|| om_data::PendingValues {
            values: [(1, server.pending_text.clone())].into(),
            unknown: Default::default(),
        }))
    }
    fn close(&mut self) {
        self.note("close");
        self.working = None;
    }
}

#[path = "container_dialog_tests.rs"]
mod container_dialog_tests;
#[path = "page_setup_tests.rs"]
mod page_setup_tests;
#[path = "pdf_tests.rs"]
mod pdf_tests;
#[path = "plugin_files_tests.rs"]
mod plugin_files_tests;
#[path = "print_tests.rs"]
mod print_tests;
struct Store;
impl LicenseStore for Store {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
        Ok(None)
    }
    fn save(&mut self, _: &LicenseKey) -> Result<(), DataError> {
        Ok(())
    }
    fn remove(&mut self) -> Result<(), DataError> {
        Ok(())
    }
}
fn calc(text: &str) -> Calculation {
    let names = Provider::new(Arc::default(), "Root.fmp12")
        .schema()
        .names()
        .clone();
    let mut context = om_data::formula::Context::new(&names);
    context.occurrence = Some(5);
    let compiled = om_data::formula::compile(text, &context).unwrap();
    Calculation::new(None, om_data::formula::polish::encode(&compiled.expr))
}
fn var(name: &str, expression: &str) -> ScriptStep {
    let mut s = ScriptStep::new(141);
    s.set_text(0, Some(name));
    s.set_calculation(1, Some(&calc(expression)));
    s
}
fn open(path: &str, hidden: bool) -> ScriptStep {
    let mut s = ScriptStep::new(33);
    let expression = path
        .split(['\n', '\r'])
        .map(|s| format!("{s:?}"))
        .collect::<Vec<_>>()
        .join(" & Char ( 13 ) & ");
    s.set_calculation(0, Some(&calc(&expression)));
    if hidden {
        s.flags |= flag::OPTION;
    }
    s
}
fn close(path: &str) -> ScriptStep {
    let mut s = open(path, false);
    s.opcode = 34;
    s
}
fn call(name: &str) -> ScriptStep {
    let mut s = ScriptStep::new(1);
    s.set_calculation(1, Some(&calc(&format!("{name:?}"))));
    s
}
fn exit(expression: &str) -> ScriptStep {
    let mut s = ScriptStep::new(103);
    s.set_calculation(0, Some(&calc(expression)));
    s
}
fn set(expression: &str) -> ScriptStep {
    let mut s = ScriptStep::new(147);
    s.set_calculation(1, Some(&calc("\"Text\"")));
    s.set_calculation(0, Some(&calc(expression)));
    s
}
fn script(mut steps: Vec<ScriptStep>) -> Script {
    let mut capture = ScriptStep::new(86);
    capture.flags |= flag::TURN_ON;
    steps.insert(0, capture);
    Script {
        id: 1,
        name: "Run".into(),
        steps,
        ..Default::default()
    }
}
struct Client {
    session: ScriptSession,
    provider: Box<dyn DataProvider>,
    files: OpenFiles,
    server: Arc<Mutex<Server>>,
    _keep_jobs: Sender<Job>,
    jobs: Receiver<Job>,
    responses: Sender<Response>,
    events: Receiver<Response>,
}
impl Client {
    fn new(root: Vec<ScriptStep>, peer: Vec<ScriptStep>) -> Self {
        let server = Arc::new(Mutex::new(Server {
            scripts: BTreeMap::from([
                ("Root.fmp12".into(), vec![script(root)]),
                ("Peer.fmp12".into(), vec![script(peer)]),
            ]),
            rows: BTreeMap::from([
                ("Root.fmp12".into(), "root row".into()),
                ("Peer.fmp12".into(), "peer row".into()),
            ]),
            ..Default::default()
        }));
        let (keep_jobs, jobs) = mpsc::channel();
        let (responses, events) = mpsc::channel();
        let mut session = ScriptSession::new(Arc::new(AtomicBool::new(false)));
        let mut options = crate::scripting::host::options();
        options.step_budget = Some(1000);
        session.runtime = Runtime::new(options);
        Self {
            session,
            provider: Box::new(Provider::new(server.clone(), "Root.fmp12")),
            files: OpenFiles::default(),
            server,
            _keep_jobs: keep_jobs,
            jobs,
            responses,
            events,
        }
    }
    fn run(&mut self) {
        self.run_in(WindowState {
            layout: Some(1),
            record: Some(1),
            ..Default::default()
        });
    }
    fn run_in(&mut self, window: WindowState) {
        self.start_in(window, Start::Script { id: 1 });
    }
    fn start_in(&mut self, window: WindowState, start: Start) {
        self.start_with(window, Vec::new(), start);
    }
    fn start_with(&mut self, window: WindowState, others: Vec<WindowState>, start: Start) {
        let mut store = Store;
        let mut io = Io {
            provider: &mut self.provider,
            files: &mut self.files,
            store: &mut store,
            jobs: &self.jobs,
            responses: &self.responses,
        };
        let file = self.session.s.file.context.file.clone();
        self.session.handle(
            &mut io,
            Job {
                epoch: 1,
                window: window.id,
                ctx: Default::default(),
                request: Request::Script(Box::new(ScriptJob {
                    start,
                    window,
                    others,
                    context: SessionContext {
                        host: "host.invalid".into(),
                        file: if file.is_empty() {
                            "Root.fmp12".into()
                        } else {
                            file
                        },
                        account: "root-account".into(),
                        local: None,
                    },
                })),
            },
        );
    }
    fn result(&self) -> String {
        self.session
            .runtime
            .final_result()
            .map(|v| {
                v.to_text(&Ctx::new(
                    &Locale::en_us(),
                    Date::from_ymd(2026, 1, 1).unwrap(),
                ))
                .as_str()
                .to_owned()
            })
            .unwrap_or_default()
    }
    fn events(&mut self) -> Vec<ScriptEvent> {
        self.events
            .try_iter()
            .filter_map(|r| match r.reply {
                worker::Reply::Script(event) => Some(event),
                _ => None,
            })
            .collect()
    }
}

/// Commit Transaction run from a window that holds its new record apart
/// from another window's transaction (#892) leaves that record and the
/// user's typing in it alone when the commit fails (script-runtime.md 8.5,
/// #1103). Here the provider cannot enter the window's set-aside session,
/// so the record's own commit is refused (3) and the transaction reverted.
#[test]
fn a_failed_commit_transaction_keeps_a_window_holding_its_record_apart() {
    let mut capture = ScriptStep::new(86);
    capture.flags |= om_data::flag::TURN_ON;
    let mut select = ScriptStep::new(123);
    select.value = 1;
    select.flags |= om_data::flag::MENU_SELECTION;
    select.set_calculation(0, Some(&calc("\"Tasks\"")));
    let mut c = Client::new(
        vec![capture, ScriptStep::new(205), select, ScriptStep::new(206)],
        vec![],
    );
    c.server.lock().unwrap().session_apart = true;
    let transaction_window = WindowState {
        id: 1,
        title: Some("W2".into()),
        layout: Some(1),
        record: Some(1),
        ..Default::default()
    };
    let mut held = WindowState {
        id: 2,
        title: Some("Tasks".into()),
        layout: Some(1),
        record: Some(1),
        ..Default::default()
    };
    held.uncommitted.session = true;
    held.drafts.insert("Name".into(), Some("Typed".into()));
    c.start_with(transaction_window, vec![held], Start::Script { id: 1 });
    let events = c.server.lock().unwrap().events.clone();
    assert!(events.iter().any(|e| e.ends_with("revert")), "{events:?}");
    let file = &c.session.s.file;
    assert_eq!(
        file.window.state.title.as_deref(),
        Some("Tasks"),
        "the script went on in Tasks"
    );
    let tasks = std::iter::once(&file.window)
        .chain(file.others.iter())
        .find(|w| w.state.title.as_deref() == Some("Tasks"))
        .expect("the Tasks window");
    assert_eq!(tasks.state.drafts.get("Name"), Some(&Some("Typed".into())));
    assert!(tasks.state.uncommitted.session, "the record is still held");
}

/// A window that holds its new record apart from another window's
/// transaction (#892) reads the record's computed values from its own
/// record session, where its commit writes it, not from the other
/// transaction (#1104).
#[test]
fn a_record_held_apart_is_computed_in_its_own_session() {
    let mut c = Client::new(vec![exit("Rows::Text")], vec![]);
    {
        let mut server = c.server.lock().unwrap();
        server.session_apart = true;
        server.pending_text = Some("computed".into());
    }
    let mut window = WindowState {
        layout: Some(1),
        record: Some(1),
        ..Default::default()
    };
    window.uncommitted.session = true;
    // A script's change of another cell: the record has unsaved changes.
    window.uncommitted.cells.insert(9, Some("changed".into()));
    c.run_in(window.clone());
    assert_eq!(c.result(), "computed");
    assert!(!c.server.lock().unwrap().session_entered, "left again");
    // A computation that panics leaves the session too: the other
    // transaction is not stranded behind it.
    let mut c = Client::new(vec![exit("Rows::Text")], vec![]);
    {
        let mut server = c.server.lock().unwrap();
        server.session_apart = true;
        server.pending_panics = true;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.run_in(window)));
    assert!(
        !c.server.lock().unwrap().session_entered,
        "left after a panic"
    );
}

#[test]
fn desktop_open_file_keeps_caller_context_and_returns_from_the_peer() {
    let mut c = Client::new(vec![var("$$mark", "\"root\""), open("Peer", false), call("Peer source::Run"), exit("Get ( ScriptResult ) & \"|\" & Get ( FileName ) & \"|\" & $$mark & \"|\" & Text")], vec![var("$$mark", "\"peer\""), exit("Get ( FileName ) & \"|\" & Get ( AccountName ) & \"|\" & Get ( AccountPrivilegeSetName ) & \"|\" & $$mark & \"|\" & Setting")]);
    c.run();
    assert_eq!(
        c.result(),
        "Peer|Peer.fmp12-account|Peer.fmp12-set|peer|Peer.fmp12-setting|Root|root|root row"
    );
    assert_eq!(c.files.peers.len(), 1);
    assert_eq!(c.session.s.file.id, 0);
    assert!(c
        .events()
        .iter()
        .any(|e| matches!(e, ScriptEvent::Finished { message: None, .. })));
}

#[test]
fn desktop_open_file_reuses_hidden_peers_and_resolves_a_cycle_to_root() {
    let mut c = Client::new(
        vec![
            open("Peer source", true),
            open("peer.FMP12", true),
            call("Peer source::Run"),
            exit("Get ( ScriptResult )"),
        ],
        vec![open("Peer source", true), exit("Get ( FileName )")],
    );
    c.run();
    assert_eq!(c.result(), "Peer");
    assert_eq!(
        c.server
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|s| s.contains(":open:"))
            .count(),
        1
    );
    assert_eq!(c.files.peers.len(), 1);
    assert!(c.files.peers.values().all(|f| f.state.window.state.hidden));
}

#[test]
fn desktop_open_file_preserves_a_dirty_callers_window_and_transaction() {
    let mut begin = ScriptStep::new(205);
    begin.value = 5;
    let mut commit = ScriptStep::new(206);
    commit.value = 5;
    let mut c = Client::new(
        vec![
            begin,
            open("Peer", false),
            call("Peer source::Run"),
            set("\"committed root\""),
            commit,
            exit("Get ( TransactionOpenState ) & \"|\" & Text"),
        ],
        vec![exit("Text")],
    );
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(1),
        drafts: BTreeMap::from([("Text".into(), Some("dirty root".into()))]),
        ..Default::default()
    });
    assert_eq!(c.result(), "0|committed root");
    assert_eq!(
        c.server.lock().unwrap().rows["Root.fmp12"],
        "committed root"
    );
    assert_eq!(c.server.lock().unwrap().rows["Peer.fmp12"], "peer row");
    assert!(
        !c.server
            .lock()
            .unwrap()
            .events
            .iter()
            .any(|s| s.starts_with("Peer.fmp12:")
                && (s.ends_with(":commit") || s.ends_with(":write")))
    );
}

#[test]
fn desktop_open_file_paths_try_alternatives_without_following_other_hosts() {
    let mut c = Client::new(
        vec![
            open(
                "fmnet:/elsewhere.invalid/Peer\nfile:Missing\nfmnet:/host.invalid/Peer",
                false,
            ),
            exit("Get ( LastError )"),
        ],
        vec![],
    );
    c.run();
    assert_eq!(c.result(), "0");
    assert_eq!(
        c.server
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|s| s.contains(":open:"))
            .cloned()
            .collect::<Vec<_>>(),
        [
            "Root.fmp12:open:Missing.fmp12",
            "Root.fmp12:open:Peer.fmp12"
        ]
    );
}

/// A script's Open File of a file on another host (#976): the data
/// source's `fmnet` path names another host and port, so the provider is
/// asked for it as `fmnet:/host:port/Name` and refuses it without contact;
/// Open File asks for an account, which opens it there. The new file is
/// that host's (its context and endpoint), and the file of the same name
/// on this host, opened next, is another file.
#[test]
fn open_file_on_another_host_asks_for_an_account_and_keeps_the_files_apart() {
    let mut c = Client::new(
        vec![
            open("fmnet:/other.invalid:5100/Peer", false),
            var("$there", "Get ( LastError )"),
            open("Peer", false),
            exit("$there & \"|\" & Get ( LastError )"),
        ],
        vec![],
    );
    c.server.lock().unwrap().other_host = true;
    c.session.s.file.endpoint = Some(crate::scripting::session::Endpoint {
        host: "host.invalid".into(),
        port: 5003,
        file: "Root.fmp12".into(),
        account: "root-account".into(),
        local: None,
    });
    let events = std::mem::replace(&mut c.events, mpsc::channel().1);
    let jobs = c._keep_jobs.clone();
    let asked = std::thread::spawn(move || {
        let mut asked = Vec::new();
        while let Ok(response) = events.recv_timeout(std::time::Duration::from_secs(30)) {
            match response.reply {
                worker::Reply::Script(ScriptEvent::Prompt {
                    id,
                    prompt: crate::scripting::Prompt::SignIn { file, .. },
                }) => {
                    asked.push(file);
                    jobs.send(Job {
                        epoch: 1,
                        window: 1,
                        ctx: Default::default(),
                        request: Request::Answer {
                            prompt: id,
                            answer: crate::scripting::Answer::SignIn(Some(
                                crate::scripting::SignIn {
                                    account: "there-account".into(),
                                    password: zeroize::Zeroizing::new("there".into()),
                                },
                            )),
                        },
                    })
                    .unwrap();
                }
                worker::Reply::Script(ScriptEvent::Finished { .. }) => break,
                _ => {}
            }
        }
        asked
    });
    c.run();
    assert_eq!(c.result(), "0|0");
    assert_eq!(asked.join().unwrap(), ["fmnet:/other.invalid:5100/Peer"]);
    let peers: Vec<_> = c.files.peers.values().map(|f| &f.state).collect();
    assert_eq!(peers.len(), 2, "two files named Peer");
    let there = peers
        .iter()
        .find(|state| state.context.host == "other.invalid")
        .expect("the other host's Peer");
    assert_eq!(there.context.file, "Peer");
    let endpoint = there.endpoint.as_ref().unwrap();
    assert_eq!(
        (endpoint.host.as_str(), endpoint.port),
        ("other.invalid", 5100)
    );
    let here = peers
        .iter()
        .find(|state| state.context.host == "host.invalid")
        .expect("this host's Peer");
    assert_eq!(here.endpoint.as_ref().unwrap().port, 5003);
    let events = c.server.lock().unwrap().events.clone();
    assert!(
        events
            .iter()
            .any(|e| e.ends_with("open-as:other.invalid:5100:Peer:there-account")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| e.ends_with("open:Peer.fmp12")),
        "{events:?}"
    );
}

/// A file of another host open already (signed in to there): a script
/// first needing another file of that host opens it through that file,
/// which sends it the account that host admitted, with no second prompt
/// and none of the calling file's account.
#[test]
fn another_file_of_a_host_signed_in_to_opens_through_the_open_one() {
    let mut c = Client::new(
        vec![
            open("fmnet:/other.invalid:5100/Peer", false),
            call("Peer source::Run"),
            exit("Get ( LastError ) & \"|\" & Get ( ScriptResult )"),
        ],
        vec![],
    );
    {
        let mut server = c.server.lock().unwrap();
        server.other_host = true;
        server.source_paths = vec!["fmnet:/other.invalid:5100/Other".into()];
        server.scripts.insert(
            "Other.fmp12".into(),
            vec![script(vec![exit("Get ( FileName )")])],
        );
        server.rows.insert("Other.fmp12".into(), "other row".into());
    }
    c.session.s.file.endpoint = Some(crate::scripting::session::Endpoint {
        host: "host.invalid".into(),
        port: 5003,
        file: "Root.fmp12".into(),
        account: "root-account".into(),
        local: None,
    });
    let events = std::mem::replace(&mut c.events, mpsc::channel().1);
    let jobs = c._keep_jobs.clone();
    let asked = std::thread::spawn(move || {
        let mut asked = Vec::new();
        while let Ok(response) = events.recv_timeout(std::time::Duration::from_secs(30)) {
            match response.reply {
                worker::Reply::Script(ScriptEvent::Prompt {
                    id,
                    prompt: crate::scripting::Prompt::SignIn { file, .. },
                }) => {
                    asked.push(file);
                    jobs.send(Job {
                        epoch: 1,
                        window: 1,
                        ctx: Default::default(),
                        request: Request::Answer {
                            prompt: id,
                            answer: crate::scripting::Answer::SignIn(Some(
                                crate::scripting::SignIn {
                                    account: "there-account".into(),
                                    password: zeroize::Zeroizing::new("there".into()),
                                },
                            )),
                        },
                    })
                    .unwrap();
                }
                worker::Reply::Script(ScriptEvent::Finished { .. }) => break,
                _ => {}
            }
        }
        asked
    });
    c.run();
    assert_eq!(c.result(), "0|Other");
    assert_eq!(asked.join().unwrap(), ["fmnet:/other.invalid:5100/Peer"]);
    let events = c.server.lock().unwrap().events.clone();
    assert!(
        events.contains(&"Peer.fmp12:open:Other.fmp12".to_owned()),
        "through the open file: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("Root.fmp12:ask:fmnet:/other.invalid:5100/Other")),
        "{events:?}"
    );
    let other = c
        .files
        .peers
        .values()
        .find(|f| f.state.context.file == "Other")
        .expect("Other open");
    let endpoint = other.state.endpoint.as_ref().unwrap();
    assert_eq!(
        (endpoint.host.as_str(), endpoint.port),
        ("other.invalid", 5100)
    );
}

/// Perform Script in a file on another host as its first use, no Open File
/// before it (#991): the data source names the file by an `fmnet` path to
/// another host and port, so the call asks for an account (the sign-in
/// Open File uses), opens the file there with it, runs the script in it
/// and returns to the caller, whose file, variables and open transaction
/// are as they were. Cancel at that prompt fails the call: by name with
/// 104, as for any source a name does not resolve (script-runtime.md
/// 13.16), and the caller goes on; by stored file reference with 1, a user
/// cancel, which the caller's open transaction records, so its next steps
/// skip to Commit Transaction, which reverts (script-runtime.md 8.4). An
/// account the host accepts for a file it does not have moves on to the
/// source's next path, as for any path.
#[test]
fn perform_script_in_a_file_on_another_host_asks_for_an_account_first() {
    let stored = || {
        let mut step = ScriptStep::new(1);
        step.set_script_key(StoredKey::from_id(1));
        step.set_data_leaf(32, Some(OwnedNode::leaf([32], [2, 0x80, 0x01])));
        step
    };
    let there = "fmnet:/other.invalid:5100/Peer";
    let gone = "fmnet:/other.invalid:5100/Gone";
    let ran = "0|Peer|Root|root|1|0";
    let other = Some(("other.invalid", 5100));
    for (step, paths, answer, expected, opened) in [
        (call("Peer source::Run"), &[there][..], true, ran, other),
        (stored(), &[there], true, ran, other),
        (
            call("Peer source::Run"),
            &[there],
            false,
            "104||Root|root|1|0",
            None,
        ),
        (stored(), &[there], false, "|1", None),
        (
            stored(),
            &[gone, "Peer"],
            true,
            ran,
            Some(("host.invalid", 5003)),
        ),
    ] {
        let mut begin = ScriptStep::new(205);
        begin.value = 5;
        let mut commit = ScriptStep::new(206);
        commit.value = 5;
        let mut c = Client::new(
            vec![
                begin,
                var("$$mark", "\"root\""),
                step,
                var(
                    "$after",
                    "Get ( LastError ) & \"|\" & Get ( ScriptResult ) & \"|\" & Get ( FileName ) & \"|\" & $$mark & \"|\" & Get ( TransactionOpenState )",
                ),
                commit,
                exit("$after & \"|\" & Get ( LastError )"),
            ],
            vec![var("$$mark", "\"peer\""), exit("Get ( FileName )")],
        );
        {
            let mut server = c.server.lock().unwrap();
            server.other_host = true;
            server.source_paths = paths.iter().map(|path| (*path).into()).collect();
        }
        c.session.s.file.endpoint = Some(crate::scripting::session::Endpoint {
            host: "host.invalid".into(),
            port: 5003,
            file: "Root.fmp12".into(),
            account: "root-account".into(),
            local: None,
        });
        let events = std::mem::replace(&mut c.events, mpsc::channel().1);
        let jobs = c._keep_jobs.clone();
        let asked = std::thread::spawn(move || {
            let mut asked = Vec::new();
            while let Ok(response) = events.recv_timeout(std::time::Duration::from_secs(30)) {
                match response.reply {
                    worker::Reply::Script(ScriptEvent::Prompt {
                        id,
                        prompt: crate::scripting::Prompt::SignIn { file, .. },
                    }) => {
                        asked.push(file);
                        let typed = answer.then(|| crate::scripting::SignIn {
                            account: "there-account".into(),
                            password: zeroize::Zeroizing::new("there".into()),
                        });
                        jobs.send(Job {
                            epoch: 1,
                            window: 1,
                            ctx: Default::default(),
                            request: Request::Answer {
                                prompt: id,
                                answer: crate::scripting::Answer::SignIn(typed),
                            },
                        })
                        .unwrap();
                    }
                    worker::Reply::Script(ScriptEvent::Finished { .. }) => break,
                    _ => {}
                }
            }
            asked
        });
        c.run();
        assert_eq!(c.result(), expected, "{paths:?}");
        assert_eq!(asked.join().unwrap(), paths[..1]);
        assert_eq!(c.session.s.file.id, 0, "back in the caller's file");
        let peers: Vec<_> = c
            .files
            .peers
            .values()
            .map(|f| {
                let endpoint = f.state.endpoint.as_ref().unwrap();
                (f.state.context.host.as_str(), endpoint.port)
            })
            .collect();
        assert_eq!(peers, Vec::from_iter(opened), "{paths:?}");
    }
}

#[test]
fn desktop_open_file_close_peer_leaves_caller_and_reopen_has_fresh_variables() {
    let mut c = Client::new(
        vec![
            open("Peer", false),
            call("Peer source::Run"),
            close("Peer"),
            open("Peer", true),
            exit("Get ( FileName ) & \"|\" & Get ( LastError )"),
        ],
        vec![var("$$mark", "\"old\""), exit("$$mark")],
    );
    c.run();
    assert_eq!(c.result(), "Root|0");
    assert_eq!(c.files.peers.len(), 1);
    let id = *c.files.peers.keys().next().unwrap();
    assert!(c.session.runtime.globals(id).is_empty());
    assert!(c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|s| s == "Peer.fmp12:close"));
}

#[test]
fn desktop_open_file_missing_denied_and_unsupported_targets_report_errors() {
    for (path, denied, expected) in [("Missing", false, "100"), ("Peer", true, "212")] {
        let mut capture = ScriptStep::new(86);
        capture.flags |= flag::TURN_ON;
        let mut c = Client::new(
            vec![capture, open(path, false), exit("Get ( LastError )")],
            vec![],
        );
        c.server.lock().unwrap().deny = denied;
        c.run();
        assert_eq!(c.result(), expected);
        assert!(c.files.peers.is_empty());
    }
    let mut c = Client::new(
        vec![open("file:/tmp/Peer.fmp12", false), exit("\"unreachable\"")],
        vec![],
    );
    c.run();
    assert!(c.events().iter().any(|e| matches!(
        e,
        ScriptEvent::Finished {
            message: Some(_),
            ..
        }
    )));
    assert!(c.files.peers.is_empty());
}

#[test]
fn desktop_open_file_canceled_or_lost_open_is_not_registered() {
    for loss in [false, true] {
        let mut capture = ScriptStep::new(86);
        capture.flags |= flag::TURN_ON;
        let mut c = Client::new(
            vec![capture, open("Peer", false), exit("Get ( LastError )")],
            vec![],
        );
        {
            let mut s = c.server.lock().unwrap();
            s.cancel = !loss;
            s.lose = loss;
        }
        c.run();
        assert!(c.files.peers.is_empty());
        assert!(c
            .server
            .lock()
            .unwrap()
            .events
            .iter()
            .any(|s| s == "Peer.fmp12:close"));
    }
}

#[test]
fn desktop_open_file_stored_references_decode_source_serials_and_call_the_peer() {
    let mut open = ScriptStep::new(33);
    open.set_data_leaf(32, Some(OwnedNode::leaf([32], [2, 0x80, 0x01])));
    let mut call = ScriptStep::new(1);
    call.set_script_key(Some(StoredKey::new(&[1, 1]).unwrap()));
    call.set_data_leaf(32, Some(OwnedNode::leaf([32], [2, 0x80, 0x01])));
    let mut c = Client::new(
        vec![open, call, exit("Get ( ScriptResult )")],
        vec![exit("Get ( FileName )")],
    );
    c.run();
    assert_eq!(c.result(), "Peer");
    assert_eq!(c.files.peers.len(), 1);
}

#[test]
fn desktop_open_file_external_calls_restore_the_callers_latest_window() {
    let mut c = Client::new(
        vec![
            ScriptStep::new(122),
            var("$$window", "Get ( WindowName )"),
            open("Peer", false),
            call("Peer source::Run"),
            exit("Get ( WindowName ) & \"|\" & $$window & \"|\" & Get ( ScriptResult )"),
        ],
        vec![ScriptStep::new(122), exit("Get ( WindowName )")],
    );
    c.run();
    assert_eq!(c.result(), "Root - 2|Root - 2|Peer - 2");
    let mut windows = vec![c.session.s.file.window.id()];
    windows.extend(c.session.s.file.others.iter().map(|w| w.id()));
    for peer in c.files.peers.values() {
        windows.push(peer.state.window.id());
        windows.extend(peer.state.others.iter().map(|w| w.id()));
    }
    windows.sort();
    windows.dedup();
    assert_eq!(windows.len(), 4);
}

#[test]
fn desktop_open_file_halt_from_peer_reverts_the_owning_transaction() {
    let mut begin = ScriptStep::new(205);
    begin.value = 5;
    let mut c = Client::new(
        vec![
            begin,
            set("\"pending\""),
            open("Peer", false),
            call("Peer source::Run"),
            ScriptStep::new(206),
        ],
        vec![ScriptStep::new(90)],
    );
    c.run();
    let state = c.server.lock().unwrap();
    assert!(state.events.contains(&"Root.fmp12:revert".into()));
    assert!(!state.events.contains(&"Peer.fmp12:revert".into()));
    assert_eq!(state.rows["Root.fmp12"], "root row");
}

#[test]
fn desktop_open_file_close_of_a_participating_file_is_refused_before_mutation() {
    let mut c = Client::new(
        vec![
            open("Peer", false),
            call("Peer source::Run"),
            exit("Get ( ScriptResult ) & \"|\" & Get ( FileName )"),
        ],
        vec![
            close("Root"),
            var("$first", "Get ( LastError )"),
            ScriptStep::new(34),
            exit("$first & \"|\" & Get ( LastError )"),
        ],
    );
    c.run();
    assert_eq!(c.result(), "3|3|Root");
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|s| s.ends_with(":close")));
    assert_eq!(c.files.peers.len(), 1);
}

#[test]
fn desktop_open_file_closing_the_root_keeps_the_peer_provider_and_context() {
    let mut c = Client::new(
        vec![open("Peer", false), ScriptStep::new(34)],
        vec![exit("Get ( FileName )")],
    );
    c.run();
    assert!(c
        .events()
        .iter()
        .any(|e| matches!(e, ScriptEvent::CloseFile)));
    let mut store = Store;
    c.session.handle(
        &mut Io {
            provider: &mut c.provider,
            files: &mut c.files,
            store: &mut store,
            jobs: &c.jobs,
            responses: &c.responses,
        },
        Job {
            epoch: 1,
            window: 1,
            ctx: Default::default(),
            request: Request::Close,
        },
    );
    assert_eq!(c.session.s.file.context.file, "Peer.fmp12");
    assert!(c.files.peers.is_empty());
    assert_eq!(
        c.provider.records(1, 0, 1).unwrap().records[0].values[0].as_deref(),
        Some("peer row")
    );
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .contains(&"Peer.fmp12:close".into()));
}

#[test]
fn desktop_open_file_hidden_startup_runs_once_before_the_caller_continues() {
    let mut c = Client::new(
        vec![
            open("Peer", true),
            open("Peer", true),
            call("Peer source::Run"),
            exit("Get ( ScriptResult )"),
        ],
        vec![exit("$$order")],
    );
    {
        let mut server = c.server.lock().unwrap();
        server.startup = true;
        let scripts = server.scripts.get_mut("Peer.fmp12").unwrap();
        for (id, text) in [(2, "first,"), (3, "window,")] {
            scripts.push(Script {
                id,
                name: format!("Startup {id}"),
                steps: vec![var("$$order", &format!("$$order & {text:?}"))],
                ..Default::default()
            });
        }
    }
    c.run();
    assert_eq!(c.result(), "first,window,");
    assert!(c.files.peers.values().all(|f| f.state.window.state.hidden));
}

#[test]
fn desktop_open_file_adopts_the_rendered_table_before_startup_deletion() {
    let mut c = Client::new(vec![open("Peer", false)], Vec::new());
    {
        let mut server = c.server.lock().unwrap();
        server.startup = true;
        server.peer_pending_table = true;
        let mut delete = ScriptStep::new(9);
        delete.flags |= flag::NO_INTERACT;
        server.scripts.get_mut("Peer.fmp12").unwrap().extend([
            Script {
                id: 2,
                steps: vec![delete],
                ..Default::default()
            },
            Script {
                id: 3,
                steps: Vec::new(),
                ..Default::default()
            },
            Script {
                id: 4,
                steps: vec![var("$$deleted", "Get ( ScriptParameter )")],
                ..Default::default()
            },
        ]);
    }
    assert!(Provider::new(c.server.clone(), "Peer.fmp12")
        .schema()
        .is_pending(90));
    c.run();
    assert!(c
        .server
        .lock()
        .unwrap()
        .events
        .contains(&"Peer.fmp12:delete:1".into()));
    let peer = if c.session.s.file.context.file == "Peer.fmp12" {
        &c.session.s.file
    } else {
        &c.files
            .peers
            .values()
            .find(|file| file.state.context.file == "Peer.fmp12")
            .unwrap()
            .state
    };
    let id = peer.id;
    assert_eq!(peer.window.table, Some(6));
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 1, 1).unwrap());
    let parameter = c
        .session
        .runtime
        .globals(id)
        .get("$$deleted", 1)
        .map(|v| v.to_text(&cx).into_string())
        .unwrap_or_default();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&parameter).unwrap(),
        serde_json::json!({"Peer": {"Related Rows": [["Deleted", 1, ""]]}})
    );
    assert!(c.events().iter().any(|event| matches!(event,
        ScriptEvent::FileOpened(opened) if opened.schema.as_ref().is_some_and(|s| s.table_of(90).is_some_and(|t| t.id == 6))
    )));
}

#[test]
fn desktop_open_file_source_names_stay_in_the_script_file_after_window_selection() {
    let mut select = ScriptStep::new(123);
    select.value = 1;
    select.flags |= flag::MENU_SELECTION;
    select.set_calculation(0, Some(&calc("\"Peer\"")));
    let mut restricted = select.clone();
    restricted.flags |= flag::LIMIT_TO_CURRENT_FILE;
    let mut c = Client::new(
        vec![
            open("Peer", false),
            restricted,
            var("$$restricted", "Get ( LastError )"),
            select,
            call("Peer source::Run"),
            exit("$$restricted & \"|\" & Get ( ScriptResult ) & \"|\" & Get ( FileName )"),
        ],
        vec![exit("Get ( FileName ) & \"|\" & Get ( ScriptParameter )")],
    );
    // Both files reuse script id 1/name Run and alias Peer source points
    // in opposite directions. The selected window cannot redefine it.
    let root = c
        .server
        .lock()
        .unwrap()
        .scripts
        .get_mut("Root.fmp12")
        .unwrap()
        .clone();
    let mut root = root;
    root[0]
        .steps
        .iter_mut()
        .find(|s| s.id() == 1)
        .unwrap()
        .set_calculation(0, Some(&calc("\"parameter\"")));
    c.server
        .lock()
        .unwrap()
        .scripts
        .insert("Root.fmp12".into(), root);
    c.run();
    assert_eq!(c.result(), "112|Peer|parameter|Peer");
}

#[test]
fn desktop_open_file_commit_in_a_selected_peer_cannot_discard_the_callers_transaction() {
    let mut begin = ScriptStep::new(205);
    begin.value = 4;
    let mut select = ScriptStep::new(123);
    select.value = 1;
    select.flags |= flag::MENU_SELECTION;
    select.set_calculation(0, Some(&calc("\"Peer\"")));
    let mut commit = ScriptStep::new(206);
    commit.value = 4;
    let mut c = Client::new(
        vec![
            begin,
            open("Peer", false),
            select,
            commit,
            exit("Get ( LastError ) & \"|\" & Get ( TransactionOpenState )"),
        ],
        vec![],
    );
    c.run();
    assert_eq!(c.result(), "3|0");
    let server = c.server.lock().unwrap();
    assert!(server.events.contains(&"Root.fmp12:revert".into()));
    assert!(!server.events.contains(&"Peer.fmp12:commit".into()));
    assert_eq!(server.rows["Root.fmp12"], "root row");
}

#[test]
fn desktop_open_file_named_script_requires_a_declared_source_alias() {
    for already_open in [false, true] {
        let mut steps = Vec::new();
        if already_open {
            steps.push(open("Peer", false));
        }
        steps.extend([call("Peer::Run"), exit("Get ( LastError )")]);
        let mut c = Client::new(steps, vec![exit("\"wrong source\"")]);
        // Peer is a hosted filename; the declared alias is Peer source.
        c.run();
        assert_eq!(c.result(), "104");
        assert_eq!(
            c.server
                .lock()
                .unwrap()
                .events
                .iter()
                .filter(|e| e.contains(":open:"))
                .count(),
            usize::from(already_open)
        );
    }
}

#[test]
fn desktop_open_file_close_cannot_discard_a_transaction_owned_by_the_selected_peer() {
    let mut select = ScriptStep::new(123);
    select.value = 1;
    select.flags |= flag::MENU_SELECTION;
    select.set_calculation(0, Some(&calc("\"Peer\"")));
    let mut begin = ScriptStep::new(205);
    begin.value = 6;
    let mut commit = ScriptStep::new(206);
    commit.value = 6;
    let mut c = Client::new(
        vec![
            open("Peer", false),
            select,
            begin,
            close("Peer"),
            var(
                "$$closed",
                "Get ( LastError ) & \"|\" & Get ( TransactionOpenState )",
            ),
            commit,
            exit("$$closed & \"|\" & Get ( TransactionOpenState )"),
        ],
        vec![],
    );
    c.run();
    assert_eq!(c.result(), "3|1|0");
    let server = c.server.lock().unwrap();
    assert!(!server.events.contains(&"Peer.fmp12:close".into()));
    assert!(server.events.contains(&"Peer.fmp12:commit".into()));
    assert_eq!(server.rows["Root.fmp12"], "root row");
}

fn call_source(stored: bool) -> ScriptStep {
    if stored {
        let mut step = ScriptStep::new(1);
        step.set_script_key(StoredKey::from_id(2));
        step.set_data_leaf(32, Some(OwnedNode::leaf([32], [2, 0x80, 0x01])));
        step
    } else {
        call("Peer source::Relay")
    }
}

fn source_alternatives(stored: bool, paths: &[&str], expected: &str, attempts: &[&str]) {
    let mut c = Client::new(
        vec![call_source(stored), exit("Get ( ScriptResult )")],
        vec![],
    );
    {
        let mut server = c.server.lock().unwrap();
        server.source_paths = paths.iter().map(|s| (*s).into()).collect();
        for (file, answer) in [
            ("Root.fmp12", "root fallback"),
            ("Peer.fmp12", "preferred peer"),
        ] {
            server.scripts.get_mut(file).unwrap().push(Script {
                id: 2,
                name: "Relay".into(),
                steps: vec![exit(&format!("{answer:?}"))],
                ..Default::default()
            });
        }
    }
    c.run();
    assert_eq!(c.result(), expected);
    assert_eq!(
        c.server
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|s| s.contains(":open:"))
            .cloned()
            .collect::<Vec<_>>(),
        attempts
    );
    assert!(!c.session.s.file.window.state.hidden);
}

#[test]
fn desktop_named_script_sources_try_paths_before_reusing_a_later_open_file() {
    source_alternatives(
        false,
        &["Peer", "Root"],
        "preferred peer",
        &["Root.fmp12:open:Peer.fmp12"],
    );
}

#[test]
fn desktop_stored_script_sources_try_paths_before_reusing_a_later_open_file() {
    source_alternatives(
        true,
        &["Peer", "Root"],
        "preferred peer",
        &["Root.fmp12:open:Peer.fmp12"],
    );
}

#[test]
fn desktop_script_sources_reuse_the_open_fallback_after_earlier_paths_fail() {
    for stored in [false, true] {
        source_alternatives(
            stored,
            &["Missing", "Root"],
            "root fallback",
            &["Root.fmp12:open:Missing.fmp12"],
        );
    }
}

#[test]
fn desktop_implicit_script_resolution_preserves_visible_peer_windows() {
    let mut c = Client::new(
        vec![
            open("Peer", false),
            call("Peer source::Run"),
            exit("Get ( ScriptResult )"),
        ],
        vec![exit("Get ( FileName )")],
    );
    c.run();
    assert_eq!(c.result(), "Peer");
    assert_eq!(c.files.peers.len(), 1);
    assert!(c.files.peers.values().all(|f| !f.state.window.state.hidden));
    assert!(!c.session.s.file.window.state.hidden);
    assert_eq!(
        c.server
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|s| s.contains(":open:"))
            .count(),
        1
    );
}

#[test]
fn desktop_peer_close_popover_keeps_the_callers_identical_popover_open() {
    let mut c = Client::new(
        vec![
            open("Peer", false),
            call("Peer source::Run"),
            exit("Get ( ScriptResult ) & \"|\" & Get ( FileName )"),
        ],
        vec![ScriptStep::new(169), exit("Get ( FileName )")],
    );
    c.run();
    c.events();
    let peer_window = {
        let peer = c.files.peers.values_mut().next().unwrap();
        peer.state.window.state.popovers.insert(1, 77);
        peer.state.window.id()
    };
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(1),
        popovers: BTreeMap::from([(1, 77)]),
        ..Default::default()
    });
    assert_eq!(c.result(), "Peer|Root");
    assert_eq!(c.session.s.file.window.state.popovers.get(&1), Some(&77));
    assert!(!c
        .files
        .peers
        .values()
        .next()
        .unwrap()
        .state
        .window
        .state
        .popovers
        .contains_key(&1));
    let closed = c
        .events
        .try_iter()
        .filter_map(|r| match r.reply {
            worker::Reply::Script(ScriptEvent::ClosePopover {
                layout: 1,
                popover: 77,
                ..
            }) => Some(r.window),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(closed, [peer_window]);
}

fn external_trigger(event: TriggerEvent) -> om_layout::ScriptTrigger {
    om_layout::ScriptTrigger {
        event: event.id(),
        script: Some(1),
        file_reference: Some(129),
        external: true,
        modes: 1,
        option: 0,
        extra: vec![],
        parameter: None,
    }
}

fn ui_external_trigger(event: TriggerEvent, modes: u32, parameter: &str) -> Start {
    Start::Trigger {
        event,
        script: 1,
        file_reference: Some(129),
        parameter: Some(calc(parameter)),
        modes,
        action: Some(super::host::UI_TOKEN | 7),
        info: Default::default(),
        prepare: false,
    }
}

fn entry_window() -> WindowState {
    WindowState {
        layout: Some(1),
        record: Some(1),
        ..Default::default()
    }
}

#[test]
fn external_trigger_runs_in_the_peer_and_can_veto_the_origin_event() {
    let mut c = Client::new(
        vec![set("\"WRONG LOCAL\"")],
        vec![
            set("Get ( FileName ) & \"|\" & Get ( ScriptParameter )"),
            ScriptStep::new(75),
            exit("0"),
        ],
    );
    c.start_in(
        entry_window(),
        ui_external_trigger(
            TriggerEvent::ObjectExit,
            1,
            "Get ( FileName ) & \"|\" & Text",
        ),
    );
    let server = c.server.lock().unwrap();
    assert_eq!(server.rows["Root.fmp12"], "root row");
    assert_eq!(server.rows["Peer.fmp12"], "Peer|Root|root row");
    drop(server);
    assert!(c.events().iter().any(|e| matches!(e, ScriptEvent::Action { token, proceed: false, .. } if *token == super::host::UI_TOKEN | 7)));
}

#[test]
fn an_external_precommit_trigger_resumes_the_owning_window_write() {
    let mut c = Client::new(
        vec![
            set("\"root changed\""),
            ScriptStep::new(75),
            exit("Get ( FileName ) & \"|\" & Text"),
        ],
        vec![set("\"peer changed\""), ScriptStep::new(75), exit("1")],
    );
    c.server.lock().unwrap().triggers.insert(
        "Root.fmp12".into(),
        vec![external_trigger(TriggerEvent::RecordCommit)],
    );
    c.run();
    assert_eq!(c.result(), "Root|root changed");
    let server = c.server.lock().unwrap();
    assert_eq!(server.rows["Root.fmp12"], "root changed");
    assert_eq!(server.rows["Peer.fmp12"], "peer changed");
}

#[test]
fn a_vetoed_external_precommit_trigger_keeps_the_origin_draft() {
    let mut c = Client::new(
        vec![
            set("\"root draft\""),
            ScriptStep::new(75),
            exit("Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Text"),
        ],
        vec![exit("0")],
    );
    c.server.lock().unwrap().triggers.insert(
        "Root.fmp12".into(),
        vec![external_trigger(TriggerEvent::RecordCommit)],
    );
    c.run();
    assert_eq!(c.result(), "20|Root|root draft");
    assert_eq!(c.server.lock().unwrap().rows["Root.fmp12"], "root row");
}

#[test]
fn a_mode_suppressed_external_trigger_never_opens_the_peer() {
    let mut c = Client::new(vec![], vec![set("\"unexpected\"")]);
    c.start_in(
        entry_window(),
        ui_external_trigger(TriggerEvent::ObjectExit, 2, "Text"),
    );
    assert!(c.files.peers.is_empty());
    assert!(c.server.lock().unwrap().events.is_empty());
    assert!(c
        .events()
        .iter()
        .any(|e| matches!(e, ScriptEvent::Action { proceed: true, .. })));
}

#[test]
fn a_refused_external_trigger_does_not_fall_back_to_the_local_script() {
    let mut c = Client::new(vec![set("\"WRONG LOCAL\"")], vec![]);
    c.server.lock().unwrap().deny = true;
    c.start_in(
        entry_window(),
        ui_external_trigger(TriggerEvent::ObjectExit, 1, "Text"),
    );
    assert_eq!(c.server.lock().unwrap().rows["Root.fmp12"], "root row");
    assert!(c.files.peers.is_empty());
    assert!(c
        .events()
        .iter()
        .any(|e| matches!(e, ScriptEvent::Action { proceed: true, .. })));
}

#[test]
fn the_peer_can_fire_the_same_event_while_the_root_trigger_is_held() {
    let mut c = Client::new(
        vec![
            set("\"root changed\""),
            ScriptStep::new(75),
            exit("Get ( LastError )"),
        ],
        vec![
            set("\"peer changed\""),
            ScriptStep::new(75),
            exit("$$local_fired = 1"),
        ],
    );
    let mut local = external_trigger(TriggerEvent::RecordCommit);
    local.file_reference = None;
    local.external = false;
    local.script = Some(2);
    let mut second = script(vec![var("$$local_fired", "1"), exit("1")]);
    second.id = 2;
    {
        let mut server = c.server.lock().unwrap();
        server.triggers.insert(
            "Root.fmp12".into(),
            vec![external_trigger(TriggerEvent::RecordCommit)],
        );
        server.triggers.insert("Peer.fmp12".into(), vec![local]);
        server.scripts.get_mut("Peer.fmp12").unwrap().push(second);
    }
    c.run();
    assert_eq!(c.result(), "0");
    let server = c.server.lock().unwrap();
    assert_eq!(server.rows["Root.fmp12"], "root changed");
    assert_eq!(server.rows["Peer.fmp12"], "peer changed");
}

#[test]
fn opening_a_peer_evaluates_its_trigger_parameter_in_the_peer_namespace() {
    let mut c = Client::new(
        vec![
            var("$$mark", "\"WRONG ROOT GLOBAL\""),
            open("Peer", true),
            exit("Get ( FileName )"),
        ],
        vec![],
    );
    let mut local = external_trigger(TriggerEvent::LayoutEnter);
    local.file_reference = None;
    local.external = false;
    local.script = Some(2);
    local.parameter = Some(om_layout::triggers::StoredFormula {
        context: None,
        polish: calc("$$mark & \"|\" & Get ( FileName ) & \"|\" & Text")
            .polish
            .unwrap(),
    });
    let mut second = script(vec![set("Get ( ScriptParameter )"), ScriptStep::new(75)]);
    second.id = 2;
    {
        let mut server = c.server.lock().unwrap();
        server.triggers.insert("Peer.fmp12".into(), vec![local]);
        server.scripts.get_mut("Peer.fmp12").unwrap().push(second);
    }
    c.run();
    assert_eq!(
        c.server.lock().unwrap().rows["Peer.fmp12"],
        "|Peer|peer row"
    );
    assert_eq!(c.result(), "Root");
}

/// Adjust Window [Resize to Fit] before the window's content area was
/// drawn (#1115): the worker sends the window what the step fits, at the
/// step's zoom and bars, after the window update with what the script
/// changed so far and before a later step's window command, which replaces
/// it. Nothing of it stays in the window's state for later updates to send
/// again.
#[test]
fn resize_to_fit_before_the_first_paint_waits_for_the_window_in_step_order() {
    let adjust = |choice| {
        let mut step = ScriptStep::new(31);
        step.value = choice;
        step
    };
    let mut zoom = ScriptStep::new(97);
    zoom.value = 5;
    let mut c = Client::new(vec![zoom, adjust(1), adjust(2)], vec![]);
    c.server.lock().unwrap().refresh_render = Some(RenderLayout {
        layout: om_layout::Layout {
            id: 1,
            def: Some(om_layout::LayoutDef {
                base_width: 800.0,
                ..Default::default()
            }),
            parts: vec![om_layout::Part {
                height: 300.0,
                ..Default::default()
            }],
            ..Default::default()
        },
        field_bindings: Default::default(),
    });
    // Before the first paint: the content area is not measured.
    let placement =
        crate::desktop::placement(&egui::Context::default(), None, true, 0.0, true, 0.0);
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(1),
        toolbar: false,
        placement: Some(placement),
        ..Default::default()
    });
    let events: Vec<String> = c
        .events()
        .into_iter()
        .filter_map(|event| match event {
            ScriptEvent::Window(update) => Some(format!("window at {}%", update.state.zoom)),
            ScriptEvent::Fit(fit) => Some(format!("{fit:?}")),
            ScriptEvent::Viewport(command) => Some(format!("{command:?}")),
            _ => None,
        })
        .collect();
    // (The run's first update, at 100%, comes before.)
    assert_eq!(
        events[events.len().saturating_sub(3)..],
        [
            "window at 150%",
            "Fit { layout: (800.0, 300.0), zoom: 150, toolbar: false, menubar: true }",
            "Maximized(true)",
        ]
    );
}

#[test]
fn ui_external_trigger_preserves_peer_layout_events() {
    let fixture = || {
        let c = Client::new(
            vec![call("Peer source::Run")],
            vec![var("$$body_fired", "1"), exit("1")],
        );
        {
            let mut server = c.server.lock().unwrap();
            let mut local = external_trigger(TriggerEvent::LayoutEnter);
            local.file_reference = None;
            local.external = false;
            local.script = Some(2);
            server.triggers.insert("Peer.fmp12".into(), vec![local]);
            server.scripts.get_mut("Peer.fmp12").unwrap().push(Script {
                id: 2,
                name: "Peer layout entry".into(),
                steps: vec![var("$$entered", "\"entered\""), exit("1")],
                ..Default::default()
            });
        }
        c
    };
    let mut via_step = fixture();
    via_step.run();
    assert!(via_step
        .session
        .runtime
        .globals(1)
        .get("$$entered", 1)
        .is_some());
    let mut via_ui = fixture();
    via_ui.start_in(
        entry_window(),
        ui_external_trigger(TriggerEvent::ObjectExit, 1, "Text"),
    );
    assert!(via_ui
        .session
        .runtime
        .globals(1)
        .get("$$body_fired", 1)
        .is_some());
    assert!(
        via_ui
            .session
            .runtime
            .globals(1)
            .get("$$entered", 1)
            .is_some(),
        "peer layout event was lost while the external trigger body ran"
    );

    // A UI job arriving during a script uses the session's queued-start path.
    let mut queued = fixture();
    queued
        .server
        .lock()
        .unwrap()
        .scripts
        .insert("Root.fmp12".into(), vec![script(vec![])]);
    queued
        ._keep_jobs
        .send(Job {
            epoch: 1,
            window: entry_window().id,
            ctx: Default::default(),
            request: Request::Script(Box::new(ScriptJob {
                start: ui_external_trigger(TriggerEvent::ObjectExit, 1, "Text"),
                window: entry_window(),
                others: vec![],
                context: SessionContext {
                    host: "host.invalid".into(),
                    file: "Root.fmp12".into(),
                    account: "root-account".into(),
                    local: None,
                },
            })),
        })
        .unwrap();
    queued.run();
    assert!(queued
        .session
        .runtime
        .globals(1)
        .get("$$body_fired", 1)
        .is_some());
    assert!(
        queued
            .session
            .runtime
            .globals(1)
            .get("$$entered", 1)
            .is_some(),
        "peer layout event was lost while the queued UI trigger body ran"
    );
}

fn related_step(external: bool) -> ScriptStep {
    let mut step = ScriptStep::new(74);
    step.value = 5;
    step.flags |= flag::RESTORE;
    step.set_flag(flag::OPTION, external);
    step.set_member_key(StoredKey::from_id(1));
    step.set_slot_child(
        0,
        0x11,
        Some(OwnedNode::leaf(
            [0x11],
            om_data::formula::Key::from_id(90).0,
        )),
    );
    step
}

#[test]
fn desktop_related_navigation_uses_peer_ids_and_keeps_the_callers_frame_context() {
    let mut invoke = call("Peer source::Run");
    invoke.set_calculation(0, Some(&calc("\"parameter\"")));
    let mut c = Client::new(
        vec![
            var("$$mark", "\"root\""),
            invoke,
            exit("Get ( FileName ) & \"|\" & Get ( ScriptResult ) & \"|\" & $$mark & \"|\" & Text"),
        ],
        vec![
            var("$$mark", "\"peer\""),
            exit("$$mark & \"|\" & Get ( ScriptParameter )"),
        ],
    );
    // Run a separate root script as a nested call: its navigation continues
    // in Peer, then returning from it restores the top-level Root window.
    {
        let mut server = c.server.lock().unwrap();
        server.related = true;
        server.scripts.get_mut("Root.fmp12").unwrap().push(Script {
            id: 2, name: "Navigate".into(), steps: vec![
                related_step(true),
                var("$local", "Get ( ScriptParameter )"),
                call("Peer source::Run"),
                exit("Get ( FileName ) & \"|\" & $$mark & \"|\" & $local & \"|\" & Text & \"|\" & Get ( ScriptResult )"),
            ], ..Default::default()
        });
        let mut call = ScriptStep::new(1);
        call.set_script_key(StoredKey::from_id(2));
        call.set_calculation(0, Some(&calc("\"parameter\"")));
        server.scripts.get_mut("Root.fmp12").unwrap()[0].steps[2] = call;
    }
    c.run();
    assert_eq!(
        c.result(),
        "Root|Peer|root|parameter|peer row|peer||root|root row"
    );
    assert_eq!(c.session.s.file.id, 0);
    let peer = c.files.peers.values().next().unwrap();
    assert_eq!(peer.state.window.table, Some(5));
    assert!(!peer.state.window.state.hidden);
    assert!(c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.starts_with("Peer.fmp12:found:") && e.ends_with("Records([1])")));
}

#[test]
fn desktop_related_source_paths_precede_a_colliding_open_root_and_fall_back_in_order() {
    for paths in [["Peer", "Root"], ["Missing", "Peer"]] {
        let mut c = Client::new(
            vec![related_step(true), exit("Get ( FileName ) & \"|\" & Text")],
            vec![],
        );
        {
            let mut server = c.server.lock().unwrap();
            server.related = true;
            server.source_paths = paths.iter().map(|s| (*s).into()).collect();
        }
        c.run();
        assert_eq!(c.result(), "Peer|peer row");
        let opens: Vec<_> = c
            .server
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|e| e.contains(":open:"))
            .cloned()
            .collect();
        let expected: Vec<_> = paths
            .iter()
            .take_while(|p| **p != "Root")
            .map(|p| format!("Root.fmp12:open:{p}.fmp12"))
            .collect();
        assert_eq!(opens, expected);
    }
}

/// The related ids came from one file and the data source's path reaches
/// another of the same name: Root's import is Peer on another port of the
/// host (`FileSchemaContext::identity`), and the source's same-host path
/// opens this host's Peer. Go to Related Record fails with 3 before it
/// commits, opens a window or changes a found set, and the caller stays
/// where it was; the same import of this host's Peer navigates.
#[test]
fn desktop_related_navigation_refuses_a_same_named_file_on_another_endpoint() {
    use om_data::file_refs::FileIdentity;
    for (port, new_window, expected) in [
        (5100, true, "3|Root|root row|Root"),
        (5100, false, "3|Root|root row|Root"),
        (5003, true, "0|Peer|peer row|Peer - 2"),
    ] {
        let mut capture = ScriptStep::new(86);
        capture.set_flag(flag::TURN_ON, true);
        let mut navigate = related_step(true);
        navigate.set_flag(flag::TURN_ON, new_window);
        let mut c = Client::new(
            vec![
                capture,
                navigate,
                exit(
                    "Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Text & \"|\" & \
                     Get ( WindowName )",
                ),
            ],
            vec![],
        );
        {
            let mut server = c.server.lock().unwrap();
            server.related = true;
            server.source_paths = vec!["Peer".into()];
            server.related_identity = Some(FileIdentity::hosted("host.invalid", port, "Peer"));
            // Peer's opening triggers: each refreshes its window, which
            // the provider records.
            server.startup = true;
            for id in [2, 3] {
                server.scripts.get_mut("Peer.fmp12").unwrap().push(Script {
                    id,
                    steps: vec![ScriptStep::new(80)],
                    ..Default::default()
                });
            }
        }
        c.session.s.file.endpoint = Some(crate::scripting::session::Endpoint {
            host: "host.invalid".into(),
            port: 5003,
            file: "Root.fmp12".into(),
            account: "root-account".into(),
            local: None,
        });
        c.run();
        let case = format!("port {port}, new window {new_window}");
        assert_eq!(c.result(), expected, "{case}");
        if port == 5100 {
            // The refused file runs no opening trigger, and no window opens.
            let events = c.server.lock().unwrap().events.clone();
            assert!(
                !events.iter().any(|e| e.starts_with("Peer.fmp12:refresh")),
                "{case}: {events:?}"
            );
            assert_eq!(c.session.s.file.window.id(), 1, "{case}");
            assert!(c.session.s.file.others.is_empty(), "{case}");
        }
    }
}

/// Go to Related Record [Show only related records; From table: Peer's;
/// External; Using layout: <Current Layout>] while Peer already shows a
/// window: FileMaker Pro 26.0.3 keeps the caller's script and window, and
/// Peer's window, on its layout, shows the related records; no window
/// opens (script-runtime.md, Go to Related Record).
#[test]
fn desktop_related_current_layout_keeps_the_caller_when_the_file_shows_a_window() {
    let mut navigate = ScriptStep::new(74);
    navigate.value = 1;
    navigate.flags |= flag::RESTORE | flag::MENU_SELECTION | flag::OPTION;
    navigate.set_slot_child(
        0,
        0x11,
        Some(OwnedNode::leaf(
            [0x11],
            om_data::formula::Key::from_id(90).0,
        )),
    );
    let mut root = ScriptStep::new(123);
    root.value = 1;
    root.flags |= flag::MENU_SELECTION;
    root.set_calculation(0, Some(&calc("\"Root\"")));
    for fails in [false, true] {
        let mut c = Client::new(
            vec![
                open("Peer", false),
                root.clone(),
                navigate.clone(),
                exit(
                    "Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Text & \"|\" & \
                     Get ( WindowName )",
                ),
            ],
            vec![],
        );
        {
            let mut server = c.server.lock().unwrap();
            server.related = true;
            server.peer_found_fails = fails;
        }
        c.run();
        // A failure there leaves the caller where it was too.
        let expected = if fails { "802" } else { "0" };
        assert_eq!(c.result(), format!("{expected}|Root|root row|Root"));
        let peer = c
            .files
            .peers
            .values()
            .find(|f| f.state.context.file == "Peer.fmp12")
            .expect("Peer is open");
        assert!(!peer.state.window.state.hidden);
        assert!(peer.state.others.is_empty(), "no window opened");
        if !fails {
            let window = peer.state.window.id();
            let events = c.server.lock().unwrap().events.clone();
            assert!(
                events
                    .iter()
                    .any(|e| e == &format!("Peer.fmp12:found:{window}:Records([1])")),
                "{events:?}"
            );
        }
    }
}

#[test]
fn desktop_related_destinations_fail_without_using_a_colliding_root_layout() {
    for (external, peer_layout, wrong_table, deny, paths, expected) in [
        (true, Some(2), false, false, vec![], "105"),
        (true, None, true, false, vec![], "106"),
        (false, None, false, false, vec![], "106"),
        (true, None, false, true, vec![], "212"),
        (
            true,
            None,
            false,
            false,
            vec!["Root".into(), "Peer".into()],
            "3",
        ),
    ] {
        let mut c = Client::new(
            vec![
                related_step(external),
                exit("Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Text"),
            ],
            vec![],
        );
        {
            let mut server = c.server.lock().unwrap();
            server.related = true;
            server.peer_layout = peer_layout;
            server.peer_wrong_table = wrong_table;
            server.deny = deny;
            server.source_paths = paths;
        }
        c.run();
        assert_eq!(c.result(), format!("{expected}|Root|root row"));
        assert_eq!(c.session.s.file.window.id(), 1);
        assert_eq!(c.session.s.file.window.state.layout, Some(1));
        assert!(!c
            .server
            .lock()
            .unwrap()
            .events
            .iter()
            .any(|e| e.contains(":found:")));
    }
}

#[test]
fn desktop_related_new_window_keeps_sparse_bounds_and_skips_opening_triggers() {
    let mut navigate = related_step(true);
    navigate.flags |= flag::TURN_ON;
    navigate.set_calculation(2, Some(&calc("740")));
    navigate.set_calculation(4, Some(&calc("31")));
    let mut c = Client::new(
        vec![
            navigate,
            exit("Get ( WindowName ) & \"|\" & Text & \"|\" & $$order"),
        ],
        vec![],
    );
    {
        let mut server = c.server.lock().unwrap();
        server.related = true;
        server.startup = true;
        for id in [2, 3] {
            server.scripts.get_mut("Peer.fmp12").unwrap().push(Script {
                id,
                steps: vec![var("$$order", "\"wrong startup\"")],
                ..Default::default()
            });
        }
    }
    c.run();
    assert_eq!(c.result(), "Peer - 2|peer row|");
    assert_eq!(c.session.s.file.context.file, "Peer.fmp12");
    let placement = c.session.s.file.window.state.placement.as_ref().unwrap();
    assert_eq!(placement.frame.width, 740.0);
    assert_eq!(placement.frame.left - placement.desktop.left, 31.0);
    assert_eq!(c.session.s.file.others.len(), 1);
    let root = &c.files.peers[&0].state;
    assert_eq!(root.window.id(), 1);
    assert_eq!(root.window.state.layout, Some(1));
}

#[test]
fn desktop_related_destination_leaves_find_mode_to_display_the_record() {
    for new_window in [false, true] {
        let mut navigate = related_step(true);
        navigate.set_flag(flag::TURN_ON, new_window);
        let mut c = Client::new(
            vec![
                open("Peer", false),
                call("Peer source::Run"),
                navigate,
                exit("Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Get ( WindowMode ) & \"|\" & Get ( RecordID ) & \"|\" & Text"),
            ],
            vec![ScriptStep::new(22), exit("\"peer find ready\"")],
        );
        c.server.lock().unwrap().related = true;
        c.run();
        assert_eq!(
            c.result(),
            "0|Peer|0|1|peer row",
            "new window: {new_window}"
        );
    }
}

#[test]
fn refresh_window_thaws_only_its_owner_and_keeps_drafts_focus_and_mode() {
    use crate::scripting::window::Window;
    use om_data::runtime::{Mode, ScriptHost, Windows};
    for mode in [Mode::Browse, Mode::Find, Mode::Preview] {
        let mut c = Client::new(vec![], vec![]);
        // Browse starts with a cleared draft and no warm worker row: the
        // refresh must obtain its original before sending the draft back.
        let draft = if mode == Mode::Browse { "" } else { "unsaved" };
        let state = WindowState {
            layout: Some(1),
            record: Some(1),
            mode,
            drafts: [("Text".into(), Some(draft.into()))].into(),
            focus: Some("Text".into()),
            selection: Some((2, 3)),
            sorted: true,
            ..Default::default()
        };
        c.session.s.file.window.adopt(state);
        if mode != Mode::Browse {
            c.session
                .s
                .file
                .window
                .current(c.provider.as_mut())
                .unwrap();
        }
        c.session.s.file.window.frozen = true;
        let mut other = Window::default();
        other.state.id = 2;
        other.frozen = true;
        c.session.s.file.others.push(other);
        let mut peer = Shared::new(Arc::new(AtomicBool::new(false))).file;
        peer.id = 1;
        peer.window.state.id = 3;
        peer.window.frozen = true;
        c.files.peers.insert(
            1,
            crate::scripting::databases::OpenFile {
                provider: Box::new(Provider::new(c.server.clone(), "Peer.fmp12")),
                state: peer,
            },
        );
        let mut store = Store;
        let mut io = Io {
            provider: &mut c.provider,
            files: &mut c.files,
            store: &mut store,
            jobs: &c.jobs,
            responses: &c.responses,
        };
        let mut host = WorkerHost::new(&mut io, &mut c.session.s);
        host.refresh_window(true, true).unwrap();
        host.flush();
        c.server
            .lock()
            .unwrap()
            .rows
            .insert("Root.fmp12".into(), "fresh row".into());
        host.calc_context();
        assert_eq!(host.shared_ref().file.record_values["Text"], "fresh row");
        let file = &host.shared_ref().file;
        assert!(!file.window.frozen);
        assert!(file.others[0].frozen);
        assert!(host.files.peers[&1].state.window.frozen);
        drop(host);
        let events = c.events();
        let update = events
            .iter()
            .find_map(|e| match e {
                ScriptEvent::Window(update) => Some(update),
                _ => None,
            })
            .unwrap();
        assert!(update.refreshed && !update.data_changed);
        assert_eq!(update.state.mode, mode);
        assert_eq!(update.state.record, Some(1));
        assert_eq!(update.state.layout, Some(1));
        assert!(update.state.sorted);
        assert_eq!(update.state.focus.as_deref(), Some("Text"));
        assert_eq!(update.state.selection, Some((2, 3)));
        assert_eq!(update.state.drafts["Text"].as_deref(), Some(draft));
        assert_eq!(update.originals["Text"].as_deref(), Some("root row"));
        assert!(update.scroll.is_empty());
        assert_eq!(
            c.server.lock().unwrap().events,
            ["Root.fmp12:refresh:true:true"]
        );
    }
}

#[test]
fn targeted_refresh_dispatch_resolves_names_active_fallback_and_repetition_defaults_without_writes()
{
    let refresh = |id, name: Option<&str>, repetition: Option<&str>| {
        let mut step = ScriptStep::new(id);
        if let Some(name) = name {
            step.set_calculation(0, Some(&calc(&format!("\"{name}\""))));
        }
        if let Some(repetition) = repetition {
            step.set_calculation(1, Some(&calc(repetition)));
        }
        step
    };
    let mut capture = ScriptStep::new(86);
    capture.set_flag(om_scripts::flag::TURN_ON, true);
    let mut c = Client::new(
        vec![
            capture,
            refresh(167, Some("Named"), None),
            refresh(167, Some("Named"), Some("1")),
            refresh(180, Some("Badge"), None),
            refresh(180, None, None),
            refresh(167, Some("named"), None),
            exit("Get ( LastError )"),
        ],
        vec![],
    );
    c.server.lock().unwrap().refresh_render = Some(RenderLayout {
        layout: om_layout::Layout {
            id: 1,
            def: Some(om_layout::LayoutDef {
                table_alias_key: 5,
                ..Default::default()
            }),
            objects: om_layout::ObjectStore {
                objects: vec![
                    om_layout::LayoutObject {
                        id: 10,
                        object_type: 1,
                        name: Some("Named".into()),
                        field_obj: Some(om_layout::FieldObject {
                            field_spec: Some(om_layout::FieldSpec {
                                table_key: 5,
                                field_key: 1,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    om_layout::LayoutObject {
                        id: 20,
                        name: Some("Badge".into()),
                        object_type: 2,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: [(10, "Text".into())].into(),
    });
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(1),
        object: Some(10),
        focus: Some("Text".into()),
        drafts: [("Text".into(), Some(String::new()))].into(),
        ..Default::default()
    });
    assert_eq!(c.result(), "116");
    let updates: Vec<_> = c
        .events()
        .into_iter()
        .filter_map(|e| {
            if let ScriptEvent::Window(w) = e {
                Some(w)
            } else {
                None
            }
        })
        .collect();
    let targets: Vec<_> = updates
        .iter()
        .flat_map(|w| &w.objects)
        .map(|r| (r.objects.clone(), r.repetition, r.join))
        .collect();
    assert_eq!(
        targets,
        [
            (vec![10], 0, None),
            (vec![10], 1, None),
            (vec![20], 0, None),
            (vec![10], 0, None)
        ]
    );
    for update in updates.iter().filter(|update| !update.objects.is_empty()) {
        assert!(!update.data_changed && !update.refreshed);
        assert_eq!(update.state.focus.as_deref(), Some("Text"));
        assert_eq!(update.state.drafts["Text"].as_deref(), Some(""));
        assert_eq!(update.originals["Text"].as_deref(), Some("root row"));
    }
    assert!(c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .all(|e| e == "Root.fmp12:refresh:false:false"));
    // A later script calculation must not read the worker's pre-refresh row,
    // even though the GUI retains its page and the user's cleared draft.
    use om_data::runtime::{ScriptHost, Windows};
    c.server
        .lock()
        .unwrap()
        .rows
        .insert("Root.fmp12".into(), "fresh stored row".into());
    let mut store = Store;
    let mut io = Io {
        provider: &mut c.provider,
        files: &mut c.files,
        store: &mut store,
        jobs: &c.jobs,
        responses: &c.responses,
    };
    let mut host = WorkerHost::new(&mut io, &mut c.session.s);
    host.refresh_object("Named", 0, false).unwrap();
    host.calc_context();
    assert_eq!(
        host.shared_ref().file.record_values["Text"],
        "fresh stored row"
    );
    assert_eq!(
        host.shared_ref().file.window.state.drafts["Text"].as_deref(),
        Some("")
    );
}

#[test]
fn targeted_portal_refresh_remaps_record_identity_and_drops_disappeared_rows() {
    for (records, expected_row) in [(vec![2, 1], 2), (vec![2], 0)] {
        let mut refresh = ScriptStep::new(180);
        refresh.set_calculation(0, Some(&calc("\"Rows\"")));
        let mut c = Client::new(vec![refresh], vec![]);
        {
            let mut server = c.server.lock().unwrap();
            server.pdf_rows = records;
            server.refresh_render = Some(RenderLayout {
                layout: om_layout::Layout {
                    id: 1,
                    def: Some(om_layout::LayoutDef {
                        table_alias_key: 5,
                        ..Default::default()
                    }),
                    objects: om_layout::ObjectStore {
                        objects: vec![om_layout::LayoutObject {
                            id: 30,
                            object_type: 9,
                            name: Some("Rows".into()),
                            portal: Some(om_layout::PortalObject {
                                table_alias_key: 5,
                                num_rows: 3.0,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                field_bindings: Default::default(),
            });
        }
        c.run_in(WindowState {
            layout: Some(1),
            record: Some(2),
            portal: Some(PortalRow {
                object: 30,
                occurrence: 5,
                row: 1,
                identity: Some(PortalIdentity::Record(1)),
                clicked: false,
                field: None,
                repetition: 1,
            }),
            ..Default::default()
        });
        let row = c.session.s.file.window.state.portal.unwrap();
        assert_eq!(row.row, expected_row);
        assert_eq!(
            row.identity,
            if expected_row == 0 {
                None
            } else {
                Some(PortalIdentity::Record(1))
            }
        );
        let server = c.server.lock().unwrap();
        assert_eq!(
            server
                .events
                .iter()
                .filter(|e| *e == "Root.fmp12:refresh:true:false")
                .count(),
            1,
        );
        // Preview's fixture records reads too; refreshing may read rows but
        // must not write, commit/revert, or operate on another file.
        assert!(
            server
                .events
                .iter()
                .all(|e| e == "Root.fmp12:refresh:true:false"
                    || e.starts_with("Root.fmp12:records:")),
            "unexpected provider events: {:?}",
            server.events,
        );
    }
}

#[test]
fn refresh_window_retains_cleared_draft_originals_after_reload_failure() {
    let mut c = Client::new(vec![ScriptStep::new(80), var("$after", "1")], vec![]);
    c.server.lock().unwrap().fail_rows_after_refresh = true;
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(1),
        drafts: [("Text".into(), None)].into(),
        focus: Some("Text".into()),
        ..Default::default()
    });
    let events = c.events();
    let first = events
        .iter()
        .find_map(|event| match event {
            ScriptEvent::Window(update) if update.refreshed => Some(update),
            _ => None,
        })
        .expect("Refresh Window emits an update");
    assert_eq!(first.originals["Text"].as_deref(), Some("root row"));
    let final_update = events
        .iter()
        .find_map(|event| match event {
            ScriptEvent::Finished { window, .. } => Some(window),
            _ => None,
        })
        .expect("script finishes after the calculation");
    assert_eq!(final_update.state.drafts.get("Text"), Some(&None));
    assert_eq!(c.server.lock().unwrap().rows["Root.fmp12"], "root row");
    assert_eq!(
        final_update
            .originals
            .get("Text")
            .and_then(|v| v.as_deref()),
        Some("root row"),
        "a failed refresh row read must retain the empty draft's original"
    );
    {
        let mut server = c.server.lock().unwrap();
        server.fail_rows_after_refresh = false;
        server.rows.insert("Root.fmp12".into(), "fresh row".into());
    }
    let (_, row) = c
        .session
        .s
        .file
        .window
        .current(c.provider.as_mut())
        .unwrap()
        .unwrap();
    assert_eq!(
        row.values[0].as_deref(),
        Some("fresh row"),
        "a successful retry must replace the retained original snapshot"
    );
}

pub(super) fn preview_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    report_fixture(false, None)
}

pub(super) fn title_grand_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    report_fixture(true, Some(pdf_tests::title_grand_document()))
}

pub(super) fn picture_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    report_fixture(false, Some(pdf_tests::picture_document()))
}

/// The ordinary rows on a layout with a button and a group (#263).
pub(super) fn button_group_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    report_fixture(false, Some(pdf_tests::button_group_document()))
}

pub(super) fn conditional_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    report_fixture(false, Some(pdf_tests::conditional_document()))
}

/// The rows sorted by Region (East, North, West) with sub-summary parts.
pub(super) fn grouped_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    let (provider, events, server) = report_server(true, Some(pdf_tests::grouped_document()));
    {
        let mut server = server.lock().unwrap();
        server.report_groups = true;
        server.report_sort = vec![10];
    }
    (provider, events)
}

/// Three rows in two Region groups (East, East | North), sorted by Region,
/// on the grouped report layout.
pub(super) fn list_groups_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    let (provider, events, server) = report_server(true, Some(pdf_tests::grouped_document()));
    {
        let mut server = server.lock().unwrap();
        server.pdf_rows = vec![7, 2, 4];
        server.report_groups = true;
        server.report_sort = vec![10];
    }
    (provider, events)
}

pub(super) fn sliding_metrics() -> (f64, f64, fn(i64) -> f64) {
    let (ascent, line) = pdf_tests::sliding_metrics();
    (ascent, line, pdf_tests::sliding_lift)
}

pub(super) fn sliding_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    let (provider, events, server) = report_server(false, Some(pdf_tests::sliding_document()));
    {
        let mut server = server.lock().unwrap();
        server.report_groups = true;
        server.report_slides = true;
        server.report_sort = vec![10];
    }
    (provider, events)
}

fn report_fixture(
    grands: bool,
    render: Option<RenderLayout>,
) -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    let (provider, events, _) = report_server(grands, render);
    (provider, events)
}

type ReportServer = (
    Box<dyn DataProvider>,
    Box<dyn Fn() -> Vec<String>>,
    Arc<Mutex<Server>>,
);
fn report_server(grands: bool, render: Option<RenderLayout>) -> ReportServer {
    let server = Arc::new(Mutex::new(Server {
        pdf_rows: vec![7, 2, 9, 4, 8, 1, 6, 3, 5],
        report_grands: grands,
        refresh_render: render,
        ..Default::default()
    }));
    let provider = Box::new(Provider::new(server.clone(), "Root.fmp12"));
    let events = server.clone();
    (
        provider,
        Box::new(move || events.lock().unwrap().events.clone()),
        server,
    )
}

#[test]
fn web_callbacks_keep_the_owner_window_and_the_running_scripts_context() {
    for (option, close_owner) in [(5, false), (0, false), (5, true)] {
        let mut c = Client::new(
            vec![var("$$resumed_window", "Get ( WindowName )"), exit("1")],
            vec![],
        );
        c.server
            .lock()
            .unwrap()
            .scripts
            .get_mut("Root.fmp12")
            .unwrap()
            .push(Script {
                id: 2,
                name: "Callback".into(),
                steps: vec![var("$$callback_window", "Get ( WindowName )"), exit("1")],
                ..Default::default()
            });
        c.session.s.file.window.adopt(WindowState {
            id: 2,
            title: Some("OTHER".into()),
            layout: Some(1),
            record: Some(1),
            ..Default::default()
        });
        let mut owned = Window::default();
        owned.adopt(WindowState {
            id: 1,
            title: Some("OWNER".into()),
            layout: Some(1),
            record: Some(1),
            ..Default::default()
        });
        c.session.s.file.others.push(owned);
        c.session.s.file.context.file = "Root.fmp12".into();
        let mut store = Store;
        let mut io = Io {
            provider: &mut c.provider,
            files: &mut c.files,
            store: &mut store,
            jobs: &c.jobs,
            responses: &c.responses,
        };
        let ScriptSession { runtime, s, .. } = &mut c.session;
        let mut host = WorkerHost::new(&mut io, s);
        host.sync_layout(true);
        runtime
            .perform_script(&mut host, ScriptCall::new(0, 1))
            .unwrap();
        web_viewer_script(
            runtime,
            &mut host,
            1,
            crate::web_viewer::ScriptCall {
                script: "Callback".into(),
                parameter: String::new(),
                option,
            },
        )
        .unwrap();
        if close_owner {
            host.shared().file.others.retain(|window| window.id() != 1);
        }
        while runtime.is_running() {
            runtime.run(&mut host);
        }
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 1, 1).unwrap());
        let seen = runtime
            .globals(0)
            .get("$$callback_window", 1)
            .map(|v| v.to_text(&cx).into_string());
        assert_eq!(
            seen,
            (!close_owner).then(|| "OWNER".to_owned()),
            "option {option}, closed={close_owner}"
        );
        assert_eq!(
            runtime
                .globals(0)
                .get("$$resumed_window", 1)
                .unwrap()
                .to_text(&cx)
                .as_str(),
            "OTHER"
        );
    }
}

#[test]
fn a_dialog_in_another_open_file_blocks_new_and_selected_documents() {
    let mut dialog = ScriptStep::new(122);
    dialog.set_number(0, Some(0x8005_0204u32 as i32));
    dialog.set_calculation(0, Some(&calc("\"Confirm\"")));
    let mut select = ScriptStep::new(123);
    select.value = 1;
    select.flags |= flag::MENU_SELECTION;
    select.set_calculation(0, Some(&calc("\"Peer\"")));
    let mut c = Client::new(
        vec![
            open("Peer", false),
            call("Peer source::Setup"),
            dialog,
            call("Peer source::Blocked"),
            exit("Get ( ScriptResult )"),
        ],
        vec![],
    );
    let mut setup = script(vec![ScriptStep::new(122), exit("Get ( WindowName )")]);
    setup.name = "Setup".into();
    let mut blocked = script(vec![ScriptStep::new(122),
        var("$new", "Get ( LastError )"), select,
        var("$select", "Get ( LastError )"),
        exit("$new & \"|\" & $select & \"|\" & Get ( WindowName ) & \"|\" & ValueCount ( WindowNames ( \"\" ) )")]);
    blocked.id = 2;
    blocked.name = "Blocked".into();
    c.server
        .lock()
        .unwrap()
        .scripts
        .insert("Peer.fmp12".into(), vec![setup, blocked]);
    c.run();
    assert_eq!(c.result(), "3|3|Peer - 2|2");
    let peer = c
        .files
        .peers
        .values()
        .find(|f| f.state.context.file == "Peer.fmp12")
        .unwrap();
    assert_eq!(
        peer.state.others.len(),
        1,
        "refused New Window made no document"
    );
    assert!(
        c.session.s.file.window.state.style.is_dialog(),
        "caller Dialog restored"
    );
}

#[test]
fn imported_script_calculations_keep_the_owning_account_facts() {
    use om_data::calc::eval::{CalcHost, Context};
    use om_data::host::session::selector::{ACCOUNT_PASSWORD_DAYS_REMAINING, ACCOUNT_TYPE};
    let mut c = Client::new(vec![], vec![]);
    let mut root = CalcSchema::new("Root", Locale::en_us());
    root.add_table(5, "Rows");
    root.add_occurrence(5, "Rows", 5);
    let mut peer = CalcSchema::new("Peer", Locale::en_us());
    peer.add_table(5, "PeerRows");
    peer.add_occurrence(5, "PeerRows", 5);
    peer.authenticated_account = om_data::host::AccountInfo {
        account_type: Some("FileMaker File".into()),
        password_expiry: Some(-1),
        set_access: None,
    };
    let map = root.import_file_schema(&peer);
    c.session.s.file.schema = Some(Some(Arc::new(root)));
    let mut store = Store;
    let mut io = Io {
        provider: &mut c.provider,
        files: &mut c.files,
        store: &mut store,
        jobs: &c.jobs,
        responses: &c.responses,
    };
    let mut host = WorkerHost::new(&mut io, &mut c.session.s);
    let context = Context::record(map.occurrences[&5], 1);
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 11, 2).unwrap());
    let account = host
        .get(ACCOUNT_TYPE, &context)
        .unwrap()
        .to_text(&cx)
        .into_string();
    let remaining = host
        .get(ACCOUNT_PASSWORD_DAYS_REMAINING, &context)
        .unwrap()
        .to_text(&cx)
        .into_string();
    assert_eq!(
        (account.as_str(), remaining.as_str()),
        ("FileMaker File", "-1"),
        "nested imported formula must use owning file facts"
    );
}

/// A step of a script set to run with full access raises its own file's
/// set only (script-runtime.md 9.1): its writes in its file carry [Full
/// Access] (`ScriptWrite::privilege_set`), and in another file's window
/// the account's set, never [Full Access] (#1002).
#[test]
fn full_access_raises_only_the_scripts_own_files_set() {
    use om_data::runtime::ScriptHost;
    let mut c = Client::new(Vec::new(), Vec::new());
    let mut store = Store;
    let mut io = Io {
        provider: &mut c.provider,
        files: &mut c.files,
        store: &mut store,
        jobs: &c.jobs,
        responses: &c.responses,
    };
    let mut host = WorkerHost::new(&mut io, &mut c.session.s);
    let here = host.shared_ref().file.id;
    let account = host.current_set_name();
    assert_eq!(account.as_deref(), Some("Root.fmp12-set"));
    host.set_full_access(here, true);
    assert_eq!(host.current_set_name().as_deref(), Some("[Full Access]"));
    host.set_full_access(here + 1, true);
    assert_eq!(host.current_set_name(), account);
    host.set_full_access(here + 1, false);
    assert_eq!(host.current_set_name(), account);
}

/// A script the account's custom set gives no access is refused with 9
/// when another script performs it, also when it is set to run with full
/// access privileges: the flag elevates the steps of a script that runs,
/// and only a caller running with full access lets a script without access
/// run (script-runtime.md 3.7, 9.1; #408). Before, the flag let it run
/// with full access.
#[test]
fn a_script_its_set_gives_no_access_is_refused_however_it_runs() {
    use om_data::host::privilege_set::{
        AccessLevel, Actions, Allow, CustomRules, ItemAccess, MenuCommands, Quad, TableRules,
    };
    let mut perform = ScriptStep::new(1);
    perform.set_script_key(StoredKey::from_id(2));
    // The fixture's scripts capture errors.
    let mut c = Client::new(
        vec![
            perform,
            exit("Get ( LastError ) & \"|\" & Get ( ScriptResult )"),
        ],
        vec![],
    );
    let all = ItemAccess {
        default: Some(Quad(0xff)),
        items: BTreeMap::new(),
    };
    // Scripts executable only (`d6`), script 2 no access (`d5`).
    let scripts = ItemAccess {
        default: Some(Quad(0xd6)),
        items: BTreeMap::from([(vec![2], Quad(0xd5))]),
    };
    {
        let mut server = c.server.lock().unwrap();
        server.set_access = Some(om_data::host::security::SetAccess::Custom(Box::new(
            CustomRules {
                default: TableRules {
                    actions: Actions {
                        create: Allow::Yes,
                        view: Allow::Yes,
                        edit: Allow::Yes,
                        delete: Allow::Yes,
                    },
                    fields: AccessLevel::Modifiable,
                    field_levels: BTreeMap::new(),
                },
                tables: BTreeMap::new(),
                layouts: all.clone(),
                scripts,
                value_lists: all,
                prints: true,
                exports: true,
                menus: MenuCommands::All,
            },
        )));
        let mut locked = Script {
            id: 2,
            name: "Locked".into(),
            steps: vec![exit("\"ran\"")],
            ..Default::default()
        };
        locked.set_runs_with_full_access(true);
        server.scripts.get_mut("Root.fmp12").unwrap().push(locked);
    }
    c.run();
    assert_eq!(c.result(), "9|");
}
