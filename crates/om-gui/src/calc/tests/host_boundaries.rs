use super::*;

fn boundary_fixture(inner: &str, hide: &str) -> (CalcSchema, Calculations) {
    let (mut schema, mut calcs) = pending_container_fixture();
    calcs.cache.get_mut().tables.insert(TABLE, Arc::from([1]));
    pending_add_calc(&mut schema, 55, "Computed", inner);
    let object = LayoutObject {
        id: 1,
        name: Some("Obj3".into()),
        calcs: om_layout::ObjectCalcs {
            hide: Some(formula(&schema, hide)),
            ..Default::default()
        },
        ..Default::default()
    };
    schema.add_layout_entry(om_data::host::LayoutEntry {
        layout: Some(Arc::new(om_layout::Layout {
            objects: om_layout::ObjectStore {
                objects: vec![object],
                root: vec![1],
                ..Default::default()
            },
            ..Default::default()
        })),
        ..om_data::host::LayoutEntry::new(1, "Probe", Some(OCCURRENCE))
    });
    (schema, calcs)
}

fn boundary_eval<'a>(calcs: &'a Calculations, schema: &'a CalcSchema) -> Evaluator<'a> {
    let mut out = evaluator(calcs, schema);
    out.info.layout_name = "Probe".into();
    out
}

fn boundary_run(call: &str, inner: &str, hide: &str, warm: bool) {
    boundary_run_variable(call, inner, hide, warm, "$n");
}

fn boundary_run_variable(call: &str, inner: &str, hide: &str, warm: bool, variable: &str) {
    let (mut schema, mut calcs) = boundary_fixture(inner, hide);
    pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ {variable} = 0 ; ignored = {call} ] ; If ( {variable} = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
    if warm {
        let needs = [CalcNeed::MemberData {
            table: TABLE,
            member: vec![3],
        }];
        calcs.accept_data(&needs, Ok(container_body_reply(3)));
    }
    let steps: &[u8] = if warm { &[2] } else { &[3, 2] };
    for member in steps {
        assert!(
            boundary_eval(&calcs, &schema)
                .named_field_value("Outer", 1)
                .is_none(),
            "{call}"
        );
        let needs = calcs.take_wanted();
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![*member]
            }],
            "{call}: unreachable successor requested"
        );
        calcs.accept_data(&needs, Ok(container_body_reply(*member)));
    }
    let current = boundary_eval(&calcs, &schema);
    let result = current
        .named_field_value("Outer", 1)
        .expect("known host call must converge");
    assert_eq!(current.display(&result), "3", "{call}");
    assert!(calcs.take_wanted().is_empty());
}

const BOUNDARY_ASSIGN: &str = "Let ( $n = 3 ; 0 )";
const BOUNDARY_NESTED: &str =
    "If ( IsValid ( Length ( Jobs::One ) ) ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )";
const BOUNDARY_QUERY: &str = "ExecuteSQL ( \"SELECT Computed FROM Jobs WHERE \" & Length ( Jobs::One ) & \" = 3\" ; \"\" ; \"\" )";
const BOUNDARY_PARAM: &str =
    "ExecuteSQL ( \"SELECT Computed FROM Jobs WHERE 3 = ?\" ; \"\" ; \"\" ; Length ( Jobs::One ) )";
const BOUNDARY_SQL: &str = "ExecuteSQL ( \"SELECT Computed FROM Jobs\" ; \"\" ; \"\" )";
const BOUNDARY_TARGET: &str =
    "GetLayoutObjectAttribute ( \"Obj\" & Length ( Jobs::One ) ; \"isObjectHidden\" )";
const BOUNDARY_HIDE: &str = "GetLayoutObjectAttribute ( \"Obj3\" ; \"isObjectHidden\" )";

#[test]
fn boundary_pending_sql_query_does_not_skip_assignment() {
    boundary_run(BOUNDARY_QUERY, BOUNDARY_ASSIGN, "0", false);
}
#[test]
fn boundary_pending_sql_parameter_does_not_skip_assignment() {
    boundary_run(BOUNDARY_PARAM, BOUNDARY_ASSIGN, "0", false);
}
#[test]
fn boundary_pending_sql_nested_execution_does_not_resume_successor() {
    boundary_run(BOUNDARY_SQL, BOUNDARY_NESTED, "0", false);
}
#[test]
fn boundary_pending_layout_target_does_not_skip_assignment() {
    boundary_run(BOUNDARY_TARGET, "0", BOUNDARY_ASSIGN, false);
}
#[test]
fn boundary_pending_layout_nested_execution_does_not_resume_successor() {
    boundary_run(BOUNDARY_HIDE, "0", BOUNDARY_NESTED, false);
}
#[test]
fn boundary_warm_calls_execute_assignments() {
    for (call, inner, hide) in [
        (BOUNDARY_QUERY, BOUNDARY_ASSIGN, "0"),
        (BOUNDARY_PARAM, BOUNDARY_ASSIGN, "0"),
        (BOUNDARY_SQL, BOUNDARY_NESTED, "0"),
        (BOUNDARY_TARGET, "0", BOUNDARY_ASSIGN),
        (BOUNDARY_HIDE, "0", BOUNDARY_NESTED),
    ] {
        boundary_run(call, inner, hide, true);
    }
}
#[test]
fn boundary_pending_variable_provenance_crosses_sql() {
    boundary_run(
        "Let ( $x = Length ( Jobs::One ) ; ExecuteSQL ( \"SELECT Computed FROM Jobs\" ; \"\" ; \"\" ) )",
        "If ( $x = 3 ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )", "0", false);
}
#[test]
fn boundary_pending_variable_provenance_crosses_layout() {
    boundary_run(
        "Let ( $x = Length ( Jobs::One ) ; GetLayoutObjectAttribute ( \"Obj3\" ; \"isObjectHidden\" ) )", "0",
        "If ( $x = 3 ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )", false);
}
#[test]
fn boundary_warm_variable_provenance_controls() {
    boundary_run(
        "Let ( $x = Length ( Jobs::One ) ; ExecuteSQL ( \"SELECT Computed FROM Jobs\" ; \"\" ; \"\" ) )",
        "If ( $x = 3 ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )", "0", true);
    boundary_run(
        "Let ( $x = Length ( Jobs::One ) ; GetLayoutObjectAttribute ( \"Obj3\" ; \"isObjectHidden\" ) )", "0",
        "If ( $x = 3 ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )", true);
}
#[test]
fn boundary_independent_sql_rows_still_batch_bodies() {
    let (mut schema, mut calcs) = boundary_fixture("Length ( Jobs::One )", "0");
    let mut second = RecordValues::default();
    let info = om_data::containers::MemberInfo {
        name: "2.dat".into(),
        md5: [2; 16],
        path_type: String::new(),
        owner: None,
    };
    second.set(51, 1, Value::Container(info.value(TABLE, &[2])));
    calcs
        .cache
        .get_mut()
        .records
        .insert((TABLE, 2), Some(Arc::new(second)));
    calcs
        .cache
        .get_mut()
        .tables
        .insert(TABLE, Arc::from([1, 2]));
    pending_add_calc(&mut schema, 56, "Outer", BOUNDARY_SQL);
    assert!(boundary_eval(&calcs, &schema)
        .named_field_value("Outer", 1)
        .is_none());
    assert_eq!(
        calcs.take_wanted(),
        [
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![2]
            },
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            },
        ]
    );
}
#[test]
fn boundary_permanent_sql_and_layout_failures_remain_catchable() {
    for (call, inner, hide) in [
        (
            "ExecuteSQL ( \"SELECT Absent FROM Jobs\" ; \"\" ; \"\" )",
            "0",
            "0",
        ),
        (
            BOUNDARY_SQL,
            "SetRecursion ( While ( [i=0]; i<2; [i=i+1]; i ); 1 )",
            "0",
        ),
        (
            BOUNDARY_HIDE,
            "0",
            "SetRecursion ( While ( [i=0]; i<2; [i=i+1]; i ); 1 )",
        ),
        (
            "GetLayoutObjectAttribute ( \"Absent\" ; \"isObjectHidden\" )",
            "0",
            BOUNDARY_ASSIGN,
        ),
    ] {
        boundary_run(
            &format!("Let ( [ $n = 3 ; ignored2 = {call} ] ; 0 )"),
            inner,
            hide,
            true,
        );
    }
}
#[test]
fn boundary_sql_pure_calculation_and_successor_still_batch() {
    let (mut schema, mut calcs) = boundary_fixture("Length ( Jobs::One )", "0");
    pending_add_calc(&mut schema, 56, "Outer", "Let ( ignored = ExecuteSQL ( \"SELECT Computed FROM Jobs\" ; \"\" ; \"\" ) ; Length ( Jobs::Two ) )");
    assert!(boundary_eval(&calcs, &schema)
        .named_field_value("Outer", 1)
        .is_none());
    assert_eq!(
        calcs.take_wanted(),
        [
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![2]
            },
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            },
        ]
    );
}
#[test]
fn boundary_sql_output_separator_does_not_gate_body_execution() {
    let (mut schema, mut calcs) = boundary_fixture(BOUNDARY_ASSIGN, "0");
    pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $n = 0 ; ignored = ExecuteSQL ( \"SELECT Computed FROM Jobs\" ; Length ( Jobs::One ) ; \"\" ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
    assert!(boundary_eval(&calcs, &schema)
        .named_field_value("Outer", 1)
        .is_none());
    assert_eq!(
        calcs.take_wanted(),
        [
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![2]
            },
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            },
        ]
    );
}
#[test]
fn boundary_unrelated_pending_variable_does_not_gate_host_execution() {
    let (mut schema, mut calcs) = boundary_fixture(BOUNDARY_ASSIGN, "0");
    pending_add_calc(&mut schema, 56, "Outer", "Let ( [ $unused = Length ( Jobs::One ) ; $n = 0 ; ignored = ExecuteSQL ( \"SELECT Computed FROM Jobs\" ; \"\" ; \"\" ) ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
    assert!(boundary_eval(&calcs, &schema)
        .named_field_value("Outer", 1)
        .is_none());
    assert_eq!(
        calcs.take_wanted(),
        [
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![2]
            },
            CalcNeed::MemberData {
                table: TABLE,
                member: vec![3]
            },
        ]
    );
}
#[test]
fn boundary_known_empty_sql_set_keeps_successor_catchable() {
    let (mut schema, mut calcs) = boundary_fixture(BOUNDARY_ASSIGN, "0");
    calcs.cache.get_mut().tables.insert(TABLE, Arc::from([]));
    pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ $n = 3 ; ignored = {BOUNDARY_SQL} ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
    pending_expect_steps(&schema, &mut calcs, "Outer", false, false);
}
#[test]
fn boundary_pending_sql_variable_write_remains_pending_in_caller() {
    boundary_run(
        BOUNDARY_SQL,
        "Let ( $n = Length ( Jobs::One ) ; 0 )",
        "0",
        false,
    );
}
#[test]
fn boundary_pending_layout_variable_write_remains_pending_in_caller() {
    boundary_run(
        BOUNDARY_HIDE,
        "0",
        "Let ( $n = Length ( Jobs::One ) ; 0 )",
        false,
    );
}
#[test]
fn boundary_warm_nested_variable_write_controls() {
    boundary_run(
        BOUNDARY_SQL,
        "Let ( $n = Length ( Jobs::One ) ; 0 )",
        "0",
        true,
    );
    boundary_run(
        BOUNDARY_HIDE,
        "0",
        "Let ( $n = Length ( Jobs::One ) ; 0 )",
        true,
    );
}
#[test]
fn boundary_sql_calculation_reads_an_available_container_body() {
    let (mut schema, mut calcs) = boundary_fixture("Length ( Jobs::One )", "0");
    let needs = [CalcNeed::MemberData {
        table: TABLE,
        member: vec![3],
    }];
    calcs.accept_data(&needs, Ok(container_body_reply(3)));
    pending_add_calc(
        &mut schema,
        56,
        "Outer",
        "GetAsNumber ( ExecuteSQL ( \"SELECT Computed FROM Jobs\" ; \"\" ; \"\" ) )",
    );
    let current = boundary_eval(&calcs, &schema);
    let result = current
        .named_field_value("Outer", 1)
        .expect("body is already present");
    assert_eq!(current.display(&result), "3");
    assert!(calcs.take_wanted().is_empty());
}
#[test]
fn boundary_direct_sql_container_column_stays_metadata_only() {
    let (mut schema, mut calcs) = boundary_fixture("0", "0");
    schema.add_field(
        TABLE,
        FieldDef::calculation(
            56,
            "Outer",
            ValueType::Text,
            formula(
                &schema,
                "ExecuteSQL ( \"SELECT One FROM Jobs\" ; \"\" ; \"\" )",
            ),
            false,
        ),
    );
    let current = boundary_eval(&calcs, &schema);
    let result = current
        .named_field_value("Outer", 1)
        .expect("direct SQL reads metadata only");
    assert_eq!(current.display(&result), "3.dat");
    assert!(calcs.take_wanted().is_empty());
}
#[test]
fn boundary_exact_warm_layout_controls() {
    boundary_run(BOUNDARY_TARGET, "0", BOUNDARY_ASSIGN, true);
    boundary_run(BOUNDARY_HIDE, "0", BOUNDARY_NESTED, true);
    boundary_run(
        BOUNDARY_HIDE,
        "0",
        "Let ( $n = Length ( Jobs::One ) ; 0 )",
        true,
    );
}
#[test]
fn boundary_sql_calculation_reads_available_member_metadata() {
    let (mut schema, mut calcs) = boundary_fixture("0", "0");
    pending_raw_one(&mut calcs);
    let need = [CalcNeed::Member {
        table: TABLE,
        member: vec![3],
    }];
    calcs.accept_data(&need, Ok(pending_header_reply(3)));
    schema.add_field(
        TABLE,
        FieldDef::calculation(
            55,
            "Computed",
            ValueType::Text,
            formula(&schema, "GetAsText ( Jobs::One )"),
            false,
        ),
    );
    schema.add_field(
        TABLE,
        FieldDef::calculation(
            56,
            "Outer",
            ValueType::Text,
            formula(&schema, BOUNDARY_SQL),
            false,
        ),
    );
    let current = boundary_eval(&calcs, &schema);
    let result = current
        .named_field_value("Outer", 1)
        .expect("header is available");
    assert_eq!(current.display(&result), "3.dat");
    assert!(calcs.take_wanted().is_empty());
}

#[test]
fn boundary_variables_keep_scope_case_and_repetition_across_nested_hosts() {
    for variable in ["$PrObE[2]", "$$PrObE[-2]", "$PrObE[100000000000000000000]"] {
        let read = variable.to_lowercase();
        let input = format!("If ( {read} = 3 ; Let ( $n = 3 ; 0 ) ; Let ( $n = 1 ; 0 ) )");
        let output = format!("Let ( {variable} = Length ( Jobs::One ) ; 0 )");
        for warm in [false, true] {
            boundary_run(
                &format!("Let ( {variable} = Length ( Jobs::One ) ; {BOUNDARY_SQL} )"),
                &input,
                "0",
                warm,
            );
            boundary_run(
                &format!("Let ( {variable} = Length ( Jobs::One ) ; {BOUNDARY_HIDE} )"),
                "0",
                &input,
                warm,
            );
            boundary_run_variable(BOUNDARY_SQL, &output, "0", warm, &read);
            boundary_run_variable(BOUNDARY_HIDE, "0", &output, warm, &read);
        }
    }
}

#[test]
fn boundary_known_overwrite_of_a_nested_pending_variable_keeps_batching() {
    for (inner, hide, call) in [
        ("Let ( $n = Length ( Jobs::One ) ; 0 )", "0", BOUNDARY_SQL),
        ("0", "Let ( $n = Length ( Jobs::One ) ; 0 )", BOUNDARY_HIDE),
    ] {
        let (mut schema, mut calcs) = boundary_fixture(inner, hide);
        pending_add_calc(&mut schema, 56, "Outer", &format!("Let ( [ ignored = {call} ; $n = 3 ] ; If ( $n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )"));
        assert!(boundary_eval(&calcs, &schema)
            .named_field_value("Outer", 1)
            .is_none());
        assert_eq!(
            calcs.take_wanted(),
            [
                CalcNeed::MemberData {
                    table: TABLE,
                    member: vec![2]
                },
                CalcNeed::MemberData {
                    table: TABLE,
                    member: vec![3]
                },
            ]
        );
    }
}

#[test]
fn boundary_shared_variables_suspend_only_the_current_evaluation() {
    use om_data::calc::eval::{evaluate_text, Evaluation};
    use om_data::host::Vars;

    let (schema, mut calcs) = boundary_fixture("0", "Let ( $$n = Length ( Jobs::One ) ; 0 )");
    let mut variables = Variables::new();
    let run = |calcs: &Calculations,
               variables: &mut Variables,
               text: &str,
               context: &Context|
     -> (Evaluation, Vec<CalcNeed>) {
        let source = CachedSource::new(&calcs.cache).retrying(&calcs.failed);
        let info = SessionInfo {
            layout_name: "Probe".into(),
            ..SessionInfo::default()
        };
        let mut host = FileHost::with_vars(
            FileCalc::new(&schema, &source, Date::from_ymd(2026, 11, 1).unwrap()),
            &info,
            Vars::Shared(variables),
        );
        (evaluate_text(text, &mut host, context), source.missing())
    };
    let context = Context::record(OCCURRENCE, 1);
    let text = format!("Let ( [ $$n = 0 ; ignored = {BOUNDARY_HIDE} ] ; If ( $$n = 3 ; Length ( Jobs::Two ) ; Length ( Jobs::Dead ) ) )");
    for member in [3, 2] {
        let (result, needs) = run(&calcs, &mut variables, &text, &context);
        assert_eq!(
            needs,
            [CalcNeed::MemberData {
                table: TABLE,
                member: vec![member]
            }]
        );
        // A control suspension is distinct from a simple missing body.
        assert_eq!(result.pending_execution, member == 3);
        let (next, needs2) = run(&calcs, &mut variables, "$$n", &context);
        assert!(!next.pending_execution);
        assert!(needs2.is_empty());
        assert_eq!(
            next.value
                .to_number(&Ctx::new(
                    &schema.locale,
                    Date::from_ymd(2026, 11, 1).unwrap()
                ))
                .to_i64(),
            Some(if member == 3 { 0 } else { 3 })
        );
        calcs.accept_data(&needs, Ok(container_body_reply(member)));
    }
    let (result, needs) = run(&calcs, &mut variables, &text, &context);
    assert!(!result.pending_execution);
    assert_eq!(result.code(), 0);
    assert!(needs.is_empty());
    for (text, context, error) in [("1 / 0", context, 15), ("Jobs::One", Context::new(), 1224)] {
        let (result, needs) = run(&calcs, &mut variables, text, &context);
        assert_eq!(result.code(), error);
        assert!(!result.pending_execution);
        assert!(needs.is_empty());
    }
}

#[test]
fn boundary_returned_values_keep_nested_pending_variable_dependencies() {
    for warm in [false, true] {
        boundary_run(
            &format!(
                "Let ( [ $x = Length ( Jobs::One ) ; $n = GetAsNumber ( {BOUNDARY_SQL} ) ] ; 0 )"
            ),
            "3 * IsValid ( $x )",
            "0",
            warm,
        );
        boundary_run(
            &format!("Let ( [ $x = Length ( Jobs::One ) ; $n = 3 * {BOUNDARY_HIDE} ] ; 0 )"),
            "0",
            "IsValid ( $x )",
            warm,
        );
    }
}
