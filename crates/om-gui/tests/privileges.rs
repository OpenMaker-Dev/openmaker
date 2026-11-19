//! Root-file privilege metadata reaches layout calculations and desktop scripts.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;
#[path = "../../om-provider/tests/common/related.rs"]
mod related;
#[path = "../../om-provider/tests/common/related_privileges.rs"]
mod related_privileges;
#[path = "../../om-provider/tests/common/privileges.rs"]
mod security;

use std::sync::Arc;

use om_data::formula::{compile, Context, Formula};
use om_data::host::{CalcData, CalcNeed, CalcSchema, Privileges};
use om_data::{
    Connection, DataError, DataProvider, LayoutInfo, RecordPage, RenderLayout, ScriptCatalog,
};
use om_layout::{LayoutObject, ObjectCalcs, Part, Rect};
use om_provider::NativeProvider;
use om_scripts::ScriptStep;

const REPORT: &str = "Get ( CurrentPrivilegeSetName ) & \"|\" & Get ( AccountPrivilegeSetName ) & \"|\" & Substitute ( Get ( CurrentExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Substitute ( Get ( AccountExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Get ( LayoutAccess ) & \"|\" & Get ( RecordAccess )";
const ACCOUNT: &str = "Office|Office|zeta,alpha|zeta,alpha|2|2";
const ELEVATED: &str = "[Full Access]|Office|fmapp,full-only|zeta,alpha|2|2";

fn show(expression: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&fixture::calc("\"Privileges\"")));
    step.set_calculation(1, Some(&fixture::calc(expression)));
    step.set_text(0, Some("OK"));
    step
}

/// Add a synthetic calculation label to the native file's otherwise empty
/// layout. Records, schema, script flags and security all come over TLS.
struct WithLabel(NativeProvider);
impl DataProvider for WithLabel {
    fn open(&mut self, c: &Connection) -> Result<(), DataError> {
        self.0.open(c)
    }
    fn close(&mut self) {
        self.0.close();
    }
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<om_data::HostedFile>, DataError> {
        Ok(vec![])
    }
    fn account_privileges(&self) -> Option<Privileges> {
        self.0.account_privileges()
    }
    fn full_access_privileges(&self) -> Option<Privileges> {
        self.0.full_access_privileges()
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.0.layouts()
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        self.0.scripts()
    }
    fn records(&mut self, id: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.0.records(id, offset, limit)
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        self.0.calc_schema()
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        self.0.calc_data(needs)
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        let Some(mut render) = self.0.render_layout(id)? else {
            return Ok(None);
        };
        let schema = self.0.calc_schema()?.unwrap();
        let polish = compile(
            "\"Account: \" & Get ( AccountPrivilegeSetName )",
            &Context::new(schema.names()),
        )
        .unwrap()
        .expr;
        render.layout.def.as_mut().unwrap().base_width = 400.0;
        render.layout.parts = vec![Part {
            part_type: 6,
            height: 120.0,
            ..Default::default()
        }];
        render.layout.objects.objects.push(LayoutObject {
            id: 90,
            object_type: 10,
            bounds: Some(Rect {
                left: 10.0,
                top: 10.0,
                right: 310.0,
                bottom: 45.0,
                ..Default::default()
            }),
            calcs: ObjectCalcs {
                label: Some(Formula {
                    context: None,
                    formatted: None,
                    polish,
                }),
                ..Default::default()
            },
            ..Default::default()
        });
        Ok(Some(render))
    }
}

fn expect(app: &mut common::App, script: &str, expected: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, script);
    common::wait_for_label(app, expected);
    common::click(app, "OK");
    common::wait_until(app, "script finished", |a| !a.state().script_running());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn authenticated_metadata_reaches_first_layout_and_elevated_scripts_keep_account_answers() {
    let scripts = vec![
        fixture::script(1, "Account privileges", vec![show(REPORT)]),
        fixture::full_access(
            2,
            "Elevated privileges",
            vec![
                fixture::set_variable("$before", REPORT),
                fixture::perform_script(3),
                show(&format!(
                    "$before & \" :: \" & $$nested & \" :: \" & ({REPORT})"
                )),
            ],
        ),
        fixture::script(
            3,
            "Nested account",
            vec![fixture::set_variable("$$nested", REPORT)],
        ),
    ];
    let root = security::tree(&scripts, security::CUSTOM_KEY);
    let file = fixture::TempFile::from_tree("gui-privileges", &root);
    let hosted = hosted::host_file(
        "privileges",
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(WithLabel(hosted.provider())));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "Account: Office");
    expect(&mut app, "Account privileges", ACCOUNT);
    expect(
        &mut app,
        "Elevated privileges",
        &format!("{ELEVATED} :: {ACCOUNT} :: {ELEVATED}"),
    );
    expect(&mut app, "Account privileges", ACCOUNT);
    common::wait_for_label(&mut app, "Account: Office");
}

#[test]
fn root_script_elevation_does_not_change_related_account_or_current_answers() {
    use related_privileges as p;
    let expression = format!(
        "({}) & \" / \" & Remote Tasks::NestedPrivileges & \" / \" & \
         Remote Tasks::DeepPrivileges & \" / \" & Unknown Tasks::Privileges & \" / \" & ({})",
        p::REPORT,
        p::REPORT
    );
    let dialog = || {
        let mut step = ScriptStep::new(87);
        step.set_calculation(1, Some(&p::calculation(&expression)));
        step.set_text(0, Some("OK"));
        step
    };
    let scripts = [
        fixture::script(1, "Related account answers", vec![dialog()]),
        fixture::full_access(2, "Elevated related answers", vec![dialog()]),
    ];
    let host = related::Host::new("gui-related-privileges", p::files(&scripts));
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved license");
    common::fill(&mut app, "Host", "127.0.0.1");
    hosted::replace(&mut app, "Port", &host.port.to_string());
    common::fill(&mut app, "File", p::ROOT);
    common::fill(&mut app, "Account", fixture::ACCOUNT);
    common::fill(&mut app, "Password", fixture::PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the synthetic file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    for (script, root_answer) in [
        ("Related account answers", ACCOUNT),
        ("Elevated related answers", p::ELEVATED),
        ("Related account answers", ACCOUNT),
    ] {
        expect(
            &mut app,
            script,
            &format!(
                "{root_answer} / {} / {} / {} / {root_answer}",
                p::RELATED_ANSWER,
                p::NESTED_ANSWER,
                p::PLAIN_ANSWER
            ),
        );
    }
}

/// File > Manage > Security on a hosted synthetic file (#228): the file's
/// own [Full Access] account sees its accounts, privilege sets and extended
/// privileges, read only; an account in [Data Entry Only] has the Manage
/// Security menu disabled by its decoded account rights. With
/// `OM_SECURITY_SHOTS` naming a directory, each tab is written there as a PNG.
#[test]
fn manage_security_shows_the_files_security_read_only() {
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    // The desktop opens a file with a script catalog.
    let script = fixture::script(1, "Hello", vec![fixture::exit_with("1")]);
    let mut root = fixture::tree(&[script], true);
    fixture::add_local_accounts(&mut root);
    // Clerk's [Data Entry Only] whole again, so that the server admits it.
    fixture::put_data_entry_only(&mut root);
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
    let file = fixture::TempFile::from_tree("gui-security", &root);
    let shots = std::env::var_os("OM_SECURITY_SHOTS").map(std::path::PathBuf::from);
    let open_security = |account: (&str, &str), name: &str| {
        let hosted = hosted::host_file(name, &file.0, fixture::FILE, account);
        let mut app = common::harness(Box::new(hosted.provider()));
        hosted.fill_form(&mut app);
        common::click(&mut app, "Open");
        common::wait_until(&mut app, "the synthetic file", |app| {
            assert!(app.state().error().is_none(), "{:?}", app.state().error());
            app.state().is_settled()
        });
        common::open_menu(&mut app, "File");
        common::click(&mut app, "Manage");
        if account.0 != fixture::CLERK {
            common::click(&mut app, "Security...");
            common::wait_for_label(&mut app, "Manage Security for “Tasks.fmp12”");
        }
        (hosted, app)
    };

    let (_admin_host, mut app) = open_security((fixture::ACCOUNT, fixture::PASSWORD), "security");
    common::wait_for_label(&mut app, "4 accounts");
    for label in [
        "Former",
        "Inactive",
        "Newcomer",
        "Must change at next login",
    ] {
        app.get_by_label(label);
    }
    app.set_render_cursor(false);
    for (tab, ready, name) in [
        ("Accounts", "4 accounts", "accounts"),
        ("Privilege Sets", "2 privilege sets", "privilege-sets"),
        (
            "Extended Privileges",
            "1 extended privilege",
            "extended-privileges",
        ),
    ] {
        common::click(&mut app, tab);
        common::wait_for_label(&mut app, ready);
        if let Some(dir) = &shots {
            let path = dir.join(format!("manage-security-{name}.png"));
            app.render().unwrap().save(&path).unwrap();
        }
    }
    app.get_by_label("fmapp");
    common::click(&mut app, "Privilege Sets");
    // Both sets of the fixture keep the flags `55 03`.
    assert_eq!(
        app.query_all_by_label("Create, edit and delete in all tables")
            .count(),
        2
    );
    // Nothing to edit: no field, checkbox or button changes the catalog.
    assert!(app.query_by_label("New Account").is_none());
    common::click(&mut app, "Done");
    app.run_steps(3);
    assert!(app.query_by_label("4 accounts").is_none());

    let (_clerk_host, mut app) =
        open_security((fixture::CLERK, fixture::CLERK_PASSWORD), "security-clerk");
    assert!(app
        .get_by_label("Security...")
        .accesskit_node()
        .is_disabled());
    assert!(app
        .query_by_label("Manage Security for “Tasks.fmp12”")
        .is_none());
    assert!(app.query_by_label("4 accounts").is_none());
    if let Some(dir) = &shots {
        app.set_render_cursor(false);
        let path = dir.join("manage-security-refused.png");
        app.render().unwrap().save(&path).unwrap();
    }
}
