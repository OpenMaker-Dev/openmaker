//! The MBS plug-in subset (`runtime::plugins`, docs/plugins.md) in scripts
//! against the memory host: a script schedules an idle script, which runs
//! in the window that scheduled it, asks with the plug-in's alert, and
//! commits or reverts by its own steps, for each of the alert's answers.
//! The scripts are synthetic, shaped like a portal's "save changes?" chain.

use om_calc::eval::FieldInfo;
use om_calc::{Value, ValueType};
use om_formula::{NameTable, PluginFunctionId};
use om_scripts::runtime::plugins::{
    AlertMode, IdleScript, PluginDialogAnswer, PluginState, MBS_FUNCTION, MBS_PLUGIN,
};
use om_scripts::runtime::*;
use om_scripts::{Calculation, Script, ScriptStep};

const PEOPLE: u64 = 0x10;
const NAME: u64 = 1;
const SCHEDULE: u64 = 10;
const WHEN_IDLE: u64 = 11;

/// The names calculations compile against: the People table and the
/// plug-in's `MBS` function.
fn names() -> NameTable {
    let mut names = NameTable::default();
    names.add_occurrence(PEOPLE, "People", 1);
    names.add_field(1, NAME, "Name");
    let mbs = PluginFunctionId {
        plugin: MBS_PLUGIN,
        function: MBS_FUNCTION,
    };
    names.add_plugin_function(mbs, "MBS", 1, None);
    names
}

fn calc(text: &str) -> Calculation {
    let names = names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(PEOPLE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

fn set_var(name: &str, value: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(value)));
    step
}

fn with_calc(id: u16, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.set_calculation(0, Some(&calc(text)));
    step
}

fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    let mut script = Script {
        id,
        name: name.into(),
        ..Default::default()
    };
    script.insert_steps(0, steps).expect("nests");
    script
}

/// A window on People's first record, with the two scripts, and the
/// plug-in's state when `plugin`.
fn host(plugin: bool) -> MemoryScriptHost {
    let mut host = MemoryScriptHost::en_us();
    host.add_table(1, "People");
    host.add_occurrence(PEOPLE, "People", 1);
    host.add_field(1, NAME, "Name", FieldInfo::stored(ValueType::Text, 1));
    host.add_record(1, 1);
    host.calc.set(1, 1, NAME, 1, Value::text("Ada"));
    host.add_layout(1, "People", PEOPLE);
    host.plugin = plugin.then(PluginState::default);
    // Restart the idle script on each change, as an OnObjectModify script
    // would.
    host.add_script(
        0,
        script(
            SCHEDULE,
            "Schedule",
            vec![
                set_var("$r", r#"MBS ( "FM.StopScriptIdle" )"#),
                set_var(
                    "$r",
                    r#"MBS ( "FM.RunScriptIdle" ; 7 ; Get ( FileName ) ; "When Idle" )"#,
                ),
            ],
        ),
    );
    // Ask, then revert on the alternate button, else commit.
    host.add_script(
        0,
        script(
            WHEN_IDLE,
            "When Idle",
            vec![
                set_var("$r", r#"MBS ( "Dialog.Reset" )"#),
                set_var(
                    "$r",
                    r#"MBS ( "Dialog.SetMessage" ; "Keep " & People::Name & "?" )"#,
                ),
                set_var("$r", r#"MBS ( "Dialog.SetMode" ; "Warning" )"#),
                set_var("$r", r#"MBS ( "Dialog.SetTimeout" ; 10 )"#),
                set_var("$r", r#"MBS ( "Dialog.SetDefaultButton" ; "Keep" )"#),
                set_var("$r", r#"MBS ( "Dialog.SetAlternateButton" ; "Drop" )"#),
                set_var("$r", r#"MBS ( "Dialog.Run" )"#),
                with_calc(68, r#"$r = "drop""#),
                ScriptStep::new(51),
                ScriptStep::new(69),
                set_var("$$kept", r#"$r"#),
                ScriptStep::new(75),
                ScriptStep::new(70),
            ],
        ),
    );
    host
}

fn run(host: &mut MemoryScriptHost, runtime: &mut Runtime, id: u64) -> RunState {
    runtime
        .perform_script(host, ScriptCall::new(0, id))
        .expect("starts");
    runtime.run(host)
}

/// The scheduled script, started as the desktop starts it: in the window
/// that scheduled it, with the parameter given.
fn fire(host: &mut MemoryScriptHost, runtime: &mut Runtime, idle: &IdleScript) -> RunState {
    runtime.fire_trigger(
        host,
        Trigger {
            preset_parameter: idle.parameter.clone().map(Value::text),
            window: idle.window,
            ..Trigger::new(TriggerEvent::Timer, idle.file, idle.script)
        },
    );
    runtime.run(host)
}

fn global(runtime: &mut Runtime, name: &str) -> String {
    let locale = om_calc::Locale::en_us();
    let cx = om_calc::Ctx::new(&locale, om_calc::Date::from_ymd(2026, 1, 1).unwrap());
    runtime
        .globals(0)
        .get(name, 1)
        .map(|v| v.to_text(&cx).into_string())
        .unwrap_or_default()
}

#[test]
fn the_idle_script_commits_on_keep_and_on_timeout_and_reverts_on_drop() {
    for (answer, commits) in [
        (PluginDialogAnswer::Button(0), Some("Keep")),
        (PluginDialogAnswer::Timeout, Some("Timeout")),
        (PluginDialogAnswer::Button(1), None),
    ] {
        let mut host = host(true);
        let mut runtime = Runtime::new(Options::default());
        run(&mut host, &mut runtime, SCHEDULE);
        // Stopped, then scheduled in this window for 7 s.
        let scheduled = IdleScript {
            file: 0,
            window: Some(1),
            script: WHEN_IDLE,
            parameter: None,
            delay: 7.0,
        };
        assert_eq!(host.idle, [None, Some(scheduled.clone())]);
        host.plugin_answers.push_back(answer);
        host.calls.clear();
        fire(&mut host, &mut runtime, &scheduled);
        let dialog = &host.plugin_dialogs[0];
        assert_eq!(dialog.message, "Keep Ada?");
        assert_eq!(dialog.mode, AlertMode::Caution);
        assert_eq!(dialog.buttons, ["Keep", "Drop"]);
        assert_eq!(dialog.timeout, Some(std::time::Duration::from_secs(10)));
        let commit = host.calls.iter().any(|c| c == "commit_records");
        let revert = host.calls.iter().any(|c| c == "revert_record");
        assert_eq!(
            (commit, revert),
            (commits.is_some(), commits.is_none()),
            "{answer:?}"
        );
        assert_eq!(global(&mut runtime, "$$kept"), commits.unwrap_or(""));
    }
}

/// `Files.CreateDirectory`, `Files.Delete` and the DynaPDF commands act
/// only in a step's calculation. Through the runtime's calculation host for evaluations
/// outside a step (a button's parameter, a layout's calculation) they are
/// `?`, as without the plug-in. In a step the file commands reach the
/// host's file operations, which the memory host refuses, and `DynaPDF.New`
/// opens a document.
#[test]
fn the_file_commands_act_only_in_a_steps_calculation() {
    let mut host = host(true);
    let mut runtime = Runtime::new(Options::default());
    let names = names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(PEOPLE);
    let locale = om_calc::Locale::en_us();
    let cx = om_calc::Ctx::new(&locale, om_calc::Date::from_ymd(2026, 1, 1).unwrap());
    for (text, in_step) in [
        (
            r#"MBS ( "Files.CreateDirectory" ; "/Reports" )"#,
            "[MBS] Failed to create directory.",
        ),
        (r#"MBS ( "DynaPDF.New" )"#, "20000"),
        (
            r#"MBS ( "Files.Delete" ; "/Reports/a.pdf" )"#,
            "[MBS] Failed to delete the file.",
        ),
    ] {
        let expr = om_formula::compile(text, &ctx).unwrap().expr;
        let context = host.calc_context();
        {
            let mut outside = runtime.calc_host(&mut host);
            let value = om_calc::eval::evaluate(&expr, &mut outside, &context).value;
            assert_eq!(om_calc::eval::display(&value, &outside), "?", "{text}");
        }
        let value = runtime.evaluate(&mut host, &calc(text)).value;
        assert_eq!(value.to_text(&cx).as_str(), in_step, "{text}");
    }
}

#[test]
fn without_the_plugins_state_the_commands_evaluate_as_without_the_plugin() {
    let mut host = host(false);
    let mut runtime = Runtime::new(Options::default());
    let state = run(&mut host, &mut runtime, SCHEDULE);
    assert!(matches!(state, RunState::Idle), "{state:?}");
    assert!(host.idle.is_empty());
    assert!(host.plugin_dialogs.is_empty());
}
