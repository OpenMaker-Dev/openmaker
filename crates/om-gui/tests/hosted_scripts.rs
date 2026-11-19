//! Scripts through the GUI on a hosted file, end to end: the runtime on the
//! provider worker, the native provider and an in-process `om-server`
//! hosting test project (see `hosted`). A transaction holds its changes and
//! locks until Commit Transaction; a stored find with a related field's
//! criterion joins back to the layout's table.
//! `cargo test -p om-gui --test hosted_scripts -- --ignored`
mod common;
mod hosted;

use om_data::formula::{compile, formatted, polish, Context, NameTable};
use om_data::found::{FoundSet, FoundSetChange};
use om_data::runtime::TransactionOptions;
use om_data::specs::{encode_requests, SpecField, StoredRequest};
use om_data::{
    flag, Calculation, Connection, DataError, DataProvider, FieldChanges, FieldSpec, HostedFile,
    LayoutInfo, Record, RecordCapabilities, RecordPage, RenderLayout, Script, ScriptCatalog,
    ScriptStep, StoredKey,
};
use om_provider::NativeProvider;

/// The native provider, with the scripts of these tests as the file's.
struct WithScripts {
    inner: NativeProvider,
    scripts: ScriptCatalog,
}

impl DataProvider for WithScripts {
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(self.scripts.clone())
    }
    fn change_found_set(&mut self, l: u32, c: FoundSetChange) -> Result<FoundSet, DataError> {
        self.inner.change_found_set(l, c)
    }
    fn value_list(&mut self, id: u64) -> Result<Vec<String>, DataError> {
        self.inner.value_list(id)
    }
    fn index_values(&mut self, l: u32, f: &str) -> Result<Vec<String>, DataError> {
        self.inner.index_values(l, f)
    }
    fn set_next_serial_value(&mut self, o: u64, f: u64, v: &str) -> Result<(), DataError> {
        self.inner.set_next_serial_value(o, f, v)
    }
    fn begin_transaction(&mut self, o: TransactionOptions) -> Result<(), DataError> {
        self.inner.begin_transaction(o)
    }
    fn begin_record_commit(&mut self) -> Result<(), DataError> {
        self.inner.begin_record_commit()
    }
    fn commit_transaction(&mut self, skip: bool) -> Result<(), DataError> {
        self.inner.commit_transaction(skip)
    }
    fn revert_transaction(&mut self) -> Result<(), DataError> {
        self.inner.revert_transaction()
    }
    fn record_capabilities(&mut self, l: u32) -> Result<RecordCapabilities, DataError> {
        self.inner.record_capabilities(l)
    }
    fn create_record(&mut self, l: u32, v: FieldChanges) -> Result<Record, DataError> {
        self.inner.create_record(l, v)
    }
    fn update_record(&mut self, l: u32, id: i64, v: FieldChanges) -> Result<Record, DataError> {
        self.inner.update_record(l, id, v)
    }
    fn delete_record(&mut self, l: u32, id: i64) -> Result<(), DataError> {
        self.inner.delete_record(l, id)
    }
    fn calc_schema(
        &mut self,
    ) -> Result<Option<std::sync::Arc<om_data::host::CalcSchema>>, DataError> {
        self.inner.calc_schema()
    }
    fn calc_data(
        &mut self,
        needs: &[om_data::host::CalcNeed],
    ) -> Result<om_data::host::CalcData, DataError> {
        self.inner.calc_data(needs)
    }
    fn render_layout(&mut self, l: u32) -> Result<Option<RenderLayout>, DataError> {
        self.inner.render_layout(l)
    }
    fn list_files(&mut self, h: &str, p: u16) -> Result<Vec<HostedFile>, DataError> {
        self.inner.list_files(h, p)
    }
    fn open(&mut self, c: &Connection) -> Result<(), DataError> {
        self.inner.open(c)
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.inner.layouts()
    }
    fn records(&mut self, l: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.inner.records(l, offset, limit)
    }
    fn layout_geometry(&mut self, l: u32) -> Result<Option<om_data::LayoutGeometry>, DataError> {
        self.inner.layout_geometry(l)
    }
    fn close(&mut self) {
        self.inner.close();
    }
}

/// Test project's ids: occurrences by name and fields by name, from its
/// calculation schema.
struct Ids {
    names: NameTable,
    contacts: u64,
    emails: u64,
    title: u64,
    first_name: u64,
    address: u64,
}

impl Ids {
    fn read(hosted: &hosted::Hosted) -> Ids {
        let mut provider = hosted.opened();
        let schema = provider.calc_schema().unwrap().unwrap();
        let occurrence = |name: &str| {
            schema
                .occurrences()
                .find(|o| o.name == name)
                .map(|o| o.id)
                .unwrap()
        };
        let (contacts, emails) = (occurrence("Contacts"), occurrence("Email Addresses"));
        let field = |occurrence: u64, name: &str| {
            schema
                .table_of(occurrence)
                .unwrap()
                .fields
                .values()
                .find(|f| f.name == name)
                .unwrap()
                .id
        };
        let mut names = NameTable::default();
        names.add_occurrence(contacts, "Contacts", contacts);
        for name in ["Title", "First Name"] {
            names.add_field(contacts, field(contacts, name), name);
        }
        let ids = Ids {
            contacts,
            emails,
            title: field(contacts, "Title"),
            first_name: field(contacts, "First Name"),
            address: field(emails, "Address"),
            names,
        };
        provider.close();
        ids
    }

    fn calc(&self, source: &str) -> Calculation {
        let compiled = compile(source, &Context::new(&self.names)).expect("formula compiles");
        Calculation::new(
            Some(formatted::encode(&compiled.tokens)),
            polish::encode(&compiled.expr),
        )
    }

    fn set_title(&self, value: &str) -> ScriptStep {
        let key = |id: u64| {
            let bytes = id.to_be_bytes();
            let bytes = &bytes[bytes.iter().position(|b| *b != 0).unwrap_or(7)..];
            StoredKey::new(&[&[bytes.len() as u8][..], bytes].concat()).unwrap()
        };
        let mut step = ScriptStep::new(76);
        step.set_field_spec(
            0,
            Some(FieldSpec::new(key(self.contacts), key(self.title), 1)),
        );
        step.set_calculation(0, Some(&self.calc(value)));
        step
    }
}

fn capture_errors() -> ScriptStep {
    let mut step = ScriptStep::new(86);
    step.flags |= flag::TURN_ON;
    step
}

fn transaction_step(id: u16, commit_at: u16) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.value = commit_at;
    step
}

fn scripts(ids: &Ids) -> ScriptCatalog {
    let mut next = ScriptStep::new(16);
    next.value = 4;
    next.flags |= flag::MENU_SELECTION;
    let mut find = ScriptStep::new(28);
    find.flags |= flag::RESTORE | flag::EXTENDED_DATA;
    find.set_data_leaf(
        0x06,
        Some(encode_requests(&[
            StoredRequest {
                omit: false,
                criteria: vec![(
                    SpecField {
                        occurrence: Some(ids.emails),
                        field: ids.address,
                        repetition: 1,
                    },
                    "*".into(),
                )],
            },
            StoredRequest {
                omit: false,
                criteria: vec![(
                    SpecField {
                        occurrence: Some(ids.contacts),
                        field: ids.first_name,
                        repetition: 1,
                    },
                    "Hello".into(),
                )],
            },
        ])),
    );
    let script = |id: u64, name: &str, steps: Vec<ScriptStep>| Script {
        id,
        name: name.into(),
        steps,
        ..Default::default()
    };
    ScriptCatalog {
        scripts: vec![
            script(
                1,
                "Hosted transaction",
                vec![
                    capture_errors(),
                    transaction_step(205, 6),
                    ids.set_title("\"Dr.\""),
                    next,
                    ids.set_title("\"Mx.\""),
                    ScriptStep::new(62),
                    transaction_step(206, 6),
                ],
            ),
            script(
                2,
                "Hosted related find",
                vec![
                    capture_errors(),
                    find,
                    ids.set_title("Get ( FoundCount ) & \"/\" & Get ( LastError )"),
                    ScriptStep::new(75),
                ],
            ),
        ],
        writable: false,
        unavailable_reason: None,
    }
}

/// Each Contacts record's Title, by record id, as another session reads it.
fn titles(hosted: &hosted::Hosted) -> Vec<(i64, Option<String>)> {
    let mut provider = hosted.opened();
    let layout = provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Contact Details")
        .unwrap()
        .id;
    let page = provider.records(layout, 0, 100).unwrap();
    let column = page.fields.iter().position(|f| f.name == "Title").unwrap();
    provider.close();
    page.records
        .iter()
        .map(|r| (r.id, r.values[column].clone()))
        .collect()
}

fn open_app(hosted: &hosted::Hosted, ids: &Ids) -> common::App {
    let provider = WithScripts {
        inner: hosted.provider(),
        scripts: scripts(ids),
    };
    let mut app = common::harness(Box::new(provider));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    app
}

fn run(app: &mut common::App, name: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, name);
}

fn wait_idle(app: &mut common::App) {
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
}

#[test]
#[ignore = "requires the git-ignored copy of test project"]
fn a_hosted_transaction_holds_changes_and_locks_until_commit() {
    let hosted = hosted::host("script-transaction");
    let ids = Ids::read(&hosted);
    let before = titles(&hosted);
    let mut app = open_app(&hosted, &ids);
    run(&mut app, "Hosted transaction");
    common::wait_until(&mut app, "paused", |app| app.state().script_paused());
    // Both records were left, but nothing reached the host, and the first
    // one is locked by the transaction.
    assert_eq!(titles(&hosted), before);
    let mut other = hosted.opened();
    let layout = other
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Contact Details")
        .unwrap()
        .id;
    let page = other.records(layout, 0, 100).unwrap();
    let locked = page
        .records
        .iter()
        .map(|r| {
            other.update_record(
                layout,
                r.id,
                FieldChanges::from([("Title".into(), Some("X".into()))]),
            )
        })
        .filter(|r| r.as_ref().is_err_and(|e| e.code == Some(301)))
        .count();
    assert_eq!(
        locked, 1,
        "the record committed into the transaction is locked"
    );
    other.close();
    common::click(&mut app, "Continue");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let mut after: Vec<Option<String>> = titles(&hosted).into_iter().map(|(_, t)| t).collect();
    after.sort();
    assert!(after.contains(&Some("Dr.".into())), "{after:?}");
    assert!(after.contains(&Some("Mx.".into())), "{after:?}");
}

#[test]
#[ignore = "requires the git-ignored copy of test project"]
fn a_hosted_find_joins_a_related_criterion_back() {
    let hosted = hosted::host("script-related");
    let ids = Ids::read(&hosted);
    let mut app = open_app(&hosted, &ids);
    run(&mut app, "Hosted related find");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // No contact has an email address; the second request finds Hello.
    let titles = titles(&hosted);
    assert_eq!(
        titles.iter().find(|(id, _)| *id == 2).unwrap().1.as_deref(),
        Some("1/0")
    );
}
