use super::*;
use om_data::host::Privileges;

fn hidden_when(schema: &CalcSchema, id: u64, source: &str) -> LayoutObject {
    LayoutObject {
        id,
        calcs: om_layout::ObjectCalcs {
            hide: Some(formula(schema, source)),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn visibility(eval: &Evaluator<'_>, object: &LayoutObject, record: u64) -> Option<Visibility> {
    eval.object_state_in_portal(object, record, None, || Value::empty(ValueType::Text))
        .map(|calc| calc.visibility)
}

fn account(set: &str) -> SessionInfo {
    SessionInfo {
        privilege_set_name: set.into(),
        privileges: Some(Privileges {
            account_set_name: set.into(),
            ..Default::default()
        }),
        ..SessionInfo::default()
    }
}

#[test]
fn hide_conditions_tell_filemakers_results_from_what_the_window_lacks() {
    let mut schema = portal_schema();
    schema.add_relationship(om_data::host::RelationshipDef::new(
        1,
        OCCURRENCE,
        ROW_OCCURRENCE,
        vec![],
    ));
    let mut calcs = Calculations::default();
    portal_data(&mut calcs);
    let title = hidden_when(&schema, 1, "Jobs::Title = \"Second parent\"");
    let error = hidden_when(&schema, 2, "1 / 0");
    let set = hidden_when(
        &schema,
        3,
        "Get ( AccountPrivilegeSetName ) ≠ \"[Full Access]\"",
    );
    let related = hidden_when(&schema, 4, "Items::Label = \"Alder\"");
    let mut undecodable = LayoutObject {
        id: 5,
        ..Default::default()
    };
    undecodable.calcs.undecodable = vec![om_layout::calcs::HIDE];
    {
        let eval = evaluator(&calcs, &schema);
        // FileMaker's own results, `?` hiding as `If` takes it.
        assert_eq!(visibility(&eval, &title, 1), Some(Visibility::Shown));
        assert_eq!(visibility(&eval, &title, 2), Some(Visibility::Hidden));
        assert_eq!(visibility(&eval, &error, 1), Some(Visibility::Hidden));
        assert_eq!(visibility(&eval, &related, 1), Some(Visibility::Shown));
        // A condition that did not decode, and one reading the privilege set
        // the window could not read: not FileMaker's answers.
        assert_eq!(
            visibility(&eval, &undecodable, 1),
            Some(Visibility::Unresolved)
        );
        assert_eq!(visibility(&eval, &set, 1), Some(Visibility::Unresolved));
        // Parent 2's related records are still being fetched.
        assert_eq!(visibility(&eval, &related, 2), None);
    }
    for (set_name, expected) in [
        ("[Full Access]", Visibility::Shown),
        ("[Read-Only Access]", Visibility::Hidden),
    ] {
        let mut eval = evaluator(&calcs, &schema);
        eval.info = account(set_name);
        calcs.observe_context(&eval.info);
        assert_eq!(visibility(&eval, &set, 1), Some(expected), "{set_name}");
    }
    // The provider cannot supply them: OpenMaker cannot tell, rather than
    // hiding the object as if FileMaker's condition held.
    let needs = calcs.take_wanted();
    assert!(!needs.is_empty());
    calcs.accept_data(&needs, Ok(CalcData::default()));
    let eval = evaluator(&calcs, &schema);
    assert_eq!(visibility(&eval, &related, 2), Some(Visibility::Unresolved));
    assert_eq!(visibility(&eval, &related, 1), Some(Visibility::Shown));
}

#[test]
fn results_that_read_the_window_follow_its_context_and_others_stay() {
    let schema = portal_schema();
    let mut calcs = Calculations::default();
    portal_data(&mut calcs);
    let current = hidden_when(&schema, 1, "Get ( ActiveRecordNumber ) ≠ 1");
    let data = hidden_when(&schema, 2, "Jobs::Title = \"Second parent\"");
    let at = |active| SessionInfo {
        active_record_number: Some(active),
        ..account("[Full Access]")
    };
    let first = Evaluator {
        info: at(1),
        ..evaluator(&calcs, &schema)
    };
    calcs.observe_context(&first.info);
    assert_eq!(visibility(&first, &current, 1), Some(Visibility::Shown));
    assert_eq!(visibility(&first, &data, 1), Some(Visibility::Shown));
    // A portal whose filter reads the window, and one that does not.
    let mut filtered = portal_object();
    filtered.calcs.portal_filter = Some(formula(&schema, "Get ( ActiveRecordNumber ) = 1"));
    let mut plain = portal_object();
    plain.id += 1;
    assert!(!first.portal_records(&filtered, 1).unwrap().is_empty());
    assert!(!first.portal_records(&plain, 1).unwrap().is_empty());
    // The same context keeps both.
    calcs.observe_context(&at(1));
    assert_eq!(calcs.states.borrow().len(), 2);
    assert_eq!(calcs.portal_sets.borrow().len(), 2);
    // Another current record on the same page: only the result that read
    // `Get` is evaluated again.
    let second = Evaluator {
        info: at(2),
        ..evaluator(&calcs, &schema)
    };
    calcs.observe_context(&second.info);
    assert!(!calcs.states.borrow().contains_key(&(1, 1, 1, None)));
    assert!(calcs.states.borrow().contains_key(&(1, 2, 1, None)));
    assert_eq!(visibility(&second, &current, 1), Some(Visibility::Hidden));
    assert_eq!(visibility(&second, &data, 1), Some(Visibility::Shown));
    // Likewise a portal's rows: the filtered set is evaluated again.
    assert!(!calcs
        .portal_sets
        .borrow()
        .contains_key(&(1, filtered.id, 1)));
    assert!(calcs.portal_sets.borrow().contains_key(&(1, plain.id, 1)));
    assert!(second.portal_records(&filtered, 1).unwrap().is_empty());
}

#[test]
fn self_preserves_unknown_and_session_provenance() {
    let mut schema = portal_schema();
    schema.add_field(
        TABLE,
        FieldDef::calculation(
            3,
            "Set",
            ValueType::Text,
            formula(&schema, "Get ( AccountPrivilegeSetName )"),
            false,
        ),
    );
    let mut self_object = hidden_when(&schema, 101, "Self ≠ \"[Full Access]\"");
    self_object.object_type = 1;
    self_object.field_obj = Some(om_layout::FieldObject {
        field_spec: Some(om_layout::FieldSpec {
            table_key: LAYOUT_OCCURRENCE,
            field_key: 3,
            repetition: 1,
            ..Default::default()
        }),
        ..Default::default()
    });
    let direct = hidden_when(&schema, 102, "Jobs::Set ≠ \"[Full Access]\"");
    let mut calcs = Calculations::default();
    portal_data(&mut calcs);
    let unknown = evaluator(&calcs, &schema);
    calcs.observe_context(&unknown.info);
    let self_visibility = |eval: &Evaluator<'_>| {
        eval.object_state_in_portal(&self_object, 1, None, || {
            eval.field_value_in_portal(101, self_object.field_spec().unwrap(), 1, None)
                .unwrap_or_else(|| Value::empty(ValueType::Text))
        })
        .map(|state| state.visibility)
    };
    // An unknown field input does not taint a hide condition that never
    // reads Self.
    let mut constant = self_object.clone();
    constant.id = 103;
    constant.calcs.hide = Some(formula(&schema, "0"));
    assert_eq!(visibility(&unknown, &constant, 1), Some(Visibility::Shown));
    let unknown_self = self_visibility(&unknown);
    let unknown_direct = visibility(&unknown, &direct, 1);
    let known = Evaluator {
        info: account("[Full Access]"),
        ..evaluator(&calcs, &schema)
    };
    calcs.observe_context(&known.info);
    let known_self = self_visibility(&known);
    let known_direct = visibility(&known, &direct, 1);
    assert_eq!(
        (unknown_self, known_self, unknown_direct, known_direct),
        (
            Some(Visibility::Unresolved),
            Some(Visibility::Shown),
            Some(Visibility::Unresolved),
            Some(Visibility::Shown)
        )
    );
}

#[test]
fn cached_summary_inputs_preserve_unknown_and_session_provenance() {
    use om_data::host::{SummaryDef, SummaryKind};
    let mut schema = portal_schema();
    for (field, name, expression, summary, total) in [
        (
            3,
            "Set length",
            "Length ( Get ( AccountPrivilegeSetName ) )",
            4,
            "Set total",
        ),
        (5, "Active", "Get ( ActiveRecordNumber )", 6, "Active total"),
    ] {
        schema.add_field(
            TABLE,
            FieldDef::calculation(
                field,
                name,
                ValueType::Number,
                formula(&schema, expression),
                false,
            ),
        );
        schema.add_field(
            TABLE,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(SummaryDef::new(SummaryKind::Total, field)),
                ..FieldDef::data(summary, total, ValueType::Number)
            },
        );
    }
    let mut calcs = Calculations::default();
    portal_data(&mut calcs);
    calcs
        .cache
        .get_mut()
        .found
        .insert(OCCURRENCE, Arc::from([1, 2]));
    let first = hidden_when(&schema, 10, "Jobs::Set total = 0");
    let second = hidden_when(&schema, 11, "Jobs::Set total = 0");
    let active = hidden_when(&schema, 12, "Jobs::Active total = 2");
    {
        let eval = evaluator(&calcs, &schema);
        calcs.observe_context(&eval.info);
        // Warm through a value-only consumer, then two independent hide
        // evaluations must inherit the cached summary input's provenance.
        assert!(eval.named_field_value("Set total", 1).is_some());
        assert_eq!(visibility(&eval, &first, 1), Some(Visibility::Unresolved));
        assert_eq!(visibility(&eval, &second, 1), Some(Visibility::Unresolved));
    }
    for (number, expected) in [(1, Visibility::Hidden), (2, Visibility::Shown)] {
        let eval = Evaluator {
            info: SessionInfo {
                active_record_number: Some(number),
                ..account("[Full Access]")
            },
            ..evaluator(&calcs, &schema)
        };
        calcs.observe_context(&eval.info);
        assert_eq!(visibility(&eval, &first, 1), Some(Visibility::Shown));
        assert_eq!(visibility(&eval, &active, 1), Some(expected));
    }
}
