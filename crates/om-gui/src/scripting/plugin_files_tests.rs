//! The plug-in subset's file commands (docs/plugins.md) through the
//! desktop's script host and its file layer: `Folders.UserDesktop` and
//! `Files.CreateDirectory` with a temporary folder standing in for the
//! desktop, and `Files.Delete` beside another database's data file.
use super::*;
use om_data::runtime::plugins::{MBS_FUNCTION, MBS_PLUGIN};

/// `text` compiled with the plug-in's `MBS` function among the names.
fn mbs_calc(text: &str) -> Calculation {
    let mut names = Provider::new(Arc::default(), "Root.fmp12")
        .schema()
        .names()
        .clone();
    let mbs = om_data::formula::PluginFunctionId {
        plugin: MBS_PLUGIN,
        function: MBS_FUNCTION,
    };
    names.add_plugin_function(mbs, "MBS", 1, None);
    let mut context = om_data::formula::Context::new(&names);
    context.occurrence = Some(5);
    let compiled = om_data::formula::compile(text, &context).unwrap();
    Calculation::new(None, om_data::formula::polish::encode(&compiled.expr))
}

/// A script makes a folder on the desktop, `Folders.UserDesktop` nested in
/// `Files.CreateDirectory`, and the folders above it with it; a file where
/// the folder would go is refused with the plug-in's error text.
#[test]
fn a_script_makes_a_folder_on_the_desktop_but_not_over_a_file() {
    let desktop = tempfile::tempdir().unwrap();
    std::fs::write(desktop.path().join("Taken"), b"").unwrap();
    // Nothing is made unless the desktop is the temporary folder.
    let here = desktop.path().to_str().unwrap();
    let guarded = |make: &str| {
        format!("If ( MBS ( \"Folders.UserDesktop\" ) = {here:?} ; {make} ; \"elsewhere\" )")
    };
    let job = guarded(
        "MBS ( \"Files.CreateDirectory\" ; MBS ( \"Folders.UserDesktop\" ) & \"/Reports/\" & \"R-1\" )",
    );
    let taken =
        guarded("MBS ( \"Files.CreateDirectory\" ; MBS ( \"Folders.UserDesktop\" ) & \"/Taken\" )");
    let mut exit = ScriptStep::new(103);
    exit.set_calculation(0, Some(&mbs_calc(&format!("{job} & \"|\" & {taken}"))));
    let mut c = Client::new(vec![exit], vec![]);
    c.session.s.file.data_files.folders.desktop = desktop.path().to_owned();
    c.run();
    assert_eq!(c.result(), "OK|[MBS] Failed to create directory.");
    assert!(desktop.path().join("Reports").join("R-1").is_dir());
    assert!(desktop.path().join("Taken").is_file());
}

/// A data file one database's script holds open is not deleted from
/// another: Root opens it, then a script of Peer, opened beside it, asks
/// `Files.Delete` for it.
#[test]
fn a_data_file_another_database_holds_is_not_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let held = dir.path().join("held.dat");
    let native = held.to_str().unwrap().to_owned();
    let path_step = |id: u16| {
        let mut s = ScriptStep::new(id);
        s.flags |= flag::FILE_PATH;
        // The steps take a FileMaker path; Files.Delete a native one.
        let path = format!(
            "file:{}",
            crate::scripting::files::filemaker_path(&held, false)
        );
        s.set_data_leaf(0x82, Some(om_data::text_node(&[0x82], &path)));
        s
    };
    let mut open_data = path_step(191);
    open_data.set_variable_target(0, "$id");
    let mut delete = ScriptStep::new(141);
    delete.set_text(0, Some("$r"));
    let source = format!("MBS ( \"Files.Delete\" ; {native:?} )");
    delete.set_calculation(1, Some(&mbs_calc(&source)));
    let mut answer = ScriptStep::new(103);
    answer.set_calculation(0, Some(&mbs_calc("$r")));
    let mut c = Client::new(
        vec![
            path_step(190),
            open_data,
            open("Peer", false),
            call("Peer source::Run"),
            exit("Get ( ScriptResult )"),
        ],
        vec![delete, answer],
    );
    c.run();
    assert_eq!(c.result(), "[MBS] The file is in use.");
    assert!(held.exists());
}
