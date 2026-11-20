//! The script runtime against the memory host: control flow, errors and
//! `Get ( LastError )`, variables and frames, script calls and queue modes,
//! triggers, transactions, server rules (script-runtime.md).

use om_calc::eval::{CalcHost, FieldInfo, Variables};
use om_calc::{Value, ValueType};
use om_scripts::runtime::*;
use om_scripts::{flag, Calculation, Client, FieldSpec, Script, ScriptStep, StoredKey};

const PEOPLE: u64 = 0x10;
const NAME: u64 = 1;
const AGE: u64 = 2;

fn host() -> MemoryScriptHost {
    let mut host = MemoryScriptHost::en_us();
    host.add_table(1, "People");
    host.add_occurrence(PEOPLE, "People", 1);
    host.add_field(1, NAME, "Name", FieldInfo::stored(ValueType::Text, 1));
    host.add_field(1, AGE, "Age", FieldInfo::stored(ValueType::Number, 3));
    for (id, name) in [(1, "Ada"), (2, "Bob"), (3, "Cy")] {
        host.add_record(1, id);
        host.calc.set(1, id, NAME, 1, Value::text(name));
    }
    host.add_layout(1, "List", PEOPLE);
    host.add_layout(2, "Detail", PEOPLE);
    host
}

/// A calculation compiled in the People context.
fn calc(host: &MemoryScriptHost, text: &str) -> Calculation {
    let mut ctx = om_formula::Context::new(host.names());
    ctx.occurrence = Some(PEOPLE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

/// A step with calculation `text` in slot 0.
fn with_calc(host: &MemoryScriptHost, id: u16, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.set_calculation(0, Some(&calc(host, text)));
    step
}

fn set_var(host: &MemoryScriptHost, name: &str, value: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(host, value)));
    step
}

fn set_var_rep(host: &MemoryScriptHost, name: &str, rep: &str, value: &str) -> ScriptStep {
    let mut step = set_var(host, name, value);
    step.set_calculation(2, Some(&calc(host, rep)));
    step
}

fn on(id: u16, on: bool) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.set_flag(flag::TURN_ON, on);
    step
}

fn exit_with(host: &MemoryScriptHost, result: &str) -> ScriptStep {
    with_calc(host, 103, result)
}

fn perform(script: u64) -> ScriptStep {
    let mut step = ScriptStep::new(1);
    step.set_script_key(StoredKey::from_serial_id(script));
    step
}

fn perform_with(host: &MemoryScriptHost, script: u64, parameter: &str) -> ScriptStep {
    let mut step = with_calc(host, 1, parameter);
    step.set_script_key(StoredKey::from_serial_id(script));
    step
}

fn go_to_record(choice: u16) -> ScriptStep {
    let mut step = ScriptStep::new(16);
    step.value = choice;
    step.flags |= flag::MENU_SELECTION;
    step
}

fn go_to_layout(layout: u64) -> ScriptStep {
    let mut step = ScriptStep::new(6);
    step.value = 5;
    step.set_member_key(StoredKey::from_id(layout));
    step
}

fn disabled(mut step: ScriptStep) -> ScriptStep {
    step.set_enabled(false);
    step
}

/// A script with its block indexes computed as the Script Workspace does.
fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    let mut script = Script {
        id,
        name: name.into(),
        ..Default::default()
    };
    script.insert_steps(0, steps).expect("nests");
    script
}

/// Run script `id` of file 0 to the end.
fn run(host: &mut MemoryScriptHost, runtime: &mut Runtime, id: u64) -> RunState {
    runtime
        .perform_script(host, ScriptCall::new(0, id))
        .expect("starts");
    runtime.run(host)
}

/// A file's `$$` variable as text.
fn global(runtime: &mut Runtime, name: &str) -> Option<String> {
    var_text(runtime.globals(0), name, 1)
}

fn var_text(vars: &Variables, name: &str, rep: i64) -> Option<String> {
    let cx_locale = om_calc::Locale::en_us();
    let cx = om_calc::Ctx::new(&cx_locale, om_calc::Date::from_ymd(2026, 1, 1).unwrap());
    vars.get(name, rep).map(|v| v.to_text(&cx).into_string())
}

fn g(runtime: &mut Runtime, name: &str) -> String {
    global(runtime, name).unwrap_or_default()
}

// ----- control flow ------------------------------------------------------------

#[test]
fn if_else_if_else_take_one_branch() {
    for (x, expected) in [(1, "one"), (2, "two"), (3, "other")] {
        let mut h = host();
        let steps = vec![
            set_var(&h, "$x", &x.to_string()),
            with_calc(&h, 68, "$x = 1"),
            set_var(&h, "$$r", "\"one\""),
            with_calc(&h, 125, "$x = 2"),
            set_var(&h, "$$r", "\"two\""),
            ScriptStep::new(69),
            set_var(&h, "$$r", "\"other\""),
            ScriptStep::new(70),
            set_var(&h, "$$after", "1"),
        ];
        h.add_script(0, script(1, "Branches", steps));
        let mut rt = Runtime::default();
        assert_eq!(run(&mut h, &mut rt, 1), RunState::Idle);
        assert_eq!(g(&mut rt, "$$r"), expected);
        assert_eq!(g(&mut rt, "$$after"), "1");
    }
}

#[test]
fn nested_ifs_and_empty_branches() {
    let mut h = host();
    let steps = vec![
        with_calc(&h, 68, "1"),
        with_calc(&h, 68, "0"),
        set_var(&h, "$$bad", "1"),
        ScriptStep::new(70),
        ScriptStep::new(69),
        set_var(&h, "$$bad", "2"),
        ScriptStep::new(70),
        // A taken If with an empty body skips its Else.
        with_calc(&h, 68, "1"),
        ScriptStep::new(69),
        set_var(&h, "$$bad", "3"),
        ScriptStep::new(70),
        set_var(&h, "$$done", "1"),
    ];
    h.add_script(0, script(1, "Nested", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(global(&mut rt, "$$bad"), None);
    assert_eq!(g(&mut rt, "$$done"), "1");
}

#[test]
fn loops_exit_and_nest() {
    let mut h = host();
    let steps = vec![
        set_var(&h, "$i", "0"),
        ScriptStep::new(71),
        set_var(&h, "$i", "$i + 1"),
        set_var(&h, "$j", "0"),
        ScriptStep::new(71),
        set_var(&h, "$j", "$j + 1"),
        set_var(&h, "$$count", "$$count + 1"),
        with_calc(&h, 72, "$j >= 3"),
        ScriptStep::new(73),
        with_calc(&h, 72, "$i >= 4"),
        ScriptStep::new(73),
        set_var(&h, "$$i", "$i"),
    ];
    h.add_script(0, script(1, "Loops", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$count"), "12");
    assert_eq!(g(&mut rt, "$$i"), "4");
}

#[test]
fn disabled_steps_and_comments_are_skipped_and_keep_last_error() {
    let mut h = host();
    let mut comment = ScriptStep::new(89);
    comment.set_text(0, Some("note"));
    let steps = vec![
        on(86, true),
        go_to_record(3), // Previous at the first record: 101
        comment,
        disabled(set_var(&h, "$$x", "1")),
        disabled(with_calc(&h, 68, "1")),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Skips", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(global(&mut rt, "$$x"), None);
    assert_eq!(g(&mut rt, "$$e"), "101");
}

#[test]
fn control_steps_and_condition_errors_leave_last_error() {
    let mut h = host();
    let steps = vec![
        on(86, true),
        go_to_record(3),
        // A condition that fails is `?`, which is true, and does not touch
        // LastError (script-runtime.md 13.3).
        with_calc(&h, 68, "Evaluate ( \"1 +\" )"),
        set_var(&h, "$$e", "Get ( LastError )"),
        set_var(&h, "$$taken", "1"),
        ScriptStep::new(70),
        ScriptStep::new(71),
        with_calc(&h, 72, "1"),
        ScriptStep::new(73),
        set_var(&h, "$$after", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Control", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$taken"), "1");
    assert_eq!(g(&mut rt, "$$e"), "101");
    // Set Variable itself succeeded, so the next read sees 0.
    assert_eq!(g(&mut rt, "$$after"), "0");
}

#[test]
fn go_to_record_exit_after_last_leaves_the_loop() {
    let mut h = host();
    let mut next = go_to_record(4);
    next.flags |= flag::EXIT;
    let steps = vec![
        go_to_record(1),
        ScriptStep::new(71),
        set_var(&h, "$$names", "$$names & People::Name"),
        next,
        ScriptStep::new(73),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Walk", steps));
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Idle);
    assert_eq!(g(&mut rt, "$$names"), "AdaBobCy");
    // The step's 101 is stored after leaving the loop (script-runtime.md
    // 13.16); whether a client shows it is the host's choice (U1).
    assert_eq!(g(&mut rt, "$$e"), "101");
    assert!(h.alerts.iter().all(|a| a.code.0 == 101));
}

// ----- scripts, results, frames --------------------------------------------------

#[test]
fn perform_script_passes_parameters_and_results_and_frames_have_own_locals() {
    let mut h = host();
    h.add_script(
        0,
        script(
            2,
            "Double",
            vec![
                set_var(&h, "$$seen_local", "$x"),
                set_var(&h, "$x", "Get ( ScriptParameter ) * 2"),
                exit_with(&h, "$x"),
            ],
        ),
    );
    let steps = vec![
        set_var(&h, "$x", "\"caller\""),
        perform_with(&h, 2, "21"),
        set_var(&h, "$$result", "Get ( ScriptResult )"),
        set_var(&h, "$$x", "$x"),
        set_var(&h, "$$name", "Get ( ScriptName )"),
        exit_with(&h, "\"top\""),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$seen_local"), "");
    assert_eq!(g(&mut rt, "$$result"), "42");
    assert_eq!(g(&mut rt, "$$x"), "caller");
    assert_eq!(g(&mut rt, "$$name"), "Main");
    assert_eq!(
        rt.final_result().map(|v| format!("{v:?}")),
        Some(format!("{:?}", Value::text("top")))
    );
}

#[test]
fn script_ids_from_128_on_are_serial_keys() {
    // Script 141 is stored under key `80 0d` (file-metadata.md 1); a
    // Perform Script step names it by that key.
    let mut h = host();
    h.add_script(0, script(141, "High", vec![exit_with(&h, "\"high\"")]));
    h.add_script(0, script(300, "Higher", vec![exit_with(&h, "\"higher\"")]));
    let step = perform(141);
    assert_eq!(
        step.script_key().unwrap().segment(),
        Some(&[0x80, 0x0d][..])
    );
    let steps = vec![
        step,
        set_var(&h, "$$a", "Get ( ScriptResult )"),
        perform(300),
        set_var(&h, "$$b", "Get ( ScriptResult )"),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$a"), "high");
    assert_eq!(g(&mut rt, "$$b"), "higher");
}

#[test]
fn exit_script_without_result_keeps_the_previous_result() {
    let mut h = host();
    h.add_script(0, script(2, "Gives", vec![exit_with(&h, "\"first\"")]));
    h.add_script(0, script(3, "Silent", vec![ScriptStep::new(103)]));
    let steps = vec![
        perform(2),
        perform(3),
        set_var(&h, "$$r", "Get ( ScriptResult )"),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$r"), "first");
}

#[test]
fn perform_script_by_name_and_missing_scripts() {
    let mut h = host();
    h.add_file(7, "Other");
    h.add_script(7, script(5, "Remote", vec![set_var(&h, "$$remote", "1")]));
    h.add_script(
        0,
        script(2, "Local Target", vec![set_var(&h, "$$local", "1")]),
    );
    let by_name = |h: &MemoryScriptHost, name: &str| {
        let mut step = ScriptStep::new(1);
        step.set_list_value(1, Some(1));
        step.set_calculation(2, Some(&calc(h, name)));
        step
    };
    let steps = vec![
        on(86, true),
        by_name(&h, "\"local target\""),
        by_name(&h, "\"Other::Remote\""),
        by_name(&h, "\"Other::remote\""),
        set_var(&h, "$$e1", "Get ( LastError )"),
        perform(99),
        set_var(&h, "$$e2", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$local"), "1");
    // The remote script's $$ belongs to its own file.
    assert_eq!(global(&mut rt, "$$remote"), None);
    assert_eq!(var_text(rt.globals(7), "$$remote", 1).as_deref(), Some("1"));
    assert_eq!(g(&mut rt, "$$e1"), "104");
    assert_eq!(g(&mut rt, "$$e2"), "104");
}

#[test]
fn deep_recursion_runs_on_the_heap() {
    let mut h = host();
    let steps = vec![
        with_calc(&h, 68, "GetAsNumber ( Get ( ScriptParameter ) ) < 20000"),
        perform_with(&h, 1, "Get ( ScriptParameter ) + 1"),
        ScriptStep::new(70),
        set_var(
            &h,
            "$$depth",
            "Max ( $$depth ; GetAsNumber ( Get ( ScriptParameter ) ) )",
        ),
    ];
    h.add_script(0, script(1, "Recurse", steps));
    let mut rt = Runtime::default();
    rt.perform_script(
        &mut h,
        ScriptCall::new(0, 1).with_parameter(Value::number(0)),
    )
    .unwrap();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$depth"), "20000");
}

/// Scripts like the stack guard probes of script-runtime.md 13.18: 1
/// "Recurse" passes its parameter on until `$$depth` reaches the start's
/// second value, 2 "Recurse Local" keeps `$$big` in `$v` instead, 3
/// "Recurse Result" keeps it as the result of 4 "Big", 5 "Recurse Mixed"
/// also calls 6 "Nop" before recursing; each records the depth and error
/// of the first call that fails in `$$fail` and `$$failerr` (`$$failnop`
/// for Nop). Script 10 + n starts script n with `$$big` as the parameter
/// of 1 and 5.
fn stack_guard_scripts(h: &mut MemoryScriptHost) {
    let failed = |h: &MemoryScriptHost, fail: &str| {
        vec![
            set_var(h, "$$e", "Get ( LastError )"),
            with_calc(h, 68, &format!("$$e <> 0 and IsEmpty ( {fail} )")),
            set_var(h, fail, "$$depth"),
            set_var(h, &format!("{fail}err"), "$$e"),
            ScriptStep::new(70),
        ]
    };
    let recurse = |h: &MemoryScriptHost, id: u64, name: &str, before: Vec<ScriptStep>, call| {
        let mut steps = vec![set_var(h, "$$depth", "$$depth + 1")];
        steps.extend(before);
        steps.push(with_calc(h, 68, "$$depth < $$max"));
        steps.push(call);
        steps.extend(failed(h, "$$fail"));
        steps.push(ScriptStep::new(70));
        script(id, name, steps)
    };
    let scripts = vec![
        recurse(
            h,
            1,
            "Recurse",
            vec![],
            perform_with(h, 1, "Get ( ScriptParameter )"),
        ),
        recurse(
            h,
            2,
            "Recurse Local",
            vec![set_var(h, "$v", "$$big")],
            perform(2),
        ),
        recurse(h, 3, "Recurse Result", vec![perform(4)], perform(3)),
        script(4, "Big", vec![exit_with(h, "$$big")]),
        recurse(
            h,
            5,
            "Recurse Mixed",
            {
                let mut nop = vec![perform(6)];
                nop.extend(failed(h, "$$failnop"));
                nop
            },
            perform_with(h, 5, "Get ( ScriptParameter )"),
        ),
        script(6, "Nop", vec![ScriptStep::new(103)]),
    ];
    for s in scripts {
        let id = s.id;
        let start = if matches!(id, 1 | 5) {
            perform_with(h, id, "$$big")
        } else {
            perform(id)
        };
        let steps = vec![
            on(86, true),
            set_var(
                h,
                "$$max",
                "GetAsNumber ( GetValue ( Get ( ScriptParameter ) ; 2 ) )",
            ),
            start,
        ];
        h.add_script(0, script(10 + id, &format!("Start {}", s.name), steps));
        h.add_script(0, s);
    }
}

/// Run start script `10 + script` with `$$big` of `length` characters and
/// the parameter "length CR max" (as the probes' start scripts had), and
/// return `$$fail`, `$$failerr`, `$$failnop` and `$$depth`.
fn recurse(options: Options, script: u64, length: usize, max: u32) -> [String; 4] {
    let mut h = host();
    stack_guard_scripts(&mut h);
    let mut rt = Runtime::new(options);
    rt.globals(0)
        .set("$$big", 1, Value::text("x".repeat(length)));
    let parameter = Value::text(format!("{length}\r{max}"));
    rt.perform_script(
        &mut h,
        ScriptCall::new(0, 10 + script).with_parameter(parameter),
    )
    .unwrap();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    ["$$fail", "$$failerr", "$$failnop", "$$depth"].map(|name| g(&mut rt, name))
}

#[test]
fn the_stack_memory_guard_ends_recursion_with_error_2() {
    // FileMaker Server's engine (script-runtime.md 13.14): frames passing a
    // 487,100-character parameter on make the call of the 1,100th frame
    // fail with 2 (1 GiB), one character less the 1,200th; the script goes
    // on and returns through every frame, never holding much more than
    // the limit's worth of parameters.
    let options = Options::default();
    assert_eq!(
        recurse(options.clone(), 1, 487_100, 1300),
        ["1099", "2", "", "1099"]
    );
    assert_eq!(
        recurse(options.clone(), 1, 487_099, 1300),
        ["1199", "2", "", "1199"]
    );
    // The same value kept in a `$` variable, without a parameter: the
    // engine's boundary lies between 487,075 and 487,085 characters.
    assert_eq!(
        recurse(options.clone(), 2, 487_085, 1300),
        ["1099", "2", "", "1099"]
    );
    assert_eq!(
        recurse(options, 2, 487_075, 1300),
        ["1199", "2", "", "1199"]
    );
}

#[test]
fn the_stack_memory_guard_counts_frames_parameters_and_variables_every_100th_call() {
    // A 64 MiB limit and 100,000-character values: 300 frames with the
    // value hold about 61 MB, 350 about 71 MB.
    let options = Options {
        stack_memory_limit: 64 << 20,
        ..Options::default()
    };
    // The guard looks at the 1st, 101st, ... call made with 100 or more
    // frames on the stack: the 400th frame's.
    assert_eq!(
        recurse(options.clone(), 1, 100_000, 500),
        ["399", "2", "", "399"]
    );
    // Two calls per frame: every 50th frame, always at the Nop call, so
    // the recursion itself never fails (as in the engine's run).
    assert_eq!(
        recurse(options.clone(), 5, 100_000, 500),
        ["", "", "349", "500"]
    );
    // A `$` variable counts like a parameter; script results do not.
    assert_eq!(
        recurse(options.clone(), 2, 100_000, 500),
        ["399", "2", "", "399"]
    );
    assert_eq!(recurse(options, 3, 100_000, 500), ["", "", "", "500"]);
}

#[test]
fn halt_script_ends_every_frame_and_resets_states() {
    let mut h = host();
    h.add_script(
        0,
        script(
            2,
            "Stopper",
            vec![on(86, true), on(85, false), ScriptStep::new(90)],
        ),
    );
    let steps = vec![perform(2), set_var(&h, "$$after", "1")];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(global(&mut rt, "$$after"), None);
    assert!(!rt.error_capture() && rt.allow_abort() && rt.revert_on_error());
    assert!(!rt.is_running());
}

#[test]
fn capture_and_abort_states_persist_after_a_sub_script_and_reset_at_the_end() {
    let mut h = host();
    h.add_script(0, script(2, "Capture", vec![on(86, true)]));
    let steps = vec![
        perform(2),
        set_var(&h, "$$capture", "Get ( ErrorCaptureState )"),
        go_to_record(3),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$capture"), "1");
    assert!(h.alerts.is_empty(), "captured");
    assert!(!rt.error_capture());
}

// ----- errors ----------------------------------------------------------------------

#[test]
fn errors_are_alerted_without_capture_and_cleared_by_the_next_step() {
    let mut h = host();
    let steps = vec![
        go_to_record(3),
        set_var(&h, "$$e", "Get ( LastError )"),
        set_var(&h, "$$loc", "Get ( LastErrorLocation )"),
        set_var(&h, "$$after", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Errors", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(h.alerts.len(), 1);
    assert_eq!(h.alerts[0].code, ErrorCode::RECORD_MISSING);
    assert_eq!(h.alerts[0].line, 1);
    assert_eq!(g(&mut rt, "$$e"), "101");
    assert_eq!(g(&mut rt, "$$loc"), "");
    assert_eq!(g(&mut rt, "$$after"), "0");
    // The location names the failing step while LastError holds it.
    let mut h = host();
    h.add_script(0, script(1, "Where", vec![on(86, true), go_to_record(3)]));
    let mut rt = Runtime::default();
    rt.perform_script(&mut h, ScriptCall::new(0, 1)).unwrap();
    rt.run(&mut h);
    let location = rt.last_error_location().expect("location");
    assert_eq!((location.script.as_str(), location.line), ("Where", 2));
    assert_eq!(location.step_name, "Go to Record/Request/Page");
}

#[test]
fn canceling_an_alert_halts_the_script() {
    let mut h = host();
    h.alert_choice = AlertChoice::Cancel;
    let steps = vec![go_to_record(3), set_var(&h, "$$after", "1")];
    h.add_script(0, script(1, "Cancel", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(global(&mut rt, "$$after"), None);
}

#[test]
fn unknown_steps_stop_with_allow_user_abort_and_are_skipped_without() {
    for (abort, expected) in [(true, None), (false, Some("4"))] {
        let mut h = host();
        let steps = vec![
            on(85, abort),
            ScriptStep::new(15), // obsolete
            set_var(&h, "$$e", "Get ( LastError )"),
        ];
        h.add_script(0, script(1, "Unknown", steps));
        let mut rt = Runtime::default();
        run(&mut h, &mut rt, 1);
        assert_eq!(global(&mut rt, "$$e").as_deref(), expected);
        assert_eq!(h.alerts.len(), 1);
        assert!(h.alerts[0].confirm);
    }
}

#[test]
fn unsupported_host_steps_are_error_3_or_stop() {
    let mut h = host();
    // Beep is implemented by the memory host; Exit Application is not.
    let steps = vec![
        on(86, true),
        ScriptStep::new(93),
        ScriptStep::new(44),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Exit", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$e"), "3");

    let mut rt = Runtime::new(Options {
        stop_on_unsupported: true,
        ..Options::default()
    });
    let state = run(&mut h, &mut rt, 1);
    let RunState::Stopped(Stop::Unsupported(at)) = state else {
        panic!("{state:?}")
    };
    assert_eq!((at.line, at.step), (3, 44));
    assert_eq!(rt.last_error(), ErrorCode::UNAVAILABLE);
}

#[test]
fn set_error_logging_logs_with_custom_info() {
    let mut h = host();
    let mut logging = with_calc(&h, 200, "\"debug \" & Get ( RecordID )");
    logging.flags |= flag::OPTION;
    let steps = vec![on(86, true), logging, go_to_record(3)];
    h.add_script(0, script(1, "Log", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    let entry = h.log.iter().find(|e| e.script_error_log).expect("entry");
    assert_eq!((entry.code, entry.line), (ErrorCode::RECORD_MISSING, 3));
    assert!(format!("{:?}", entry.custom).contains("debug 1"));
}

// ----- variables -------------------------------------------------------------------

#[test]
fn variables_repetitions_names_and_deletion() {
    let mut h = host();
    let steps = vec![
        on(86, true),
        set_var_rep(&h, "$$v", "2", "\"two\""),
        set_var(&h, "$$v", "\"one\""),
        set_var(&h, "$$gone", "1"),
        set_var(&h, "$$gone", "\"\""),
        set_var(&h, "bad name", "1"),
        set_var(&h, "$$e", "Get ( LastError )"),
        set_var(&h, "$$let", "Let ( $$fromlet = 7 ; 1 )"),
        set_var(&h, "$$typed", "Date ( 1 ; 2 ; 2026 )"),
    ];
    h.add_script(0, script(1, "Vars", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    let globals = rt.globals(0);
    assert_eq!(var_text(globals, "$$V", 2).as_deref(), Some("two"));
    assert_eq!(var_text(globals, "$$v", 1).as_deref(), Some("one"));
    assert_eq!(var_text(globals, "$$gone", 1), None);
    assert_eq!(var_text(globals, "$$e", 1).as_deref(), Some("5"));
    assert_eq!(var_text(globals, "$$fromlet", 1).as_deref(), Some("7"));
    assert_eq!(
        globals.get("$$typed", 1).map(Value::value_type),
        Some(ValueType::Date)
    );
}

#[test]
fn script_parameters_and_results_keep_their_type() {
    let mut h = host();
    h.add_script(
        0,
        script(
            2,
            "Types",
            vec![
                set_var(&h, "$$p", "Get ( ScriptParameter ) + 1"),
                exit_with(&h, "Date ( 1 ; 2 ; 2026 )"),
            ],
        ),
    );
    let steps = vec![
        perform_with(&h, 2, "Date ( 1 ; 2 ; 2026 )"),
        set_var(&h, "$$r", "Get ( ScriptResult )"),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    // A date parameter plus 1 is the next day (script-runtime.md 13.5).
    assert_eq!(g(&mut rt, "$$p"), "1/3/2026");
    assert_eq!(
        rt.globals(0).get("$$r", 1).map(Value::value_type),
        Some(ValueType::Date)
    );
}

// ----- host steps --------------------------------------------------------------------

#[test]
fn navigation_records_and_fields_reach_the_host() {
    let mut h = host();
    let mut set_name = with_calc(&h, 76, "\"Dee\"");
    set_name.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::new(&[3, 0, 0, 0x10]).unwrap(),
            StoredKey::from_id(NAME).unwrap(),
            1,
        )),
    );
    let mut by_name = ScriptStep::new(147);
    by_name.set_calculation(1, Some(&calc(&h, "\"People::Age\"")));
    by_name.set_calculation(0, Some(&calc(&h, "40")));
    let steps = vec![
        go_to_layout(2),
        ScriptStep::new(7),
        set_name,
        by_name,
        set_var(&h, "$$count", "Get ( FoundCount )"),
        set_var(&h, "$$layout", "Get ( LayoutName )"),
        go_to_record(1),
        ScriptStep::new(9),
        set_var(&h, "$$first", "People::Name"),
    ];
    h.add_script(0, script(1, "Edit", steps));
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Idle);
    assert!(h.alerts.is_empty(), "{:?}", h.alerts);
    assert_eq!(g(&mut rt, "$$count"), "4");
    assert_eq!(g(&mut rt, "$$layout"), "Detail");
    assert_eq!(g(&mut rt, "$$first"), "Bob");
    let window = h.window().unwrap();
    assert_eq!(window.found, [2, 3, 4]);
    assert_eq!(age_is(&h, 4), "40");
}

/// FileMaker Pro 26.1.1's steps by name (script-steps.md 12.4, #349):
/// Set Variable by Name sets the variable its calculation names, at the
/// repetition asked for, and a name that is not a variable's is error 5;
/// Replace Field Contents by Name replaces the field its calculation names
/// in every record of the found set, and a name that names no field is
/// error 102 [I: both inferred from Set Variable and Set Field By Name].
#[test]
fn steps_by_name_find_their_variable_and_field() {
    let mut h = host();
    let set = |h: &MemoryScriptHost, name: &str, rep: Option<&str>, value: &str| {
        let mut step = ScriptStep::new(250);
        step.set_calculation(3, Some(&calc(h, name)));
        if let Some(rep) = rep {
            step.set_calculation(2, Some(&calc(h, rep)));
        }
        step.set_calculation(1, Some(&calc(h, value)));
        step
    };
    let replace = |h: &MemoryScriptHost, name: &str, value: &str| {
        let mut step = ScriptStep::new(249);
        step.set_flag(flag::NO_INTERACT, true);
        step.set_calculation(1, Some(&calc(h, name)));
        step.set_number(1, Some(3));
        step.set_calculation(0, Some(&calc(h, value)));
        step
    };
    let steps = vec![
        set(&h, "\"$$by\" & \"Name\"", Some("2"), "41"),
        set(&h, "\"$$plain\"", None, "\"x\""),
        go_to_layout(2),
        ScriptStep::new(7),
        replace(&h, "\"People::Age\"", "Length ( People::Name ) * 10"),
        set_var(&h, "$$replaced", "Get ( LastError )"),
        replace(&h, "\"People::Height\"", "1"),
        set_var(&h, "$$missing", "Get ( LastError )"),
        set(&h, "\"name\"", None, "1"),
        set_var(&h, "$$bad", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "By name", steps));
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Idle);
    assert_eq!(
        var_text(rt.globals(0), "$$byName", 2).as_deref(),
        Some("41")
    );
    assert_eq!(var_text(rt.globals(0), "$$byName", 1), None);
    assert_eq!(g(&mut rt, "$$plain"), "x");
    assert_eq!(g(&mut rt, "$$replaced"), "0");
    assert_eq!(
        [age_is(&h, 1), age_is(&h, 2), age_is(&h, 3)],
        ["30", "30", "20"]
    );
    assert_eq!(g(&mut rt, "$$missing"), "102");
    assert_eq!(g(&mut rt, "$$bad"), "5");

    // "Use Entry Options" takes the field's own serial settings, which are
    // not read: refused with 3 before any record is written.
    let mut entry = replace(&h, "\"People::Age\"", "1");
    entry.set_number(1, Some(2));
    entry.set_number(5, Some(1));
    let steps = vec![
        go_to_layout(2),
        entry,
        set_var(&h, "$$entry", "Get ( LastError )"),
    ];
    h.add_script(0, script(2, "Entry options", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 2);
    assert_eq!(g(&mut rt, "$$entry"), "3");
    assert_eq!(
        [age_is(&h, 1), age_is(&h, 2), age_is(&h, 3)],
        ["30", "30", "20"]
    );

    // On the server 249 is unknown (FileMaker Server 26.0.3's table): its
    // error 4 does not fail an open transaction, whose write commits. With
    // Allow User Abort on, error 4 would halt the script instead.
    let mut h = host();
    let steps = vec![
        on(85, false),
        on(86, true),
        ScriptStep::new(205),
        set_age(&h, "30", 1),
        replace(&h, "\"People::Age\"", "1"),
        set_var(&h, "$$unknown", "Get ( LastError )"),
        ScriptStep::new(206),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Server", steps));
    let mut rt = Runtime::new(Options::new(Client::Server));
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$unknown"), "4");
    assert_eq!(g(&mut rt, "$$e"), "0");
    assert_eq!(age_is(&h, 1), "30");
}

#[test]
fn insert_steps_write_variables_and_fields() {
    let mut h = host();
    let variable = |id: u16, h: &MemoryScriptHost, name: &str, text: Option<&str>, select: bool| {
        let mut step = match text {
            Some(t) => with_calc(h, id, t),
            None => ScriptStep::new(id),
        };
        step.set_variable_target(0, name);
        step.set_flag(flag::SELECT_ALL, select);
        step
    };
    let steps = vec![
        variable(77, &h, "$$t", Some("\"a\""), true),
        variable(77, &h, "$$t", Some("\"b\""), false),
        variable(13, &h, "$$date", None, true),
    ];
    h.add_script(0, script(1, "Insert", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$t"), "ab");
    assert!(global(&mut rt, "$$date").is_some());
}

#[test]
fn send_mail_reads_the_message_and_its_options_from_filemakers_slots() {
    // script-steps.md 12.3: To, CC, BCC, Subject and Message are
    // calculations 0-4; Collect `[00 0..2, 03]`, SMTP `[00 4, 03]`.
    let mut h = host();
    let mut mail = ScriptStep::new(63);
    for (slot, text) in [
        (0, "\"pat@example.com\""),
        (3, "\"Report \" & 1"),
        (4, "\"Hi\""),
    ] {
        mail.set_calculation(slot, Some(&calc(&h, text)));
    }
    mail.set_number_bool(0, Some(true));
    let mut smtp = mail.clone();
    smtp.set_number_bool(4, Some(true));
    smtp.set_flag(flag::NO_INTERACT, true);
    // What OpenMaker cannot read is 3, no message sent: an SMTP selector
    // of a width no number has, and attachments flagged (`kFilePath`)
    // with no list.
    let mut unreadable = mail.clone();
    unreadable.set_slot_child(4, 0x03, Some(OwnedNode::leaf([0x03], [1, 0, 0])));
    let mut attached = mail.clone();
    attached.set_flag(flag::FILE_PATH, true);
    let steps = vec![
        on(86, true),
        mail,
        smtp,
        unreadable,
        set_var(&h, "$$unreadable", "Get ( LastError )"),
        attached,
        set_var(&h, "$$attached", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Mail", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$unreadable"), "3");
    assert_eq!(g(&mut rt, "$$attached"), "3");
    assert_eq!(
        h.mails,
        [
            Mail {
                via: MailVia::Client,
                to: "pat@example.com".into(),
                cc: String::new(),
                bcc: String::new(),
                subject: "Report 1".into(),
                message: "Hi".into(),
                collect: [true, false, false],
                one_per_record: false,
                attachments: String::new(),
                with_dialog: true,
            },
            Mail {
                via: MailVia::Smtp,
                with_dialog: false,
                ..h.mails[0].clone()
            },
        ]
    );
}

#[test]
fn custom_dialogs_store_inputs_and_the_choice() {
    let mut h = host();
    let mut dialog = with_calc(&h, 87, "\"Title\"");
    dialog.set_calculation(1, Some(&calc(&h, "\"Message\"")));
    dialog.set_text(0, Some("OK"));
    dialog.set_text(1, Some("Cancel"));
    dialog.set_number(3, Some(1));
    dialog.set_number(4, Some(0));
    dialog.set_variable_target(0, "$$answer");
    dialog.set_calculation(2, Some(&calc(&h, "\"Your name\"")));
    // FileMaker Pro 26.1's "Close after (seconds)": on in numeric slot 6,
    // the seconds in calculation slot 13 (script-steps.md 12.4).
    let mut timed = dialog.clone();
    timed.set_number_bool(6, Some(true));
    timed.set_calculation(13, Some(&calc(&h, "1 + 1")));
    let steps = vec![
        set_var(&h, "$$answer", "\"old\""),
        dialog.clone(),
        set_var(&h, "$$choice", "Get ( LastMessageChoice )"),
        dialog,
        set_var(&h, "$$choice2", "Get ( LastMessageChoice )"),
        timed,
        set_var(&h, "$$choice3", "Get ( LastMessageChoice )"),
    ];
    h.add_script(0, script(1, "Ask", steps));
    h.dialog_answers.push_back(DialogResponse {
        button: 1,
        inputs: vec![Value::text("Grace")],
    });
    h.dialog_answers.push_back(DialogResponse {
        button: 2,
        inputs: vec![Value::text("ignored")],
    });
    // Closed by its timer: no button, so no input is stored.
    h.dialog_answers.push_back(DialogResponse {
        button: 0,
        inputs: vec![Value::text("timed out")],
    });
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(
        h.dialogs.iter().map(|d| d.close_after).collect::<Vec<_>>(),
        [None, None, Some(2.0)]
    );
    assert_eq!(g(&mut rt, "$$choice3"), "0");
    assert_eq!(h.dialogs[0].title, "Title");
    assert_eq!(h.dialogs[0].inputs[0].label, "Your name");
    assert_eq!(h.dialogs[1].inputs[0].value.value_type(), ValueType::Text);
    assert_eq!(g(&mut rt, "$$answer"), "Grace");
    assert_eq!(g(&mut rt, "$$choice"), "1");
    assert_eq!(g(&mut rt, "$$choice2"), "2");
}

/// FileMaker Pro keeps each unused input of Show Custom Dialog as a cleared
/// field spec (#940): the dialog shows no input for it and OK writes
/// nothing, so it does not fail with 102.
#[test]
fn custom_dialogs_leave_out_the_inputs_pro_keeps_unused() {
    let mut h = host();
    let unused = FieldSpec::from_bytes(&[
        0x02, 0x01, 0x00, 0x00, 0xf0, 0x4a, 0x02, 0x01, 0x00, 0x00, 0x79, 0x00, 0x00, 0x01,
    ])
    .unwrap();
    let mut dialog = with_calc(&h, 87, "\"Title\"");
    dialog.set_calculation(1, Some(&calc(&h, "\"Message\"")));
    for slot in 0..3 {
        dialog.set_field_spec(slot, Some(unused));
    }
    let steps = vec![dialog, set_var(&h, "$$error", "Get ( LastError )")];
    h.add_script(0, script(1, "Ask", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert!(h.dialogs[0].inputs.is_empty(), "{:?}", h.dialogs[0].inputs);
    assert_eq!(g(&mut rt, "$$error"), "0");
}

#[test]
fn data_files_round_trip_through_variables() {
    let mut h = host();
    let mut create = ScriptStep::new(190);
    set_path(&mut create, "file:notes.txt");
    let mut open = ScriptStep::new(191);
    set_path(&mut open, "$$path");
    open.set_variable_target(0, "$$id");
    let mut write = with_calc(&h, 192, "$$id");
    write.set_variable_target(0, "$$text");
    write.flags |= flag::OPTION2;
    let mut seek = ScriptStep::new(196);
    seek.set_calculation(0, Some(&calc(&h, "$$id")));
    let mut reopen = ScriptStep::new(191);
    set_path(&mut reopen, "file:notes.txt");
    reopen.set_variable_target(0, "$$id");
    let mut read = with_calc(&h, 193, "$$id");
    read.set_variable_target(0, "$$read");
    let steps = vec![
        set_var(&h, "$$path", "\"file:notes.txt\""),
        set_var(&h, "$$text", "\"hello\""),
        create,
        open,
        write,
        seek,
        reopen,
        read,
    ];
    h.add_script(0, script(1, "Files", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert!(h.alerts.is_empty(), "{:?}", h.alerts);
    assert_eq!(g(&mut rt, "$$read"), "hello\n");
}

#[test]
fn path_lists_expand_variables_anywhere_and_by_lines() {
    // Claris "Creating file paths": `file:testing/$fileName`,
    // `$chosenType:/$driveLetter/...`, a variable of several paths.
    let mut h = host();
    let mut create = ScriptStep::new(190);
    set_path(&mut create, "file:testing/$fileName");
    let mut by_parts = ScriptStep::new(190);
    set_path(
        &mut by_parts,
        "$chosenType:/$driveLetter/$targetDir/contacts.txt",
    );
    let mut list = ScriptStep::new(190);
    set_path(&mut list, "$$searchList");
    let mut dollars = ScriptStep::new(190);
    set_path(&mut dollars, "file:cost$ 5.txt");
    let steps = vec![
        set_var(&h, "$fileName", "\"test.xlsx\""),
        set_var(&h, "$chosenType", "\"filewin\""),
        set_var(&h, "$driveLetter", "\"G:\""),
        set_var(&h, "$targetDir", "\"Documents/Clients\""),
        set_var(
            &h,
            "$$searchList",
            "\"file:old results.txt¶file:../archived/old results.txt\"",
        ),
        create,
        by_parts,
        list,
        dollars,
    ];
    h.add_script(0, script(1, "Paths", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert!(h.alerts.is_empty(), "{:?}", h.alerts);
    let mut created: Vec<&str> = h.data_files.keys().map(String::as_str).collect();
    created.sort_unstable();
    // The memory host creates the first path of each list.
    assert_eq!(
        created,
        [
            "file:cost$ 5.txt",
            "file:old results.txt",
            "file:testing/test.xlsx",
            "filewin:/G:/Documents/Clients/contacts.txt",
        ]
    );
}

/// A path list, as a step-level `82`.
fn set_path(step: &mut ScriptStep, path: &str) {
    step.set_data_leaf(0x82, Some(om_scripts::text_node(&[0x82], path)));
}

#[test]
fn pending_steps_wait_for_the_host() {
    let mut h = host();
    h.pending_new_records = 1;
    let steps = vec![
        ScriptStep::new(7),
        set_var(&h, "$$after", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Async", steps));
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Waiting);
    assert!(rt.is_waiting());
    assert_eq!(global(&mut rt, "$$after"), None);
    rt.complete_step(ErrorCode::NONE);
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$after"), "0");
    // Without a completion the step is entered again.
    h.pending_new_records = 1;
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Waiting);
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(h.calls.iter().filter(|c| *c == "new_record").count(), 3);
}

#[test]
fn pause_waits_until_resumed() {
    let mut h = host();
    h.pause_waits = true;
    let mut pause = with_calc(&h, 62, "2");
    pause.flags |= flag::CALCULATION;
    let steps = vec![pause, set_var(&h, "$$after", "1")];
    h.add_script(0, script(1, "Pause", steps));
    let mut rt = Runtime::default();
    assert_eq!(
        run(&mut h, &mut rt, 1),
        RunState::Paused { seconds: Some(2.0) }
    );
    assert!(rt.is_paused());
    rt.resume();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$after"), "1");
}

#[test]
fn queue_modes_of_host_started_scripts() {
    let mut h = host();
    h.pause_waits = true;
    let steps = vec![ScriptStep::new(62), set_var(&h, "$$first", "1")];
    h.add_script(0, script(1, "Paused", steps));
    h.add_script(0, script(2, "Second", vec![set_var(&h, "$$second", "1")]));
    // Halt: the paused script never finishes.
    let mut rt = Runtime::default();
    assert!(matches!(run(&mut h, &mut rt, 1), RunState::Paused { .. }));
    let mut call = ScriptCall::new(0, 2);
    call.mode = QueueMode::Halt;
    rt.perform_script(&mut h, call).unwrap();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$second"), "1");
    assert_eq!(global(&mut rt, "$$first"), None);
    // Resume: the new script runs, then the paused one goes on.
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    let mut call = ScriptCall::new(0, 2);
    call.mode = QueueMode::Resume;
    rt.perform_script(&mut h, call).unwrap();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$first"), "1");
    // Pause: the paused script stays paused after the new one.
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    let mut call = ScriptCall::new(0, 2);
    call.mode = QueueMode::Pause;
    rt.perform_script(&mut h, call).unwrap();
    assert!(matches!(rt.run(&mut h), RunState::Paused { .. }));
    assert_eq!(g(&mut rt, "$$second"), "1");
}

#[test]
fn button_steps_run_one_step_or_a_script_with_the_buttons_parameter() {
    let mut h = host();
    h.add_script(
        0,
        script(
            2,
            "Param",
            vec![set_var(&h, "$$p", "Get ( ScriptParameter )")],
        ),
    );
    let mut rt = Runtime::default();
    rt.perform_step(&mut h, ScriptStep::new(7), None).unwrap();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(h.window().unwrap().found.len(), 4);
    // A layout button's Perform Script: reference without kScriptKey.
    let mut button = ScriptStep::new(1);
    button.reserved[..4].copy_from_slice(&[2, 1, 2, 0xbe]);
    rt.perform_step(&mut h, button, Some(Value::text("First Name")))
        .unwrap();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$p"), "First Name");
}

#[test]
fn a_step_budget_stops_endless_loops() {
    let mut h = host();
    h.add_script(
        0,
        script(1, "Forever", vec![ScriptStep::new(71), ScriptStep::new(73)]),
    );
    let mut rt = Runtime::new(Options {
        step_budget: Some(1000),
        ..Options::default()
    });
    assert_eq!(
        run(&mut h, &mut rt, 1),
        RunState::Stopped(Stop::Budget(1000))
    );
    assert!(!rt.is_running());
    // The budget is per top-level script, not per runtime.
    let steps = vec![
        set_var(&h, "$i", "0"),
        ScriptStep::new(71),
        set_var(&h, "$i", "$i + 1"),
        with_calc(&h, 72, "$i >= 200"),
        ScriptStep::new(73),
    ];
    h.add_script(0, script(2, "Bounded", steps));
    let mut rt = Runtime::new(Options {
        step_budget: Some(1000),
        ..Options::default()
    });
    for _ in 0..3 {
        assert_eq!(run(&mut h, &mut rt, 2), RunState::Idle);
    }
    assert!(rt.steps_run() > 1000);
    // Without a budget a time slice yields instead.
    let mut rt = Runtime::new(Options {
        time_slice: Some(std::time::Duration::from_millis(5)),
        ..Options::default()
    });
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Yielded);
    rt.halt(&mut h);
    assert!(!rt.is_running());
}

#[test]
fn go_to_related_record_takes_its_table_by_calculation() {
    // FileMaker Pro 26.1's "Table by Calculation": `0x400` and the
    // occurrence's name in calculation slot 8 (script-steps.md 12.4).
    let mut h = host();
    let mut by_name = ScriptStep::new(74);
    by_name.set_flag(flag::OPTION3, true);
    by_name.set_calculation(8, Some(&calc(&h, "\"Peo\" & \"ple\"")));
    let mut missing = by_name.clone();
    missing.set_calculation(8, Some(&calc(&h, "\"Nowhere\"")));
    let steps = vec![
        on(86, true),
        by_name,
        missing,
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Related", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    let calls: Vec<_> = h
        .calls
        .iter()
        .filter(|c| c.starts_with("go_to_related_record"))
        .collect();
    assert_eq!(calls, [&format!("go_to_related_record Some({PEOPLE})")]);
    // A name no occurrence has: 106, the host not asked.
    assert_eq!(g(&mut rt, "$$e"), "106");
}

#[test]
fn go_to_list_of_records_reads_filemaker_slots() {
    // #189: the ids are calculation slot 7, and `kTurnOn` asks for the new
    // window named by slot 0, which Pro keeps when New Window is turned
    // off (script-steps.md 12.2). `0x100`, read before, opens none.
    let mut h = host();
    let mut list = ScriptStep::new(228);
    list.set_calculation(0, Some(&calc(&h, "\"Kept name\"")));
    list.set_calculation(7, Some(&calc(&h, "\"2¶3\"")));
    list.set_flag(flag::OPTION, true);
    let mut windowed = list.clone();
    windowed.set_flag(flag::TURN_ON, true);
    windowed.set_calculation(1, Some(&calc(&h, "300")));
    h.add_script(0, script(1, "Client", vec![list, windowed]));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    let lists: Vec<_> = h
        .record_lists
        .iter()
        .map(|r| {
            let window = r.new_window.as_ref();
            (
                r.ids.as_str(),
                window.and_then(|w| w.name.as_deref()),
                window.and_then(|w| w.bounds.height),
            )
        })
        .collect();
    assert_eq!(
        lists,
        [
            ("2\r3", None, None),
            ("2\r3", Some("Kept name"), Some(300.0))
        ]
    );
}

// ----- triggers ----------------------------------------------------------------------

#[test]
fn layout_exit_trigger_cancels_go_to_layout_with_false() {
    for (result, layout, error) in [("0", 1, "20"), ("1", 2, "0")] {
        let mut h = host();
        h.layout_triggers
            .insert((1, TriggerEvent::LayoutExit), (0, 9));
        h.layout_triggers
            .insert((2, TriggerEvent::LayoutEnter), (0, 10));
        h.add_script(0, script(9, "OnExit", vec![exit_with(&h, result)]));
        h.add_script(
            0,
            script(
                10,
                "OnEnter",
                vec![set_var(&h, "$$entered", "$$entered + 1")],
            ),
        );
        let steps = vec![
            on(86, true),
            go_to_layout(2),
            set_var(&h, "$$e", "Get ( LastError )"),
            set_var(&h, "$$layout", "Get ( LayoutName )"),
            set_var(&h, "$$entered_during", "$$entered"),
        ];
        h.add_script(0, script(1, "Switch", steps));
        let mut rt = Runtime::default();
        assert_eq!(run(&mut h, &mut rt, 1), RunState::Idle);
        assert_eq!(h.window().unwrap().layout, layout);
        assert_eq!(g(&mut rt, "$$e"), error);
        if layout == 2 {
            assert_eq!(g(&mut rt, "$$layout"), "Detail");
            // OnLayoutEnter, fired by the step, ran right after it
            // (script-runtime.md 13.12).
            assert_eq!(g(&mut rt, "$$entered_during"), "1");
            assert_eq!(g(&mut rt, "$$entered"), "1");
        }
    }
}

#[test]
fn a_step_held_by_its_trigger_runs_again_when_the_trigger_proceeds() {
    // Go to Layout fires OnLayoutExit, whose script ends after a failing
    // step without Exit Script: the step returns 3000 (neither stored nor
    // alerted), the trigger starts with Get ( LastError ) 0, and Go to
    // Layout runs again when the trigger lets the event happen, storing
    // its own 0 (script-runtime.md 13.17).
    let mut h = host();
    h.layout_triggers
        .insert((1, TriggerEvent::LayoutExit), (0, 9));
    h.add_script(
        0,
        script(
            9,
            "OnExit",
            vec![set_var(&h, "$$seen", "Get ( LastError )"), perform(77)],
        ),
    );
    let steps = vec![
        perform(77),
        go_to_layout(2),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Switch", steps));
    let mut rt = Runtime::default();
    rt.set_tracing(true);
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Idle);
    assert_eq!(h.window().unwrap().layout, 2);
    assert_eq!(g(&mut rt, "$$seen"), "0");
    assert_eq!(g(&mut rt, "$$e"), "0");
    let codes: Vec<i32> = h.alerts.iter().map(|a| a.code.0).collect();
    assert_eq!(codes, [104, 104]);
    let switch: Vec<(usize, i32)> = rt
        .take_trace()
        .iter()
        .filter(|t| t.script == "Switch" && t.step == 6)
        .map(|t| (t.line, t.error.0))
        .collect();
    assert_eq!(switch, [(2, 3000), (2, 0)]);
}

#[test]
fn trigger_firing_rules() {
    let mut h = host();
    h.add_script(0, script(9, "T", vec![set_var(&h, "$$ran", "$$ran + 1")]));
    let mut rt = Runtime::default();
    // Mode filter: a Browse-only trigger in Find mode.
    let mut trigger = Trigger::new(TriggerEvent::RecordLoad, 0, 9);
    trigger.modes = 1;
    trigger.mode = Mode::Find;
    assert_eq!(
        rt.fire_trigger(&mut h, trigger),
        Fired::Suppressed(Suppressed::Mode)
    );
    // A window event while a script is paused.
    h.pause_waits = true;
    h.add_script(0, script(1, "Paused", vec![ScriptStep::new(62)]));
    run(&mut h, &mut rt, 1);
    let event = Trigger::new(TriggerEvent::WindowClose, 0, 9);
    assert_eq!(
        rt.fire_trigger(&mut h, event),
        Fired::Suppressed(Suppressed::ScriptRunning)
    );
    // An object trigger runs on top of the paused script.
    let mut keystroke = Trigger::new(TriggerEvent::ObjectKeystroke, 0, 9);
    keystroke.action = Some(77);
    keystroke.info.keystroke = Some("a".into());
    assert_eq!(rt.fire_trigger(&mut h, keystroke), Fired::Started);
    assert!(matches!(rt.run(&mut h), RunState::Paused { .. }));
    assert_eq!(g(&mut rt, "$$ran"), "1");
    assert_eq!(
        h.trigger_actions.last(),
        Some(&TriggerAction {
            event: TriggerEvent::ObjectKeystroke,
            action: 77,
            proceed: true
        })
    );
    // A missing script does not fire; the event goes ahead.
    let mut missing = Trigger::new(TriggerEvent::RecordCommit, 0, 404);
    missing.action = Some(5);
    assert_eq!(
        rt.fire_trigger(&mut h, missing),
        Fired::Suppressed(Suppressed::CallFailed(104))
    );
    assert!(h.trigger_actions.last().unwrap().proceed);
}

#[test]
fn trigger_scripts_see_their_event_and_restore_the_callers_capture() {
    let mut h = host();
    h.layout_triggers
        .insert((1, TriggerEvent::RecordCommit), (0, 9));
    h.add_script(
        0,
        script(
            9,
            "OnCommit",
            vec![on(86, false), set_var(&h, "$$p", "Get ( ScriptParameter )")],
        ),
    );
    let mut commit = ScriptStep::new(75);
    commit.flags |= flag::NO_INTERACT;
    let steps = vec![
        on(86, true),
        commit,
        set_var(&h, "$$capture", "Get ( ErrorCaptureState )"),
    ];
    h.add_script(0, script(1, "Commit", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$capture"), "1");
    assert!(h
        .trigger_actions
        .iter()
        .any(|a| a.event == TriggerEvent::RecordCommit && a.proceed));
}

// ----- transactions ------------------------------------------------------------------

/// A record's Age as text (empty when unset).
fn age_is(host: &MemoryScriptHost, record: u64) -> String {
    let value = host.calc.field_value(PEOPLE, record, AGE, 1).unwrap();
    let cx = om_calc::Ctx::new(host.locale(), host.today());
    value.to_text(&cx).into_string()
}

fn set_age(h: &MemoryScriptHost, value: &str, repetition: i16) -> ScriptStep {
    let mut step = with_calc(h, 76, value);
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::new(&[3, 0, 0, 0x10]).unwrap(),
            StoredKey::from_id(AGE).unwrap(),
            repetition,
        )),
    );
    step
}

#[test]
fn transactions_commit_or_revert_on_error() {
    // A clean transaction commits.
    let mut h = host();
    let steps = vec![
        ScriptStep::new(205),
        set_age(&h, "30", 1),
        ScriptStep::new(206),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Ok", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(age_is(&h, 1), "30");
    assert_eq!(g(&mut rt, "$$e"), "0");
    assert!(h.calls.contains(&"commit_transaction".to_owned()));

    // An error skips to Commit Transaction, which reverts and returns it.
    let mut h = host();
    let steps = vec![
        on(86, true),
        ScriptStep::new(205),
        set_var(&h, "$$open", "Get ( TransactionOpenState )"),
        set_age(&h, "30", 1),
        set_age(&h, "31", 9), // repetition 9 of a 3-repetition field: 111
        set_var(&h, "$$skipped", "1"),
        ScriptStep::new(206),
        set_var(&h, "$$e", "Get ( LastError )"),
        set_var(&h, "$$open_after", "Get ( TransactionOpenState )"),
    ];
    h.add_script(0, script(1, "Fails", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$open"), "1");
    assert_eq!(global(&mut rt, "$$skipped"), None);
    assert_eq!(g(&mut rt, "$$e"), "111");
    assert_eq!(g(&mut rt, "$$open_after"), "0");
    assert_eq!(age_is(&h, 1), "");

    // With Set Revert Transaction on Error Off the steps go on.
    let mut h = host();
    let steps = vec![
        on(86, true),
        on(223, false),
        ScriptStep::new(205),
        set_age(&h, "31", 9),
        set_age(&h, "30", 1),
        ScriptStep::new(206),
    ];
    h.add_script(0, script(1, "Keeps", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(age_is(&h, 1), "30");
}

#[test]
fn revert_transaction_with_a_custom_error() {
    let mut h = host();
    let mut revert = ScriptStep::new(207);
    revert.flags |= flag::OPTION2;
    revert.set_calculation(1, Some(&calc(&h, "5001")));
    revert.set_calculation(2, Some(&calc(&h, "\"Out of stock\"")));
    let steps = vec![
        on(86, true),
        ScriptStep::new(205),
        set_age(&h, "30", 1),
        revert,
        set_var(&h, "$$skipped", "1"),
        ScriptStep::new(206),
        set_var(
            &h,
            "$$e",
            "Get ( LastError ) & \"|\" & Get ( LastErrorDetail )",
        ),
    ];
    h.add_script(0, script(1, "Custom", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(global(&mut rt, "$$skipped"), None);
    // The detail names the Revert Transaction step and its message
    // (script-runtime.md 13.10).
    assert_eq!(
        g(&mut rt, "$$e"),
        "5001|Custom\rRevert Transaction\r4\rOut of stock"
    );
    assert_eq!(age_is(&h, 1), "");
}

#[test]
fn transaction_misuse_is_error_3_and_halt_reverts() {
    let mut h = host();
    h.add_script(
        0,
        script(
            2,
            "Nested",
            vec![
                ScriptStep::new(205),
                set_var(&h, "$$inner", "Get ( LastError )"),
                ScriptStep::new(206),
                set_var(&h, "$$inner_commit", "Get ( LastError )"),
            ],
        ),
    );
    let mut alone = Script {
        id: 3,
        name: "Alone".into(),
        ..Default::default()
    };
    alone.steps = vec![
        on(86, true),
        ScriptStep::new(206),
        set_var(&h, "$$alone", "Get ( LastError )"),
    ];
    h.add_script(0, alone);
    // Revert on error Off, so the nested errors do not end the transaction.
    let steps = vec![
        on(86, true),
        on(223, false),
        ScriptStep::new(205),
        set_age(&h, "30", 1),
        perform(2),
        set_var(&h, "$$after_sub", "Get ( TransactionOpenState )"),
        ScriptStep::new(90),
        ScriptStep::new(206),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$inner"), "3");
    assert_eq!(g(&mut rt, "$$inner_commit"), "3");
    assert_eq!(g(&mut rt, "$$after_sub"), "1");
    // Halt reverted the open transaction.
    assert_eq!(age_is(&h, 1), "");
    assert!(!rt.transaction_open());
    run(&mut h, &mut rt, 3);
    assert_eq!(g(&mut rt, "$$alone"), "3");
}

#[test]
fn a_nested_open_transaction_is_skipped_and_the_outer_one_goes_on() {
    // `open-transaction.html` example 4: a sub-script's own Open and Commit
    // Transaction return 3, and its other steps run in the caller's
    // transaction, which still commits (Revert on Error is On by default).
    let mut h = host();
    // An unbalanced script, as stored scripts can be.
    h.add_script(
        0,
        Script {
            id: 2,
            name: "Nested".into(),
            steps: vec![ScriptStep::new(205)],
            ..Default::default()
        },
    );
    let steps = vec![
        on(86, true),
        ScriptStep::new(205),
        set_age(&h, "30", 1),
        perform(2),
        set_var(&h, "$$after", "Get ( LastError )"),
        ScriptStep::new(206),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Main", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$after"), "3");
    assert_eq!(g(&mut rt, "$$e"), "0");
    assert_eq!(age_is(&h, 1), "30");
}

#[test]
fn exit_loop_past_commit_transaction_reverts() {
    let mut h = host();
    let steps = vec![
        ScriptStep::new(71),
        ScriptStep::new(205),
        set_age(&h, "30", 1),
        with_calc(&h, 72, "1"),
        ScriptStep::new(206),
        ScriptStep::new(73),
        set_var(&h, "$$open", "Get ( TransactionOpenState )"),
    ];
    h.add_script(0, script(1, "Loop", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$open"), "0");
    assert_eq!(age_is(&h, 1), "");
    assert!(h.calls.contains(&"revert_transaction".to_owned()));
}

// ----- server ------------------------------------------------------------------------

#[test]
fn server_skips_unsupported_steps_with_error_3_and_never_alerts() {
    let mut h = host();
    let steps = vec![
        with_calc(&h, 87, "\"Hi\""), // Show Custom Dialog: not on the server
        set_var(&h, "$$e", "Get ( LastError )"),
        go_to_record(3),
        set_var(&h, "$$done", "1"),
    ];
    h.add_script(0, script(1, "Server", steps));
    let mut rt = Runtime::new(Options::new(Client::Server));
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$e"), "3");
    assert_eq!(g(&mut rt, "$$done"), "1");
    assert!(h.alerts.is_empty() && h.dialogs.is_empty());
    let codes: Vec<i32> = h.log.iter().map(|e| e.code.0).collect();
    assert_eq!(codes, [3, 101]);
}

#[test]
fn server_aborts_on_unknown_steps_and_disables_triggers() {
    let mut h = host();
    let steps = vec![ScriptStep::new(52), set_var(&h, "$$after", "1")];
    h.add_script(0, script(1, "Unknown", steps));
    let mut rt = Runtime::new(Options::new(Client::Server));
    run(&mut h, &mut rt, 1);
    assert_eq!(global(&mut rt, "$$after"), None);
    assert!(!rt.triggers_enabled());
}

#[test]
fn perform_script_on_server_returns_result_and_error() {
    let mut h = host();
    h.add_script(
        0,
        script(
            2,
            "On Server",
            vec![
                set_var(&h, "$$server_seen", "$$client"),
                set_age(&h, "Get ( ScriptParameter ) + 1", 1),
                with_calc(&h, 87, "\"no dialogs on the server\""),
                exit_with(&h, "\"done \" & Get ( ScriptParameter )"),
            ],
        ),
    );
    let mut psos = with_calc(&h, 164, "41");
    psos.set_script_key(StoredKey::from_serial_id(2));
    let steps = vec![
        on(86, true),
        set_var(&h, "$$client", "1"),
        psos,
        set_var(&h, "$$e", "Get ( LastError )"),
        set_var(&h, "$$r", "Get ( ScriptResult )"),
    ];
    h.add_script(0, script(1, "Client", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$r"), "done 41");
    // The server script's last step was Exit Script (a control step) after
    // Show Custom Dialog failed with 3.
    assert_eq!(g(&mut rt, "$$e"), "3");
    assert_eq!(age_is(&h, 1), "42");
}

#[test]
fn perform_script_on_server_with_callback_reads_filemaker_slots() {
    // #188: Pro stores the callback from the list as the hex literal of its
    // key in slot 3 and keeps it when the choice changes to by name
    // (`[00 6]` = 1, the name in slot 7); the parameter is slot 4.
    let mut h = host();
    h.add_script(0, script(2, "Server", vec![]));
    h.add_script(0, script(3, "Callback", vec![]));
    h.add_script(0, script(4, "Named", vec![]));
    let mut psos = with_calc(&h, 210, "\"ServerMarker\"");
    psos.set_script_key(StoredKey::from_serial_id(2));
    psos.set_calculation(3, Some(&calc(&h, "\"03\"")));
    psos.set_calculation(4, Some(&calc(&h, "\"CallbackMarker\"")));
    psos.flags |= flag::OPTION | flag::OPTION2;
    let mut named = psos.clone();
    named.set_list_value(6, Some(1));
    named.set_calculation(7, Some(&calc(&h, "\"named\"")));
    let mut missing = psos.clone();
    missing.set_calculation(3, Some(&calc(&h, "\"09\"")));
    let steps = vec![
        psos,
        set_var(&h, "$$list", "Get ( LastError )"),
        named,
        set_var(&h, "$$named", "Get ( LastError )"),
        missing,
        set_var(&h, "$$missing", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Client", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(
        (g(&mut rt, "$$list"), g(&mut rt, "$$named")),
        ("0".into(), "0".into())
    );
    // A callback the file does not have: the server is not called.
    assert_eq!(g(&mut rt, "$$missing"), "104");
    let parameter = calc(&h, "\"CallbackMarker\"");
    let calls: Vec<_> = h
        .server_callbacks
        .iter()
        .map(|c| (c.call.script, c.callback, c.callback_parameter.as_ref()))
        .collect();
    assert_eq!(
        calls,
        [
            (2, Some((0, 3)), Some(&parameter)),
            (2, Some((0, 4)), Some(&parameter))
        ]
    );
}

// ----- every step id ------------------------------------------------------------------

#[test]
fn every_step_id_dispatches_on_every_client() {
    for client in [Client::Mac, Client::Server, Client::WebDirect] {
        for id in 0..=300u16 {
            let mut h = host();
            let raw = Script {
                id: 1,
                name: format!("Step {id}"),
                steps: vec![
                    on(86, true),
                    on(85, false),
                    ScriptStep::new(id),
                    set_var(&h, "$$e", "Get ( LastError )"),
                ],
                ..Default::default()
            };
            h.add_script(0, raw);
            let mut rt = Runtime::new(Options {
                step_budget: Some(10_000),
                ..Options::new(client)
            });
            let state = run(&mut h, &mut rt, 1);
            if matches!(id, 68 | 69 | 73 | 125) {
                // A block step without its partner keeps a cached index of
                // 0, which restarts the script (script-runtime.md 3.1).
                assert_eq!(state, RunState::Stopped(Stop::Budget(10_000)), "{id}");
                continue;
            }
            assert!(
                matches!(state, RunState::Idle | RunState::Paused { .. }),
                "{client:?} step {id}: {state:?}"
            );
            let error = global(&mut rt, "$$e");
            if id == 0 {
                // -1 is never stored (script-runtime.md 13.2).
                assert_eq!(error.as_deref(), Some("0"));
            } else if client == Client::Server && matches!(id, 92 | 97 | 166) {
                // Reached on the server, no effect (13.7).
                assert_eq!(error.as_deref(), Some("0"), "step {id}");
            } else if client == Client::Server && id == 146 {
                assert_eq!(error.as_deref(), Some("5"));
            } else {
                let (known, supported) = if client == Client::Mac {
                    (
                        om_scripts::is_valid_step_id(id),
                        om_scripts::is_supported(id, client),
                    )
                } else {
                    // The server keeps FileMaker Server 26.0.3's table.
                    (
                        om_scripts::is_server_step_id(id),
                        om_scripts::is_supported_on_server(id, client),
                    )
                };
                if !known || om_scripts::OBSOLETE_STEP_IDS.contains(&id) {
                    assert_eq!(error.as_deref(), Some("4"), "{client:?} step {id}");
                } else if !supported {
                    assert_eq!(error.as_deref(), Some("3"), "{client:?} step {id}");
                }
            }
        }
    }
}

#[test]
fn contacts_focus_button_evaluates_its_parameter_then_selects_all() {
    // The Contacts template: a field button performs script 2 with a
    // literal parameter; the script goes to the object it names.
    let mut h = host();
    h.add_script(
        0,
        script(
            2,
            "Focus",
            vec![
                with_calc(&h, 145, "Get ( ScriptParameter )"),
                ScriptStep::new(50),
            ],
        ),
    );
    let mut button = ScriptStep::new(1);
    button.reserved[..4].copy_from_slice(&[2, 1, 2, 0xbe]);
    let mut rt = Runtime::default();
    rt.perform_step(&mut h, button, Some(Value::text("First Name")))
        .unwrap();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(h.calls, ["go_to_object First Name", "select_all"]);
}

#[test]
fn delayed_triggers_queue_behind_the_running_script() {
    let mut h = host();
    h.pause_waits = true;
    h.add_script(
        0,
        script(
            9,
            "Delayed",
            vec![set_var(&h, "$$order", "$$order & \"t\"")],
        ),
    );
    let steps = vec![
        ScriptStep::new(62),
        set_var(&h, "$$order", "$$order & \"s\""),
    ];
    h.add_script(0, script(1, "Running", steps));
    let mut rt = Runtime::default();
    assert!(matches!(run(&mut h, &mut rt, 1), RunState::Paused { .. }));
    let load = Trigger::new(TriggerEvent::RecordLoad, 0, 9);
    assert_eq!(rt.fire_trigger(&mut h, load), Fired::Queued);
    rt.resume();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$order"), "st");
}

#[test]
fn malformed_calculations_fail_without_stopping_the_script() {
    let mut h = host();
    let mut bad = ScriptStep::new(141);
    bad.set_text(0, Some("$$v"));
    // A text literal followed by a stray byte (old prototype vector).
    bad.set_calculation(
        1,
        Some(&Calculation::new(None, vec![0x13, 1, b'X' ^ 0x5a, 0, 0])),
    );
    let mut condition = ScriptStep::new(68);
    condition.set_calculation(0, Some(&Calculation::new(None, vec![0xff, 0xff])));
    let mut end = ScriptStep::new(70);
    end.value = 0;
    let mut script = Script {
        id: 1,
        name: "Bad".into(),
        steps: vec![
            bad,
            condition,
            set_var(&h, "$$inside", "1"),
            end,
            set_var(&h, "$$after", "1"),
        ],
        ..Default::default()
    };
    script.update_partner_indexes().unwrap();
    h.add_script(0, script);
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Idle);
    assert_eq!(g(&mut rt, "$$v"), "?");
    // A condition that cannot be evaluated is `?`, which is true.
    assert_eq!(g(&mut rt, "$$inside"), "1");
    assert_eq!(g(&mut rt, "$$after"), "1");
}

#[test]
fn button_frames_use_their_callers_variables_and_clear_drops_everything() {
    let mut h = host();
    h.pause_waits = true;
    let steps = vec![
        set_var(&h, "$local", "\"mine\""),
        ScriptStep::new(62),
        set_var(&h, "$$seen", "$local"),
    ];
    h.add_script(0, script(1, "Paused", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    // A button's single Set Variable runs in the paused script's scope.
    let button = set_var(&h, "$local", "\"button\"");
    rt.perform_step(&mut h, button, None).unwrap();
    assert!(matches!(rt.run(&mut h), RunState::Paused { .. }));
    rt.resume();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$seen"), "button");
    run(&mut h, &mut rt, 1);
    assert!(rt.is_running());
    rt.clear();
    assert!(!rt.is_running() && !rt.is_paused());
}

#[test]
fn install_on_timer_reaches_the_host() {
    let mut h = host();
    let mut timer = with_calc(&h, 148, "\"tick\"");
    timer.set_script_key(StoredKey::from_serial_id(9));
    timer.set_calculation(1, Some(&calc(&h, "5")));
    h.add_script(0, script(1, "Timer", vec![timer]));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    let timer = h.timer.as_ref().expect("timer");
    assert_eq!((timer.script, timer.interval), (9, 5.0));
}

#[test]
fn pending_transaction_steps_keep_the_transaction() {
    // script-runtime.md 1.7: a pending Open Transaction that completes
    // starts the runtime's transaction; a pending Commit keeps it open
    // until the commit is entered again.
    let mut h = host();
    h.pending_opens = 1;
    let steps = vec![
        ScriptStep::new(205),
        set_age(&h, "30", 1),
        ScriptStep::new(206),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Async", steps));
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Waiting);
    rt.complete_step(ErrorCode::NONE);
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$e"), "0");
    assert_eq!(
        h.calls
            .iter()
            .filter(|c| *c == "commit_transaction")
            .count(),
        1
    );
    assert_eq!(age_is(&h, 1), "30");

    let mut h = host();
    h.pending_commits = 1;
    let steps = vec![
        ScriptStep::new(205),
        set_age(&h, "31", 1),
        ScriptStep::new(206),
        set_var(&h, "$$e", "Get ( LastError )"),
    ];
    h.add_script(0, script(1, "Async", steps));
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Waiting);
    assert!(rt.transaction_open());
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$e"), "0");
    assert_eq!(
        h.calls
            .iter()
            .filter(|c| *c == "commit_transaction")
            .count(),
        2
    );
    assert!(!rt.transaction_open());
    assert_eq!(age_is(&h, 1), "31");
}

#[test]
fn a_false_result_cancels_only_pre_event_triggers() {
    // OnRecordLoad runs after its event and has no result
    // (`onrecordload.html`): Exit Script [False] neither cancels nor
    // stores error 20.
    let mut h = host();
    h.add_script(0, script(9, "Load", vec![exit_with(&h, "0")]));
    let mut rt = Runtime::default();
    let mut load = Trigger::new(TriggerEvent::RecordLoad, 0, 9);
    load.action = Some(5);
    rt.fire_trigger(&mut h, load);
    while rt.run(&mut h) == RunState::Yielded {}
    assert_eq!(rt.last_error(), ErrorCode::NONE);
    assert!(h.trigger_actions.last().unwrap().proceed);
}

#[test]
fn huge_variable_repetitions_stay_distinct() {
    // FileMaker Server: `$r[10^20]` and `$r[10^20 + 1]` are two values.
    let mut h = host();
    let steps = vec![set_var(
        &h,
        "$$x",
        "Let ( [ $r[100000000000000000000] = 1 ; $r[100000000000000000001] = 2 ] ; \
         $r[100000000000000000000] & \"|\" & $r[100000000000000000001] )",
    )];
    h.add_script(0, script(1, "Big", steps));
    let mut rt = Runtime::default();
    run(&mut h, &mut rt, 1);
    assert_eq!(g(&mut rt, "$$x"), "1|2");
}

// ----- the Script Debugger (script-runtime.md 6.4) ----------------------------

fn breakpoint(mut step: ScriptStep) -> ScriptStep {
    step.set_flag(flag::BREAKPOINT, true);
    step
}

/// The Call Stack: each script's name and step, the innermost last.
fn call_stack(rt: &Runtime) -> Vec<(String, Option<usize>)> {
    rt.debug_stack()
        .into_iter()
        .map(|frame| (frame.name, frame.step))
        .collect()
}

fn at(frames: &[(&str, Option<usize>)]) -> Vec<(String, Option<usize>)> {
    frames.iter().map(|&(n, s)| (n.to_owned(), s)).collect()
}

/// Continue the stopped scripts with `command` until the debugger stops
/// them again or they end.
fn debug(h: &mut MemoryScriptHost, rt: &mut Runtime, command: DebugCommand) -> RunState {
    assert!(rt.debug(command), "not stopped");
    rt.run(h)
}

#[test]
fn the_debugger_steps_over_into_and_out_and_stops_at_breakpoints() {
    let mut h = host();
    let sub = vec![set_var(&h, "$$s", "1"), breakpoint(set_var(&h, "$$t", "2"))];
    h.add_script(0, script(2, "Sub", sub));
    let main = vec![
        set_var(&h, "$$a", "1"),
        perform(2),
        breakpoint(set_var(&h, "$$b", "2")),
        set_var(&h, "$$c", "3"),
    ];
    h.add_script(0, script(1, "Main", main));
    let mut rt = Runtime::default();
    rt.start_debugging();
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    // A script started with the debugger open stops before its first step,
    // and stays stopped until it is continued.
    let started = RunState::Debugger(DebugStop::Started);
    assert_eq!(rt.run(&mut h), started);
    assert_eq!(rt.run(&mut h), started);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(0))]));
    assert_eq!(global(&mut rt, "$$a"), None);

    let stepped = RunState::Debugger(DebugStop::Stepped);
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::StepOver), stepped);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(1))]));
    assert_eq!(g(&mut rt, "$$a"), "1");
    // Step Into the Perform Script: the sub-script's first step.
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::StepInto), stepped);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(1)), ("Sub", Some(0))]));
    // Step Out stops at a breakpoint on its way.
    let breakpoint = RunState::Debugger(DebugStop::Breakpoint);
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::StepOut), breakpoint);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(1)), ("Sub", Some(1))]));
    // From the breakpoint it runs on, and stops in the caller after the
    // Perform Script.
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::StepOut), stepped);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(2))]));
    assert_eq!(g(&mut rt, "$$t"), "2");
    // Run continues past the breakpoint it is stopped at, to the end.
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
    assert_eq!(g(&mut rt, "$$c"), "3");
    assert_eq!(rt.debug_stop(), None);

    // Run from the start stops at the breakpoint inside the sub-script;
    // Step Over from there stops in the caller once the sub-script ends.
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), started);
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), breakpoint);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(1)), ("Sub", Some(1))]));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::StepOver), stepped);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(2))]));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
}

#[test]
fn the_debugger_sets_the_next_step_pauses_on_errors_halts_and_closes() {
    let mut h = host();
    let main = vec![
        set_var(&h, "$$a", "1"),
        go_to_layout(99),
        set_var(&h, "$$b", "2"),
        breakpoint(set_var(&h, "$$c", "3")),
    ];
    h.add_script(0, script(1, "Main", main));
    let mut rt = Runtime::default();
    rt.start_debugging();
    rt.set_pause_on_error(true);
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Started));
    // Set Next Step skips the first step and stays stopped.
    assert!(rt.set_next_step(1));
    assert!(!rt.set_next_step(4), "past the last step");
    assert_eq!(call_stack(&rt), at(&[("Main", Some(1))]));
    // The missing layout's error stops the script before the next step.
    let state = debug(&mut h, &mut rt, DebugCommand::Run);
    let error = rt.last_error();
    assert!(error.is_error());
    assert_eq!(state, RunState::Debugger(DebugStop::Error(error)));
    assert_eq!(call_stack(&rt), at(&[("Main", Some(2))]));
    assert_eq!(global(&mut rt, "$$a"), None);
    assert!(rt.set_next_step(3));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
    assert_eq!(global(&mut rt, "$$b"), None);
    assert_eq!(g(&mut rt, "$$c"), "3");
    assert!(!rt.set_next_step(0), "no script is stopped");

    // Halt ends the stopped script.
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Started));
    rt.halt(&mut h);
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(rt.debug_stop(), None);
    assert!(rt.is_debugging());

    // Closing the debugger lets a stopped script run to its end, past its
    // error and its breakpoint, with script triggers enabled again.
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Started));
    rt.set_user_triggers(false);
    rt.stop_debugging();
    assert_eq!(rt.run(&mut h), RunState::Idle);
    assert_eq!(g(&mut rt, "$$b"), "2");
    assert!(rt.triggers_enabled());
}

#[test]
fn the_debugger_runs_an_executable_only_script_whole() {
    // A Modifiable script calling an Executable Only one: the latter runs
    // whole with its steps hidden; a Modifiable script it calls is shown
    // (script-runtime.md 6.4).
    let mut h = host();
    h.add_script(
        0,
        script(3, "Inner", vec![breakpoint(set_var(&h, "$$i", "1"))]),
    );
    h.add_script(0, script(2, "Hidden", vec![breakpoint(perform(3))]));
    h.executable_only.insert((0, 2));
    let main = vec![perform(2), set_var(&h, "$$a", "1")];
    h.add_script(0, script(1, "Main", main));
    let mut rt = Runtime::default();
    rt.start_debugging();
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Started));
    // Run passes the hidden script's breakpoint and stops at the inner one.
    let state = debug(&mut h, &mut rt, DebugCommand::Run);
    assert_eq!(state, RunState::Debugger(DebugStop::Breakpoint));
    assert_eq!(
        call_stack(&rt),
        at(&[("Main", Some(0)), ("Hidden", None), ("Inner", Some(0))])
    );
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
    assert_eq!(g(&mut rt, "$$i"), "1");

    // Step Into from the caller goes through the hidden script to the
    // first step it shows.
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Started));
    let state = debug(&mut h, &mut rt, DebugCommand::StepInto);
    assert_eq!(state, RunState::Debugger(DebugStop::Stepped));
    assert_eq!(
        call_stack(&rt),
        at(&[("Main", Some(0)), ("Hidden", None), ("Inner", Some(0))])
    );
    assert_eq!(
        debug(&mut h, &mut rt, DebugCommand::StepOut),
        RunState::Debugger(DebugStop::Stepped)
    );
    assert_eq!(call_stack(&rt), at(&[("Main", Some(1))]));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
    assert_eq!(g(&mut rt, "$$a"), "1");
}

#[test]
fn the_debugger_keeps_stops_and_errors_to_the_scripts_it_shows() {
    let mut h = host();
    h.pause_waits = true;
    let transaction = vec![
        ScriptStep::new(205),
        set_var(&h, "$$t", "1"),
        ScriptStep::new(206),
        set_var(&h, "$$after", "1"),
    ];
    h.add_script(0, script(1, "Transaction", transaction));
    h.add_script(0, script(2, "Hidden", vec![go_to_layout(99)]));
    h.executable_only.insert((0, 2));
    let caller = vec![perform(2), set_var(&h, "$$caller", "1")];
    h.add_script(0, script(3, "Caller", caller));
    h.add_script(
        0,
        script(4, "Last", vec![set_var(&h, "$$a", "1"), go_to_layout(99)]),
    );
    let mut pause = ScriptStep::new(62);
    pause.flags &= !flag::CALCULATION;
    let paused = vec![pause, set_var(&h, "$$b", "2"), set_var(&h, "$$c", "3")];
    h.add_script(0, script(5, "Paused", paused));
    let mut rt = Runtime::default();
    rt.start_debugging();
    rt.set_pause_on_error(true);
    let started = RunState::Debugger(DebugStop::Started);

    // Set Next Step is refused while a transaction is open, and a failed
    // Commit Transaction, which stores its own error, stops the script.
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), started);
    assert_eq!(
        debug(&mut h, &mut rt, DebugCommand::StepOver),
        RunState::Debugger(DebugStop::Stepped)
    );
    assert!(rt.transaction_open());
    assert!(!rt.set_next_step(3));
    h.pending_commits = 1;
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Waiting);
    rt.complete_step(ErrorCode(301));
    assert_eq!(
        rt.run(&mut h),
        RunState::Debugger(DebugStop::Error(ErrorCode(301)))
    );
    assert_eq!(call_stack(&rt), at(&[("Transaction", Some(3))]));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);

    // A hidden script's error does not stop its caller.
    rt.perform_script(&mut h, ScriptCall::new(0, 3))
        .expect("starts");
    assert_eq!(rt.run(&mut h), started);
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
    assert_eq!(g(&mut rt, "$$caller"), "1");

    // A failure of the last step stops at the script's end.
    rt.perform_script(&mut h, ScriptCall::new(0, 4))
        .expect("starts");
    assert_eq!(rt.run(&mut h), started);
    let state = debug(&mut h, &mut rt, DebugCommand::Run);
    assert_eq!(state, RunState::Debugger(DebugStop::Error(ErrorCode(105))));
    assert_eq!(call_stack(&rt), at(&[("Last", Some(2))]));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);

    // A script queued to run after the current one neither keeps that one
    // from stopping first nor shows in the Call Stack.
    rt.perform_script(&mut h, ScriptCall::new(0, 3))
        .expect("starts");
    let mut queued = ScriptCall::new(0, 4);
    queued.queue_at_bottom = true;
    rt.perform_script(&mut h, queued).expect("queues");
    assert_eq!(rt.run(&mut h), started);
    assert_eq!(call_stack(&rt), at(&[("Caller", Some(0))]));
    // The stop belongs to its script: Exit ends it, and the hidden script
    // started in its place runs whole.
    let mut exit = ScriptCall::new(0, 2);
    exit.mode = QueueMode::Exit;
    rt.perform_script(&mut h, exit).expect("starts");
    assert_eq!(rt.debug_stop(), None);
    assert!(!rt.set_next_step(0));
    // The queued script then starts and stops before its first step.
    assert_eq!(rt.run(&mut h), started);
    assert_eq!(call_stack(&rt), at(&[("Last", Some(0))]));
    rt.halt(&mut h);
    assert_eq!(rt.run(&mut h), RunState::Idle);

    // A Pause/Resume Script pause is continued by the debugger's commands.
    rt.set_pause_on_error(false);
    rt.perform_script(&mut h, ScriptCall::new(0, 5))
        .expect("starts");
    assert_eq!(rt.run(&mut h), started);
    assert!(matches!(
        debug(&mut h, &mut rt, DebugCommand::Run),
        RunState::Paused { .. }
    ));
    assert_eq!(
        debug(&mut h, &mut rt, DebugCommand::StepOver),
        RunState::Debugger(DebugStop::Stepped)
    );
    assert_eq!(call_stack(&rt), at(&[("Paused", Some(2))]));
    assert_eq!(g(&mut rt, "$$b"), "2");
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
}

#[test]
fn the_debugger_keeps_each_stop_and_error_with_its_frame() {
    // Main's last step fails; Keys has a breakpoint; Plain has none.
    let scripts = |h: &mut MemoryScriptHost| {
        let main = vec![set_var(h, "$$a", "1"), go_to_layout(99)];
        h.add_script(0, script(1, "Main", main));
        h.add_script(
            0,
            script(6, "Keys", vec![breakpoint(set_var(h, "$$k", "1"))]),
        );
        h.add_script(0, script(7, "Plain", vec![set_var(h, "$$p", "1")]));
    };
    let debugging = |h: &mut MemoryScriptHost| {
        let mut rt = Runtime::default();
        rt.start_debugging();
        rt.set_pause_on_error(true);
        rt.perform_script(h, ScriptCall::new(0, 1)).expect("starts");
        assert_eq!(rt.run(h), RunState::Debugger(DebugStop::Started));
        rt
    };
    let keystroke = |script| Trigger::new(TriggerEvent::ObjectKeystroke, 0, script);
    let failed = RunState::Debugger(DebugStop::Error(ErrorCode(105)));

    // A trigger's own stop and Run leave Main's error stop below it.
    let mut h = host();
    scripts(&mut h);
    let mut rt = debugging(&mut h);
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), failed);
    rt.fire_trigger(&mut h, keystroke(6));
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Breakpoint));
    assert_eq!(call_stack(&rt), at(&[("Main", Some(1)), ("Keys", Some(0))]));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), failed);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(2))]));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);

    // A trigger on top of a script waiting for its first step is no new
    // script: it runs through, and the script is stopped again.
    let mut h = host();
    scripts(&mut h);
    let mut rt = debugging(&mut h);
    rt.fire_trigger(&mut h, keystroke(7));
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Started));
    assert_eq!(call_stack(&rt), at(&[("Main", Some(0))]));
    assert_eq!(g(&mut rt, "$$p"), "1");

    // Main's error waits for Main: a trigger run before the check does not
    // take it.
    let mut h = host();
    scripts(&mut h);
    let mut rt = debugging(&mut h);
    assert!(rt.debug(DebugCommand::Run));
    assert_eq!(rt.step(&mut h), RunState::Yielded);
    assert_eq!(rt.step(&mut h), RunState::Yielded);
    rt.fire_trigger(&mut h, keystroke(7));
    assert_eq!(rt.run(&mut h), failed);
    assert_eq!(call_stack(&rt), at(&[("Main", Some(2))]));
    assert_eq!(g(&mut rt, "$$p"), "1");

    // A step that was waiting when the debugger opened is still Main's.
    let mut h = host();
    let steps = vec![
        ScriptStep::new(205),
        ScriptStep::new(206),
        set_var(&h, "$$after", "1"),
    ];
    h.add_script(0, script(1, "Commit", steps));
    h.pending_commits = 1;
    let mut rt = Runtime::default();
    assert_eq!(run(&mut h, &mut rt, 1), RunState::Waiting);
    rt.start_debugging();
    rt.set_pause_on_error(true);
    rt.complete_step(ErrorCode(301));
    assert_eq!(
        rt.run(&mut h),
        RunState::Debugger(DebugStop::Error(ErrorCode(301)))
    );
    assert_eq!(call_stack(&rt), at(&[("Commit", Some(2))]));

    // A hidden script's step that a shown trigger let run, and that then
    // failed, is still the hidden script's: its caller does not stop.
    let mut h = host();
    h.layout_triggers
        .insert((1, TriggerEvent::LayoutExit), (0, 9));
    h.add_script(0, script(9, "OnExit", vec![set_var(&h, "$$x", "1")]));
    h.add_script(0, script(2, "Hidden", vec![go_to_layout(99)]));
    h.executable_only.insert((0, 2));
    let caller = vec![perform(2), set_var(&h, "$$m", "1")];
    h.add_script(0, script(1, "Main", caller));
    let mut rt = Runtime::default();
    rt.start_debugging();
    rt.set_pause_on_error(true);
    rt.perform_script(&mut h, ScriptCall::new(0, 1))
        .expect("starts");
    assert_eq!(rt.run(&mut h), RunState::Debugger(DebugStop::Started));
    assert_eq!(debug(&mut h, &mut rt, DebugCommand::Run), RunState::Idle);
    assert_eq!(g(&mut rt, "$$x"), "1");
    assert_eq!(g(&mut rt, "$$m"), "1");
    assert_eq!(rt.last_error(), ErrorCode::NONE);
}
