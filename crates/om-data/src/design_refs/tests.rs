use super::*;

/// A graph of scripts 1 to 5 and the performs between them.
fn scripts(performs: &[(u64, u64)]) -> DesignGraph {
    let mut graph = DesignGraph::default();
    for id in 1..=5 {
        let id = ObjectId::Script(id);
        graph.objects.insert(
            id,
            Object {
                id,
                name: String::new(),
                detail: String::new(),
            },
        );
    }
    for (n, (from, to)) in performs.iter().enumerate() {
        graph.uses.push(Use {
            holder: ObjectId::Script(*from),
            place: Place {
                step: Some(StepPlace {
                    index: n + 1,
                    line: n + 1,
                    id: 1,
                    name: "Perform Script".into(),
                    enabled: true,
                }),
                part: "Script".into(),
            },
            target: Target::Object(ObjectId::Script(*to)),
            resolution: Resolution::Resolved(ObjectId::Script(*to)),
        });
    }
    graph
}

/// Cycles end the walk and are reported in both directions; each script
/// is listed once, nearest first, the start again only when a cycle comes
/// back to it.
#[test]
fn call_chains_walk_each_script_once_and_report_cycles() {
    let graph = scripts(&[(1, 2), (2, 3), (3, 2), (3, 1), (4, 4)]);
    let reached = |chain: &CallChain<'_>| -> Vec<(u64, usize)> {
        chain.scripts.iter().map(|r| (r.script, r.depth)).collect()
    };
    let calls = graph.call_chain(1, Direction::Calls);
    assert_eq!(reached(&calls), [(2, 1), (3, 2), (1, 3)]);
    assert_eq!(calls.cycles, [vec![2, 3], vec![1, 2, 3]]);
    assert_eq!(calls.calls.len(), 4);
    let callers = graph.call_chain(1, Direction::Callers);
    assert_eq!(reached(&callers), [(3, 1), (2, 2), (1, 3)]);
    assert_eq!(callers.cycles, [vec![1, 3, 2], vec![3, 2]]);
    // A script performing itself is a cycle of one; one nothing performs
    // has no callers.
    let own = graph.call_chain(4, Direction::Calls);
    assert_eq!(reached(&own), [(4, 1)]);
    assert_eq!(own.cycles, [vec![4]]);
    assert!(graph.call_chain(5, Direction::Callers).scripts.is_empty());
    // A self-call is not a use by another object.
    assert!(graph.uses_of(ObjectId::Script(4)).is_empty());
    let unused: Vec<ObjectId> = graph
        .no_reference_found(Kind::Script)
        .into_iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(unused, [ObjectId::Script(4), ObjectId::Script(5)]);
}

mod completeness {
    //! Every kind of place the builder reads, fed one case it cannot read,
    //! leaves the kinds that place could refer to incomplete, so nothing
    //! of those kinds is reported unreferenced.

    use om_formula::{Key, NameTable};
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, KeySegment, KeyTree, Node, OwnedNode};
    use om_layout::encode;
    use om_scripts::{Calculation, FieldSpec, Script, ScriptStep, StoredKey};

    use super::super::access::Declared;
    use super::super::read::{build_traced, build_within, Structure};
    use super::super::*;

    const TABLE: u64 = 0x8001;
    const OCCURRENCE: u64 = 0xd0_0001;
    const A: u64 = 1;
    const B: u64 = 2;
    const LAYOUT: u64 = 1;
    const OBJECT: u64 = 1;
    const SCRIPT: u64 = 1;
    const LIST: u64 = 1;
    const FUNCTION: u64 = 1;
    const SET: u64 = 1;

    fn view(node: &OwnedNode) -> Node<'_> {
        Node {
            segment: KeySegment(&node.segment),
            value: node.value.as_deref(),
            push: node.push,
            children: node.children.iter().map(view).collect(),
        }
    }

    fn graph(root: &OwnedNode) -> DesignGraph {
        build(&KeyTree { root: view(root) }, Vec::new())
    }

    fn names() -> NameTable {
        let mut names = NameTable::default();
        names.add_occurrence(OCCURRENCE, "Things", TABLE);
        names.add_field(TABLE, A, "A");
        names.add_field(TABLE, B, "B");
        names.add_custom_function(FUNCTION, "F", 0);
        names
    }

    /// `text` compiled in the occurrence's context: its formatted and
    /// Polish streams.
    fn compiled(text: &str) -> (Vec<u8>, Vec<u8>) {
        let names = names();
        let mut ctx = om_formula::Context::new(&names);
        ctx.occurrence = Some(OCCURRENCE);
        let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        (
            om_formula::formatted::encode(&compiled.tokens),
            om_formula::polish::encode(&compiled.expr),
        )
    }

    /// A stored formula node: context, formatted and Polish streams.
    fn formula(segment: &[u8], text: &str) -> OwnedNode {
        let (formatted, polish) = compiled(text);
        OwnedNode::list(
            segment.to_vec(),
            vec![
                OwnedNode::leaf(vec![4], Key::from_id(OCCURRENCE).0),
                OwnedNode::leaf(vec![5], formatted),
                OwnedNode::leaf(vec![6], polish),
            ],
        )
    }

    fn step_calculation(text: &str) -> Calculation {
        let (formatted, polish) = compiled(text);
        Calculation::new(Some(formatted), polish)
    }

    /// A field definition (schema-path.md 5.5): kind, text, the option
    /// and validation words, one repetition.
    fn definition(kind: u8, options: u32, validation: u32) -> Vec<u8> {
        let mut out = vec![kind, 1, 0, 0, 0, 0, 0, 21];
        out.extend(options.to_be_bytes());
        out.extend(validation.to_be_bytes());
        out.extend(1u32.to_be_bytes());
        out.extend(0u32.to_be_bytes());
        out.extend(1u16.to_be_bytes());
        out
    }

    fn text(root: &mut OwnedNode, path: &[&[u8]], value: &str) {
        root.put(path, om_scripts::text_node(&[], value));
    }

    fn put(root: &mut OwnedNode, path: &[&[u8]], value: Vec<u8>) {
        root.put(path, OwnedNode::leaf(Vec::new(), value));
    }

    fn add_script(root: &mut OwnedNode, id: u64, name: &str, steps: Vec<ScriptStep>) {
        let mut script = Script {
            id,
            name: name.into(),
            ..Script::default()
        };
        script.insert_steps(0, steps).unwrap();
        let node = script.encode();
        let segment = node.segment.clone();
        root.put(&[&[17], &[5], &segment], node);
        // The last data key issued, as FileMaker stores it.
        let last = serial::encode(script.steps.len() as u64);
        let mut counter = vec![last.len() as u8];
        counter.extend(last);
        put(root, &[&[17], &[5], &segment, &[5], &[0]], counter);
    }

    /// Set Field [Things::field; "x"].
    fn set_field(field: u64) -> ScriptStep {
        let mut step = ScriptStep::new(76);
        step.set_calculation(0, Some(&step_calculation("\"x\"")));
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                StoredKey::from_id(OCCURRENCE).unwrap(),
                StoredKey::from_id(field).unwrap(),
                0,
            )),
        );
        step
    }

    fn object_bounds() -> om_layout::Rect {
        om_layout::Rect {
            top: 10.0,
            left: 10.0,
            bottom: 30.0,
            right: 110.0,
            other: Vec::new(),
        }
    }

    /// A file every place of which is read: a table of A and B (the
    /// calculation `A`), its occurrence, a layout showing A, the script Go
    /// setting A, a custom value list, the custom function F and [Full
    /// Access].
    fn base() -> OwnedNode {
        let mut root = OwnedNode::default();
        let t = uint_segment(TABLE);
        text(&mut root, &[&[3], &[16], &[5], &t, &[0x10]], "Things");
        for (field, name, kind) in [(A, "A", 1u8), (B, "B", 2)] {
            let f = uint_segment(field);
            text(&mut root, &[&t, &[3], &[5], &f, &[0x10]], name);
            put(
                &mut root,
                &[&t, &[3], &[5], &f, &[2]],
                definition(kind, 0, 0),
            );
        }
        root.put(
            &[&t, &[3], &[5], &uint_segment(B), &[5]],
            formula(&[5], "A"),
        );
        let o = uint_segment(OCCURRENCE);
        text(&mut root, &[&[3], &[17], &[5], &o, &[0x10]], "Things");
        let mut occurrence = vec![2, 1, 0, t.len() as u8 + 1, t.len() as u8];
        occurrence.extend(&t);
        put(&mut root, &[&[3], &[17], &[5], &o, &[2]], occurrence);
        // The layout: its occurrence and one edit box showing A.
        let l = uint_segment(LAYOUT);
        let alias = serial::decode(&o).unwrap();
        text(&mut root, &[&[4], &[5], &l, &[0x10]], "Things");
        put(
            &mut root,
            &[&[4], &[5], &l, &[2]],
            encode::new_layout_def(alias, 400.0),
        );
        let object = encode::field_object(OBJECT, &object_bounds(), alias, A);
        put(
            &mut root,
            &[&[4], &[5], &l, &[7]],
            encode::append_to_store(&[], &[OBJECT], &[object]),
        );
        text(
            &mut root,
            &[&[4], &[5], &l, &[5], &serial::encode(OBJECT), &[0x10]],
            "",
        );
        add_script(&mut root, SCRIPT, "Go", vec![set_field(A)]);
        let v = uint_segment(LIST);
        text(&mut root, &[&[0x21], &[5], &v, &[0x10]], "Choices");
        text(&mut root, &[&[0x21], &[5], &v, &[3]], "x\ry");
        let c = uint_segment(FUNCTION);
        text(&mut root, &[&[0x10], &[5], &c, &[0x10]], "F");
        root.put(&[&[0x10], &[5], &c, &[0x23]], formula(&[0x23], "1"));
        let p = uint_segment(SET);
        text(
            &mut root,
            &[&[23], &[2], &[5], &p, &[0x10]],
            "[Full Access]",
        );
        put(&mut root, &[&[23], &[2], &[5], &p, &[2]], vec![0x55, 0x03]);
        root
    }

    /// `mutate` the base file in one place: the kinds `kinds` lose
    /// complete coverage, nothing of them is unreferenced, and a new
    /// unread or by-name entry names that place: `at`'s holder and a part
    /// containing `at`'s text.
    fn check(
        case: &str,
        mutate: impl FnOnce(&mut OwnedNode),
        kinds: &[Kind],
        at: (ObjectId, &str),
    ) {
        let before = graph(&base());
        for kind in kinds {
            assert!(
                before.coverage().nothing_known_unread_for(*kind),
                "{case}: the base reads every place"
            );
        }
        let mut root = base();
        mutate(&mut root);
        let after = graph(&root);
        for kind in kinds {
            assert!(
                !after.coverage().nothing_known_unread_for(*kind),
                "{case}: {kind:?} coverage is complete\n{:#?}",
                after.coverage()
            );
        }
        let (holder, part) = at;
        let named = after
            .coverage()
            .unread
            .iter()
            .map(|u| (u.holder, &u.place))
            .chain(
                after
                    .coverage()
                    .by_name
                    .iter()
                    .map(|b| (b.holder, &b.place)),
            )
            .any(|(h, p)| h == holder && p.part.contains(part));
        assert!(
            named,
            "{case}: no entry names {holder:?} {part:?}\n{:#?}",
            after.coverage()
        );
    }

    fn with_step(root: &mut OwnedNode, step: ScriptStep) {
        add_script(root, 2, "Other", vec![step]);
    }

    const OTHER: ObjectId = ObjectId::Script(2);

    #[test]
    fn the_base_file_is_read_completely() {
        let graph = graph(&base());
        for kind in Kind::ANY {
            assert!(
                graph.coverage().nothing_known_unread_for(kind),
                "{kind:?}: {:#?}",
                graph.coverage()
            );
        }
        assert!(
            graph.targets_not_found().is_empty(),
            "{:#?}",
            graph.targets_not_found()
        );
        let unused: Vec<ObjectId> = graph
            .no_reference_found(Kind::Script)
            .into_iter()
            .map(|o| o.id)
            .collect();
        assert_eq!(unused, [ObjectId::Script(SCRIPT)]);
        assert!(graph.coverage().nothing_known_unread_for(Kind::Script));
        // A, through the layout, the calculation and the script.
        assert_eq!(
            graph
                .uses_of(ObjectId::Field {
                    table: TABLE,
                    field: A
                })
                .len(),
            3
        );
    }

    #[test]
    fn every_place_that_cannot_be_read_leaves_its_kinds_incomplete() {
        let t = uint_segment(TABLE);
        let l = uint_segment(LAYOUT);
        let any = &Kind::ANY;
        let field_a = ObjectId::Field {
            table: TABLE,
            field: A,
        };
        let field_b = ObjectId::Field {
            table: TABLE,
            field: B,
        };
        let layout = ObjectId::Layout(LAYOUT);
        let object = ObjectId::LayoutObject {
            layout: LAYOUT,
            object: OBJECT,
        };
        check(
            "an unknown key of a field definition",
            |root| put(root, &[&t, &[3], &[5], &uint_segment(A), &[0x77]], vec![1]),
            any,
            (field_a, "stored 32769/3/5/1/119"),
        );
        check(
            "a calculation that does not decode",
            |root| {
                put(
                    root,
                    &[&t, &[3], &[5], &uint_segment(B), &[5], &[6]],
                    vec![0xff, 0xff],
                )
            },
            any,
            (field_b, "calculation"),
        );
        check(
            "a formula stored without its compiled stream",
            |root| {
                root.remove(&[&t, &[3], &[5], &uint_segment(B), &[5], &[6]]);
            },
            any,
            (field_b, "calculation"),
        );
        check(
            "a formula's context that does not assemble",
            |root| {
                root.put(
                    &[&t, &[3], &[5], &uint_segment(B), &[5], &[4]],
                    OwnedNode::list(vec![4], vec![OwnedNode::leaf(vec![1], vec![1])]),
                );
            },
            &[Kind::Occurrence],
            (field_b, "calculation"),
        );
        check(
            "an unknown key nested in a field's formula",
            |root| {
                put(
                    root,
                    &[&t, &[3], &[5], &uint_segment(B), &[5], &[0x7e]],
                    vec![1],
                )
            },
            any,
            (field_b, "stored 32769/3/5/2/5/126"),
        );
        check(
            "a field of an unknown kind",
            |root| {
                put(
                    root,
                    &[&t, &[3], &[5], &uint_segment(A), &[2]],
                    definition(9, 0, 0),
                )
            },
            any,
            (field_a, "definition"),
        );
        check(
            "a step without a parameter schema",
            |root| with_step(root, ScriptStep::new(999)),
            any,
            (OTHER, "step"),
        );
        check(
            "an unknown key of a script",
            |root| {
                put(
                    root,
                    &[&[17], &[5], &serial::encode(SCRIPT), &[0x77]],
                    vec![1],
                )
            },
            any,
            (ObjectId::Script(SCRIPT), "stored 17/5/1/119"),
        );
        check(
            "a script's key 13 holding a value",
            |root| {
                put(
                    root,
                    &[&[17], &[5], &serial::encode(SCRIPT), &[0x13]],
                    vec![1],
                )
            },
            any,
            (ObjectId::Script(SCRIPT), "stored 17/5/1/19"),
        );
        check(
            "step data no step names",
            |root| {
                root.put(
                    &[
                        &[17],
                        &[5],
                        &serial::encode(SCRIPT),
                        &[5],
                        &serial::encode(9),
                    ],
                    OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![1], vec![1])]),
                )
            },
            any,
            (ObjectId::Script(SCRIPT), "stored 17/5/1/5/9"),
        );
        check(
            "a field operand that does not read",
            |root| {
                let mut step = set_field(A);
                step.set_slot_child(0, 0x10, Some(OwnedNode::leaf(vec![0x10], vec![0xff])));
                step.set_slot_child(1, 0x10, Some(OwnedNode::leaf(vec![0x10], vec![0xff])));
                with_step(root, step);
            },
            &[Kind::Field, Kind::Occurrence],
            (OTHER, ""),
        );
        check(
            "Perform Script by name, which may name a data source",
            |root| {
                let mut step = ScriptStep::new(1);
                step.set_list_value(1, Some(1));
                step.set_calculation(2, Some(&step_calculation("\"Billing::Post\"")));
                with_step(root, step);
            },
            &[Kind::Script, Kind::DataSource],
            (OTHER, "Specify by name"),
        );
        check(
            "Go to Related Record to a calculated layout",
            |root| {
                let mut step = ScriptStep::new(74);
                step.value = 3;
                step.set_member_key(StoredKey::from_id(LAYOUT));
                step.set_calculation(5, Some(&step_calculation("\"Things\"")));
                with_step(root, step);
            },
            &[Kind::Layout],
            (OTHER, "Layout"),
        );
        check(
            "GetLayoutObjectAttribute",
            |root| {
                let mut step = ScriptStep::new(141);
                step.set_text(0, Some("$bounds"));
                step.set_calculation(
                    1,
                    Some(&step_calculation(
                        "GetLayoutObjectAttribute ( \"Total\" ; \"bounds\" )",
                    )),
                );
                with_step(root, step);
            },
            &[Kind::LayoutObject],
            (OTHER, ""),
        );
        check(
            "Open URL, which may perform a script by name",
            |root| with_step(root, ScriptStep::new(111)),
            &[Kind::Script],
            (OTHER, "URL"),
        );
        check(
            "Generate Response from Model's web viewer and tool definitions",
            |root| {
                let mut step = ScriptStep::new(220);
                step.set_calculation(20, Some(&step_calculation("\"Viewer\"")));
                step.set_calculation(13, Some(&step_calculation("\"[]\"")));
                with_step(root, step);
            },
            &[Kind::LayoutObject, Kind::CustomFunction],
            (OTHER, "Web viewer"),
        );
        check(
            "an unknown key of a layout",
            |root| put(root, &[&[4], &[5], &l, &[0x77]], vec![1]),
            any,
            (layout, "stored 4/5/1/119"),
        );
        check(
            "an unknown attribute of an object",
            |root| {
                put(
                    root,
                    &[&[4], &[5], &l, &[5], &serial::encode(OBJECT), &[0x77]],
                    vec![1],
                )
            },
            any,
            (object, "stored 4/5/1/5/1/119"),
        );
        check(
            "fields of an object's record its decoder does not know",
            |root| {
                let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
                let mut object = encode::field_object(OBJECT, &object_bounds(), alias, A);
                encode::uint(99, 1, &mut object);
                put(
                    root,
                    &[&[4], &[5], &l, &[7]],
                    encode::append_to_store(&[], &[OBJECT], &[object]),
                );
            },
            any,
            (object, ""),
        );
        check(
            "an object store that does not assemble",
            |root| {
                root.put(
                    &[&[4], &[5], &l, &[7]],
                    OwnedNode::list(vec![7], vec![OwnedNode::leaf(vec![1], vec![1])]),
                );
            },
            any,
            (layout, "stored 4/5/1/7"),
        );
        check(
            "a table view whose object store does not assemble",
            |root| {
                root.put(
                    &[&[4], &[5], &l, &[0x0d], &[7]],
                    OwnedNode::list(vec![7], vec![OwnedNode::leaf(vec![1], vec![1])]),
                );
            },
            any,
            (layout, "stored 4/5/1/13/7"),
        );
        check(
            "a part definition that does not assemble",
            |root| {
                root.put(
                    &[&[4], &[5], &l, &[3], &[1], &[2]],
                    OwnedNode::list(vec![2], vec![OwnedNode::leaf(vec![1], vec![1])]),
                );
            },
            any,
            (layout, "stored 4/5/1/3/1/2"),
        );
        check(
            "a field index entry naming an object the layout does not have",
            |root| put(root, &[&[4], &[5], &l, &[1], &[9], &[9], &[9]], Vec::new()),
            &[Kind::Field, Kind::Occurrence],
            (layout, "stored 4/5/1/1/9"),
        );
        check(
            "a layout trigger record that does not read",
            |root| {
                put(
                    root,
                    &[&[4], &[5], &l, &[0x30], &[5], &[103], &[1]],
                    vec![0],
                )
            },
            &[Kind::Script, Kind::DataSource],
            (layout, "script trigger OnRecordLoad"),
        );
        check(
            "a calculated value list without its compiled stream",
            |root| {
                let v = uint_segment(LIST);
                let mut definition = vec![3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                definition.extend(om_scsu::encode_counted(" "));
                put(root, &[&[0x21], &[5], &v, &[2]], definition);
                let mut calculation = formula(&[5], "\"a\"");
                calculation.remove(&[&[6]]);
                root.put(&[&[0x21], &[5], &v, &[5]], calculation);
            },
            any,
            (ObjectId::ValueList(LIST), "calculation"),
        );
        check(
            "a relationship that does not read",
            |root| put(root, &[&[3], &[0xfb], &[5], &[1], &[2]], vec![0xff]),
            &[Kind::Occurrence, Kind::Field],
            (ObjectId::Relationship(1), "definition"),
        );
        check(
            "an occurrence definition that does not read",
            |root| {
                put(
                    root,
                    &[&[3], &[17], &[5], &uint_segment(OCCURRENCE), &[2]],
                    vec![0xff],
                )
            },
            &[Kind::Table, Kind::DataSource],
            (ObjectId::Occurrence(OCCURRENCE), ""),
        );
        check(
            "a custom function body that does not decode",
            |root| {
                put(
                    root,
                    &[&[0x10], &[5], &uint_segment(FUNCTION), &[0x23], &[6]],
                    vec![0xff],
                )
            },
            any,
            (ObjectId::CustomFunction(FUNCTION), "body"),
        );
        check(
            "a record access calculation naming a value list by name",
            |root| {
                root.put(
                    &[&[23], &[2], &[5], &uint_segment(SET), &[5], &t, &[9]],
                    formula(&[9], "ValueListItems ( Get ( FileName ) ; \"Choices\" )"),
                )
            },
            &[Kind::ValueList],
            (ObjectId::PrivilegeSet(SET), "record access"),
        );
        check(
            "a record access calculation without its compiled stream",
            |root| {
                let mut calculation = formula(&[6], "A = 1");
                calculation.remove(&[&[6]]);
                root.put(
                    &[&[23], &[2], &[5], &uint_segment(SET), &[5], &t, &[6]],
                    calculation,
                )
            },
            any,
            (ObjectId::PrivilegeSet(SET), "record access"),
        );
        check(
            "privilege sets not given",
            |root| {
                root.remove(&[&[23]]);
            },
            any,
            (ObjectId::File, "privilege sets"),
        );
        check(
            "a custom menu",
            |root| text(root, &[&[0x41], &[5], &[1], &[0x10]], "Menu"),
            any,
            (ObjectId::CustomMenu(1), ""),
        );
        check(
            "catalog 64 holding more than a UUID",
            |root| put(root, &[&[0x40], &[5], &[1], &[0x77]], vec![1]),
            any,
            (ObjectId::File, "stored 64/5/1/119"),
        );
    }

    /// The stored key of a step whose runtime takes its layout from a
    /// calculation does not count as a use of that layout.
    #[test]
    fn a_calculated_layout_does_not_use_the_stored_one() {
        let mut root = base();
        let mut step = ScriptStep::new(74);
        step.value = 4;
        step.set_member_key(StoredKey::from_id(LAYOUT));
        step.set_calculation(5, Some(&step_calculation("2")));
        with_step(&mut root, step);
        let graph = graph(&root);
        assert!(graph.uses_of(ObjectId::Layout(LAYOUT)).is_empty());
        assert!(graph
            .coverage()
            .by_name
            .iter()
            .any(|b| b.how == NameUse::LayoutByCalculation));
    }

    /// A reference to a layout the file does not have is broken only while
    /// every layout was listed: a member whose key does not read leaves it
    /// unresolved.
    #[test]
    fn a_target_is_missing_only_from_a_catalog_listed_whole() {
        let mut root = base();
        let mut go = ScriptStep::new(6);
        go.set_member_key(StoredKey::from_id(9));
        with_step(&mut root, go);
        let whole = graph(&root);
        let broken: Vec<_> = whole.targets_not_found().iter().map(|u| u.holder).collect();
        assert_eq!(broken, [OTHER], "{:#?}", whole.uses());
        text(
            &mut root,
            &[&[4], &[5], &[1, 2, 3, 4, 5, 6, 7, 8, 9], &[0x10]],
            "Lost",
        );
        let lost = graph(&root);
        assert!(
            lost.targets_not_found().is_empty(),
            "{:#?}",
            lost.targets_not_found()
        );
        assert!(lost
            .uses()
            .iter()
            .any(|u| u.holder == OTHER && matches!(u.resolution, Resolution::Unresolved { .. })));
    }

    /// The base with a reference read before its target's catalog: field
    /// B's formula calls the custom function F.
    fn forward() -> OwnedNode {
        let mut root = base();
        root.put(
            &[&uint_segment(TABLE), &[3], &[5], &uint_segment(B), &[5]],
            formula(&[5], "A & F"),
        );
        root
    }

    /// A build that meets its budget stops and says where in a fixed part
    /// of its coverage, no kind is complete, and no reference is broken:
    /// at every budget, wherever the stop falls between a reference and
    /// its target.
    #[test]
    fn a_build_stops_at_its_budgets_and_says_where() {
        let root = forward();
        let tree = KeyTree { root: view(&root) };
        let full = build(&tree, Vec::new());
        assert!(full.coverage().stopped.is_none());
        assert!(
            full.targets_not_found().is_empty(),
            "{:#?}",
            full.targets_not_found()
        );
        let mut unresolved = 0;
        let mut stops = 0;
        for budget in 1..2_000 {
            for (entries, work) in [(budget, MAX_WORK), (MAX_ENTRIES, budget)] {
                let graph = build_within(&tree, Vec::new(), entries, work);
                let Some(stopped) = &graph.coverage().stopped else {
                    continue;
                };
                stops += 1;
                assert_eq!(stopped.reason, UnreadReason::Limit);
                assert!(graph.uses().len() <= entries);
                assert!(
                    graph.targets_not_found().is_empty(),
                    "budget {entries} {work}: {:#?}",
                    graph.targets_not_found()
                );
                for kind in Kind::ALL {
                    assert!(!graph.coverage().nothing_known_unread_for(kind), "{kind:?}");
                }
                unresolved += graph
                    .uses()
                    .iter()
                    .filter(|u| matches!(u.resolution, Resolution::Unresolved { .. }))
                    .count();
            }
        }
        assert!(stops > 0 && unresolved > 0, "{stops} stops");
    }

    /// The base with every structure the builder reads: a relationship, a
    /// data source, a layout part, a hide condition, a button, a table
    /// view, a record access calculation, and a script whose steps
    /// perform a script, go to a layout, sort, find and export.
    fn rich() -> OwnedNode {
        let mut root = base();
        let t = uint_segment(TABLE);
        let l = uint_segment(LAYOUT);
        let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
        // A self-join matching A to B (schema-path.md; as in the schema's
        // own test of relationships).
        let hex = |text: &str| -> Vec<u8> {
            let text: String = text.split_whitespace().collect();
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
                .collect()
        };
        root.put(
            &[&[3], &[0xfb], &[5], &[1]],
            OwnedNode::list(
                vec![1],
                vec![
                    OwnedNode::leaf(
                        vec![2],
                        hex("0403d00001 0403d00001 0000 0000 0000 0001 \
                             0403d00001 020101 0001 020100 020100 0001 00 01 00000000 00000000"),
                    ),
                    OwnedNode::list(
                        vec![3],
                        vec![OwnedNode::leaf(vec![1], hex("05 020101 020102"))],
                    ),
                ],
            ),
        );
        text(
            &mut root,
            &[&[0x20], &[5], &serial::encode(1), &[0x10]],
            "Other",
        );
        put(
            &mut root,
            &[&[4], &[5], &l, &[3], &[1], &[2]],
            encode::new_body_part(200.0),
        );
        root.put(
            &[&[4], &[5], &l, &[5], &serial::encode(OBJECT), &[0x8d]],
            formula(&[0x8d], "A = \"\""),
        );
        // A button performing Go, and a table view showing A.
        const BUTTON: u64 = 2;
        let mut reserved = [0u8; 14];
        reserved[..6].copy_from_slice(&StoredKey::from_id(SCRIPT).unwrap().0);
        let stored = root
            .child(&[4])
            .and_then(|n| n.child(&[5]))
            .and_then(|n| n.child(&l))
            .and_then(|n| n.child(&[7]))
            .and_then(|n| n.value.clone())
            .unwrap();
        let button = encode::button_object(
            BUTTON,
            &object_bounds(),
            1,
            0,
            om_scripts::flag::SCRIPT_KEY,
            &reserved,
        );
        put(
            &mut root,
            &[&[4], &[5], &l, &[7]],
            encode::append_to_store(&stored, &[BUTTON], &[button]),
        );
        text(
            &mut root,
            &[&[4], &[5], &l, &[5], &serial::encode(BUTTON), &[0x10]],
            "",
        );
        put(
            &mut root,
            &[&[4], &[5], &l, &[0x0d], &[7]],
            encode::append_to_store(
                &[],
                &[3],
                &[encode::table_column(3, &object_bounds(), alias, A)],
            ),
        );
        root.put(
            &[&[23], &[2], &[5], &uint_segment(SET), &[5], &t, &[6]],
            formula(&[6], "A = 1"),
        );
        // The field index of the layout's object showing A.
        put(
            &mut root,
            &[
                &[4],
                &[5],
                &l,
                &[1],
                &uint_segment(OCCURRENCE),
                &uint_segment(A),
                &serial::encode(OBJECT),
            ],
            Vec::new(),
        );
        let mut perform = ScriptStep::new(1);
        perform.set_script_key(StoredKey::from_id(SCRIPT));
        let mut go = ScriptStep::new(6);
        go.set_member_key(StoredKey::from_id(LAYOUT));
        let mut sort = ScriptStep::new(39);
        sort.set_data_leaf(
            0x0a,
            Some(OwnedNode::leaf(
                vec![0x0a],
                om_scripts::specs::SortSpec {
                    keep_sorted: true,
                    blanks_last: false,
                    fields: vec![om_scripts::specs::SortEntry {
                        field: om_scripts::specs::SpecField {
                            occurrence: Some(OCCURRENCE),
                            field: A,
                            repetition: 1,
                        },
                        summary: None,
                        descending: false,
                        value_list: None,
                        locale: 0,
                    }],
                }
                .encode(),
            )),
        );
        let mut find = ScriptStep::new(28);
        let requests = om_scripts::specs::encode_requests(&[om_scripts::specs::StoredRequest {
            omit: false,
            criteria: vec![(
                om_scripts::specs::SpecField {
                    occurrence: Some(OCCURRENCE),
                    field: A,
                    repetition: 1,
                },
                "x".into(),
            )],
        }]);
        find.set_data_leaf(0x06, Some(requests));
        add_script(&mut root, 3, "Steps", vec![perform, go, sort, find]);
        // A script long enough that its step list is stored in pieces.
        add_script(&mut root, 4, "Long", long_script());
        root
    }

    /// Forty Go to Layout steps: a step list stored in pieces (scripts.md).
    fn long_script() -> Vec<ScriptStep> {
        (0..40)
            .map(|_| {
                let mut go = ScriptStep::new(6);
                go.set_member_key(StoredKey::from_id(LAYOUT));
                go
            })
            .collect()
    }

    /// Every node under `node`, by its path.
    fn nodes(node: &OwnedNode, path: &mut Vec<Vec<u8>>, out: &mut Vec<Vec<Vec<u8>>>) {
        for child in &node.children {
            path.push(child.segment.clone());
            out.push(path.clone());
            nodes(child, path, out);
            path.pop();
        }
    }

    /// Whether the build's catalogs hold `path`: the design catalogs and
    /// each table's field catalog `[T, 3]`.
    fn in_catalogs(path: &[Vec<u8>]) -> bool {
        match path {
            [table, rest @ ..] if table.len() > 1 => rest.first().is_some_and(|s| s == &[3]),
            [catalog, rest @ ..] => match catalog.as_slice() {
                [23] => rest.first().is_some_and(|s| s == &[2]),
                [2] | [3] | [4] | [16] | [17] | [32] | [33] | [64] | [65] => true,
                _ => false,
            },
            [] => false,
        }
    }

    /// One change to one stored node.
    #[derive(Clone, Copy, Debug)]
    enum Mutation {
        /// A list replaced by a leaf.
        Leaf,
        /// A leaf replaced by a list.
        List,
        /// A value stored in pieces, its last piece gone.
        Truncate,
        /// A key no reader knows, added under a list.
        Key,
        /// A value stored beside a list's keys.
        ValueBeside,
        /// The node removed.
        Remove,
    }

    fn mutate(root: &OwnedNode, path: &[Vec<u8>], mutation: Mutation) -> Option<OwnedNode> {
        let at: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let node = root.at(&at)?.clone();
        let list = !node.children.is_empty();
        let mut out = root.clone();
        let changed = match mutation {
            Mutation::Leaf if list => OwnedNode::leaf(node.segment.clone(), vec![0x55]),
            Mutation::List if !list => OwnedNode::list(
                node.segment.clone(),
                vec![OwnedNode::leaf(vec![0x7e], vec![1])],
            ),
            Mutation::Truncate
                if list
                    && node.value.is_none()
                    && node.children.len() > 2
                    && node.assembled().is_some() =>
            {
                let mut node = node;
                node.children.pop();
                node
            }
            Mutation::Key if list => {
                let mut node = node;
                node.put(&[&[0x7e]], OwnedNode::leaf(Vec::new(), vec![1]));
                node
            }
            Mutation::ValueBeside if list => {
                let mut node = node;
                node.value = Some(vec![1]);
                node
            }
            Mutation::Remove => {
                out.remove(&at);
                return Some(out);
            }
            _ => return None,
        };
        out.put(&at, changed);
        Some(out)
    }

    fn signature(g: &DesignGraph) -> String {
        let objects: Vec<&Object> = g.objects().collect();
        format!("{:?}\n{:?}\n{objects:?}", g.uses(), g.coverage())
    }

    /// The member paths an object is stored at, and those of what it is
    /// stored in (a field's table).
    fn member_paths(id: ObjectId) -> Vec<Vec<Vec<u8>>> {
        let member = |catalog: &[&[u8]], key: Vec<u8>| -> Vec<Vec<u8>> {
            catalog
                .iter()
                .map(|s| s.to_vec())
                .chain([vec![5], key])
                .collect()
        };
        match id {
            ObjectId::Field { table, field } => vec![
                vec![uint_segment(table), vec![3], vec![5], uint_segment(field)],
                member(&[&[3], &[0x10]], uint_segment(table)),
            ],
            ObjectId::Table(t) => vec![member(&[&[3], &[0x10]], uint_segment(t))],
            ObjectId::Occurrence(o) => vec![member(&[&[3], &[0x11]], uint_segment(o))],
            ObjectId::Relationship(r) => vec![member(&[&[3], &[0xfb]], uint_segment(r))],
            ObjectId::Layout(l) => vec![member(&[&[4]], uint_segment(l))],
            ObjectId::Script(s) => vec![member(&[&[0x11]], serial::encode(s))],
            ObjectId::ValueList(v) => vec![member(&[&[0x21]], uint_segment(v))],
            ObjectId::CustomFunction(c) => vec![member(&[&[0x10]], uint_segment(c))],
            ObjectId::DataSource(d) => vec![member(&[&[0x20]], serial::encode(d))],
            _ => Vec::new(),
        }
    }

    /// `rich` with every other structure the readers know, so that the
    /// test of every node covers them all (checked against the readers'
    /// own list, [`Structure::ALL`] and [`Declared::ALL`]).
    fn full() -> OwnedNode {
        let mut root = rich();
        let t = uint_segment(TABLE);
        let l = uint_segment(LAYOUT);
        let o = serial::encode(OBJECT);
        let occurrence = uint_segment(OCCURRENCE);
        let alias = serial::decode(&occurrence).unwrap();
        // The file's status and options: its version, and the script Go
        // run when the first window opens.
        text(&mut root, &[&[2], &[3]], "12.0");
        put(
            &mut root,
            &[&[2], &[0x18]],
            vec![0, 0, 0, 0, 0, 0, 2, 1, 1, 0, 0],
        );
        // The occurrence's relationship index: itself, by relationship 1.
        put(
            &mut root,
            &[&[3], &[0x11], &[5], &occurrence, &[0xfb], &occurrence],
            vec![1, 1],
        );
        // An inactive auto-enter formula on A, which does not auto-enter.
        root.put(
            &[&t, &[3], &[5], &uint_segment(A), &[5]],
            formula(&[5], "B"),
        );
        // A layout folder, the layout's index entry and name index.
        let folder = uint_segment(9);
        text(&mut root, &[&[4], &[5], &folder, &[0x10]], "Folder");
        put(
            &mut root,
            &[&[4], &[5], &folder, &[0xf0]],
            vec![0, 0, 0, 0, 0, 0, 0, 1],
        );
        put(
            &mut root,
            &[&[4], &[5], &folder, &[2]],
            encode::new_layout_def(alias, 400.0),
        );
        put(
            &mut root,
            &[&[4], &[1], &[7], &l, &[2]],
            encode::layout_entry(alias, 0),
        );
        put(&mut root, &[&[4], &[5], &l, &[8], &[1, 2]], vec![1, 1]);
        // The edit box's web address, items, conditional format and sort.
        let attributes = [&[4][..], &[5], &l, &[5], &o];
        let under = |keys: &[&[u8]]| -> Vec<Vec<u8>> {
            attributes.iter().chain(keys).map(|s| s.to_vec()).collect()
        };
        fn at(path: &[Vec<u8>]) -> Vec<&[u8]> {
            path.iter().map(Vec::as_slice).collect()
        }
        root.put(&at(&under(&[&[0, 0], &[5]])), formula(&[5], "\"x\""));
        root.put(&at(&under(&[&[0x11], &[1], &[5]])), formula(&[5], "A"));
        put(&mut root, &at(&under(&[&[0x8a], &[0, 0], &[1]])), vec![0]);
        root.put(
            &at(&under(&[&[0x8a], &[0, 0], &[5]])),
            formula(&[5], "A = 1"),
        );
        put(
            &mut root,
            &at(&under(&[&[0x8a], &[0, 0], &[6]])),
            Vec::new(),
        );
        put(&mut root, &at(&under(&[&[0x8a], &[4]])), vec![0, 1]);
        put(
            &mut root,
            &at(&under(&[&[0x0a]])),
            om_scripts::specs::SortSpec {
                keep_sorted: true,
                blanks_last: false,
                fields: Vec::new(),
            }
            .encode(),
        );
        // The layout's OnLayoutEnter trigger, performing Go with a parameter.
        let trigger = om_layout::triggers::ScriptTrigger {
            event: 101,
            script: Some(SCRIPT),
            file_reference: None,
            external: false,
            modes: 1,
            option: 0,
            extra: Vec::new(),
            parameter: None,
        };
        put(&mut root, &[&[4], &[5], &l, &[0x30], &[4]], vec![0, 1]);
        put(
            &mut root,
            &[&[4], &[5], &l, &[0x30], &[5], &[101], &[1]],
            om_layout::triggers::encode_record(&trigger),
        );
        root.put(
            &[&[4], &[5], &l, &[0x30], &[5], &[101], &[0x31]],
            formula(&[0x31], "1"),
        );
        // A script folder, a deleted script, the catalog's index, and an
        // export order.
        text(
            &mut root,
            &[&[0x11], &[5], &serial::encode(5), &[0x10]],
            "Folder",
        );
        put(
            &mut root,
            &[&[0x11], &[5], &serial::encode(5), &[0xf0]],
            vec![0, 0, 0, 0, 0, 0, 0, 1],
        );
        root.put(
            &[&[0x11], &[5], &serial::encode(6)],
            OwnedNode::list(serial::encode(6), Vec::new()),
        );
        put(&mut root, &[&[0x11], &[1], &[3], &[1]], vec![1, 1]);
        let order = om_transfer::spec::ExportOptions {
            fields: vec![om_transfer::spec::ExportField {
                field: om_transfer::spec::FieldSpec::new(OCCURRENCE, A),
                sum_by: om_transfer::spec::FieldSpec::none(),
            }],
            group_by: Vec::new(),
            formatting: false,
            charset: om_transfer::ExportCharset::Windows,
            use_name_alias: None,
            field_names: None,
        };
        let mut export = ScriptStep::new(36);
        export.set_data_leaf(0x85, Some(om_scripts::bytes_node(&[0x85], &order.encode())));
        add_script(&mut root, 7, "Export", vec![export]);
        // A privilege set's object access, a custom menu, catalog 64.
        put(
            &mut root,
            &[&[0x17], &[2], &[5], &uint_segment(SET), &[9], &[1]],
            vec![1],
        );
        text(&mut root, &[&[0x41], &[5], &[1], &[0x10]], "Menu");
        put(&mut root, &[&[0x40], &[5], &[1], &[0xd8]], vec![1; 16]);
        root
    }

    /// The fixture holds every structure the readers know and uses every
    /// declaration: the lists come from the reader code
    /// ([`Structure::ALL`], [`Declared::ALL`], each generated with its
    /// enum), so a new reader or declaration fails here until the fixture
    /// holds one.
    #[test]
    fn the_fixture_holds_every_structure_the_readers_know() {
        let root = full();
        let (graph, trace) = build_traced(
            &KeyTree { root: view(&root) },
            Vec::new(),
            MAX_ENTRIES,
            MAX_WORK,
        );
        assert!(
            graph.coverage().read_cleanly(),
            "{:#?}",
            graph.coverage().anomalies
        );
        let missing: Vec<&Structure> = Structure::ALL
            .iter()
            .filter(|s| !trace.visited.contains(s))
            .collect();
        assert!(missing.is_empty(), "the fixture lacks {missing:?}");
        let unused: Vec<&Declared> = Declared::ALL
            .iter()
            .filter(|d| !trace.declared.contains(d))
            .collect();
        assert!(unused.is_empty(), "the fixture uses no {unused:?}");
    }

    /// The paths a holder's references are stored under: its member, and
    /// for a field its table's member, for a layout its index entry.
    fn holder_paths(id: ObjectId) -> Vec<Vec<Vec<u8>>> {
        match id {
            ObjectId::LayoutObject { layout, .. } => member_paths(ObjectId::Layout(layout)),
            ObjectId::Layout(l) => {
                let mut paths = member_paths(id);
                paths.push(vec![vec![4], vec![1], vec![7], uint_segment(l)]);
                paths
            }
            ObjectId::PrivilegeSet(p) => {
                vec![vec![vec![0x17], vec![2], vec![5], uint_segment(p)]]
            }
            ObjectId::CustomMenu(m) => vec![vec![vec![0x41], vec![5], uint_segment(m)]],
            ObjectId::File => vec![vec![vec![2], vec![0x18]]],
            _ => member_paths(id),
        }
    }

    /// Every node of a file holding every structure the readers know,
    /// corrupted one at a time: a list made a leaf and a leaf a list, a
    /// value in pieces truncated, an unknown key added, a value set beside
    /// keys. Each corruption is an error the read must notice: the read is
    /// then not clean, and nothing strong is claimed (no target not found,
    /// no kind listed whole, nothing known unread). The only exceptions:
    /// inside a subtree declared by spec (a catalog index, file status,
    /// access settings, a layout's own object indexes) or reported as a
    /// known structure not decoded (a custom menu), which by design is not
    /// read; and a value in pieces stored whole instead, which is the same
    /// value. A read still clean must then give the same result, byte
    /// for byte. Removing a node can only take information away: the same
    /// assertions hold for a read not clean; in a clean read no use
    /// appears on an object that was there, none is lost but those its
    /// holder stored under the node removed, and no target goes missing
    /// but what was removed. The walk covers whatever the file holds, with
    /// no list of structures.
    #[test]
    fn every_stored_node_is_read_or_reported() {
        let root = full();
        let (before, trace) = build_traced(
            &KeyTree { root: view(&root) },
            Vec::new(),
            MAX_ENTRIES,
            MAX_WORK,
        );
        assert!(
            before.coverage().read_cleanly(),
            "{:#?}",
            before.coverage().anomalies
        );
        assert!(
            before.targets_not_found().is_empty(),
            "{:#?}",
            before.targets_not_found()
        );
        let base = signature(&before);
        let by_spec: Vec<Vec<Vec<u8>>> = trace
            .declared_paths
            .iter()
            .filter(|(_, why)| {
                // Declared by spec, or reported by its reader: a place
                // reported is an anomaly (the read is then not clean) or a
                // known structure not decoded (a custom menu), coverage that
                // could name anything, whatever it holds.
                matches!(
                    why,
                    Declared::CatalogIndex
                        | Declared::FileStatus
                        | Declared::AccessSetting
                        | Declared::LayoutIndex
                        | Declared::Reported
                )
            })
            .map(|(path, _)| path.clone())
            .collect();
        let mut paths = Vec::new();
        nodes(&root, &mut Vec::new(), &mut paths);
        paths.retain(|p| in_catalogs(p));
        let mut failures = Vec::new();
        let mut checked = 0;
        for path in &paths {
            let at: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let in_pieces = root.at(&at).is_some_and(|n| {
                n.value.is_none() && n.children.len() > 1 && n.assembled().is_some()
            });
            for mutation in [
                Mutation::Leaf,
                Mutation::List,
                Mutation::Truncate,
                Mutation::Key,
                Mutation::ValueBeside,
                Mutation::Remove,
            ] {
                let Some(mutated) = mutate(&root, path, mutation) else {
                    continue;
                };
                checked += 1;
                let after = graph(&mutated);
                let at = format!("{mutation:?} at {path:02x?}");
                let clean = after.coverage().read_cleanly();
                if !clean {
                    if !after.targets_not_found().is_empty() {
                        failures.push(format!("{at}: not clean, yet targets not found"));
                    }
                    for kind in Kind::ALL {
                        if after.listed_whole(kind)
                            || after.coverage().nothing_known_unread_for(kind)
                        {
                            failures.push(format!("{at}: not clean, yet {kind:?} is sealed"));
                        }
                    }
                }
                if let Mutation::Remove = mutation {
                    if !clean {
                        continue;
                    }
                    // The node removed lies in the holder's member, or holds it.
                    let stored_under = |holder: ObjectId| {
                        holder_paths(holder)
                            .iter()
                            .any(|p| p.starts_with(path) || path.starts_with(p))
                    };
                    for u in after.uses() {
                        let known = before.object(u.holder).is_none()
                            || before
                                .uses()
                                .iter()
                                .any(|v| v.holder == u.holder && v.target == u.target);
                        if !known {
                            failures.push(format!("{at}: a new use {u:?}"));
                        }
                        if let Resolution::TargetNotFound { missing } = u.resolution {
                            let removed = member_paths(missing)
                                .iter()
                                .any(|member| member.starts_with(path));
                            if !removed {
                                failures.push(format!("{at}: target newly not found {u:?}"));
                            }
                        }
                    }
                    for v in before.uses() {
                        let kept = after
                            .uses()
                            .iter()
                            .any(|u| u.holder == v.holder && u.target == v.target);
                        if !kept && !stored_under(v.holder) {
                            failures.push(format!("{at}: a use lost {v:?}"));
                        }
                    }
                    continue;
                }
                let excused = by_spec.iter().any(|declared| path.starts_with(declared))
                    || (in_pieces && matches!(mutation, Mutation::Leaf));
                if clean && !excused {
                    failures.push(format!("{at}: an error the read did not notice"));
                } else if clean && signature(&after) != base {
                    failures.push(format!("{at}: read cleanly, but the result changed"));
                }
            }
        }
        assert!(checked > 500, "{checked} mutations");
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Instance 1: a member list stored as a value is not an empty
    /// catalog. The custom function a field's formula calls is then not
    /// known to be missing, and an object store that does not read keeps
    /// layout objects from being listed whole.
    #[test]
    fn a_malformed_member_list_is_not_an_empty_catalog() {
        let mut root = forward();
        put(&mut root, &[&[0x10], &[5]], vec![0x55]);
        let graph = graph(&root);
        assert!(
            graph.targets_not_found().is_empty(),
            "{:#?}",
            graph.targets_not_found()
        );
        assert!(!graph.listed_whole(Kind::CustomFunction));
        assert!(graph.unlisted_reason(Kind::CustomFunction).is_some());
        assert!(graph.uses().iter().any(|u| {
            u.target == Target::Object(ObjectId::CustomFunction(FUNCTION))
                && matches!(u.resolution, Resolution::Unresolved { .. })
        }));
        assert!(graph
            .coverage()
            .unread
            .iter()
            .any(|u| u.place.part == "stored 16/5"));
        let mut root = base();
        root.put(
            &[&[4], &[5], &uint_segment(LAYOUT), &[7]],
            OwnedNode::list(vec![7], vec![OwnedNode::leaf(vec![1], vec![1])]),
        );
        let graph = self::graph(&root);
        assert!(!graph.coverage().read_cleanly());
        for kind in Kind::ALL {
            assert!(!graph.listed_whole(kind), "{kind:?}");
            assert!(!graph.coverage().nothing_known_unread_for(kind), "{kind:?}");
        }
    }

    /// Instance 2: a step list missing a piece is not a script without
    /// steps: the layout only its steps go to is not unreferenced.
    #[test]
    fn a_step_list_missing_a_piece_is_not_an_empty_script() {
        let mut root = base();
        add_script(&mut root, 4, "Long", long_script());
        let whole = graph(&root);
        assert!(!whole.uses_of(ObjectId::Layout(LAYOUT)).is_empty());
        let list = [&[17][..], &[5], &serial::encode(4), &[4]];
        let mut stored = root.at(&list).unwrap().clone();
        assert!(stored.children.len() > 2, "stored in pieces");
        stored.children.remove(1);
        root.put(&list, stored);
        let graph = graph(&root);
        assert!(graph
            .coverage()
            .unread
            .iter()
            .any(|u| u.place.part == "stored 17/5/4/4"));
        let unused: Vec<ObjectId> = graph
            .no_reference_found(Kind::Layout)
            .into_iter()
            .map(|o| o.id)
            .collect();
        assert_eq!(unused, [ObjectId::Layout(LAYOUT)]);
        assert!(!graph.coverage().nothing_known_unread_for(Kind::Layout));
    }

    /// Instance 3: after a stop before the script catalog, and with a
    /// script member list that does not read, whether a script exists is
    /// not known.
    #[test]
    fn a_kind_not_listed_whole_says_why() {
        let root = base();
        let tree = KeyTree { root: view(&root) };
        let stopped = build_within(&tree, Vec::new(), MAX_ENTRIES, 5);
        assert!(!stopped.listed_whole(Kind::Script));
        assert!(stopped
            .unlisted_reason(Kind::Script)
            .is_some_and(|r| r.contains("stopped")));
        let full = build(&tree, Vec::new());
        assert!(full.listed_whole(Kind::Script));
        assert_eq!(full.unlisted_reason(Kind::Script), None);
    }

    /// Instance 4: steps sharing one data key are read once, and the same
    /// unread place repeated in each is kept a few times in full, the rest
    /// counted, with details of bounded length.
    #[test]
    fn repeated_diagnostics_are_counted_not_kept() {
        let mut root = base();
        let mut steps: Vec<ScriptStep> = (0..200).map(|_| set_field(A)).collect();
        for step in &mut steps {
            for slot in 0x30..0x60 {
                step.set_text(slot, Some(&"x".repeat(50)));
            }
        }
        let mut script = Script {
            id: 2,
            name: "Shared".into(),
            ..Script::default()
        };
        script.insert_steps(0, steps).unwrap();
        let shared = script.steps[0].data_key;
        for step in &mut script.steps {
            step.data_key = shared;
        }
        let node = script.encode();
        let segment = node.segment.clone();
        root.put(&[&[17], &[5], &segment], node);
        let graph = graph(&root);
        let kept = graph
            .coverage()
            .unread
            .iter()
            .filter(|u| u.place.part == "stored data")
            .count();
        assert_eq!(kept, 20);
        let counted: usize = graph
            .coverage()
            .omitted
            .iter()
            .filter(|o| o.place == "stored data")
            .map(|o| o.count)
            .sum();
        assert_eq!(counted, 180);
        assert!(graph
            .coverage()
            .unread
            .iter()
            .all(|u| u.detail.chars().count() <= 303));
        assert!(!graph.coverage().nothing_known_unread_for(Kind::Field));
    }

    /// Instance 5: Perform SQL Query by Natural Language with its options
    /// as JSON can name occurrences.
    #[test]
    fn sql_by_natural_language_with_json_options_names_occurrences() {
        for by_slot in [true, false] {
            let mut root = base();
            let mut step = ScriptStep::new(214);
            if by_slot {
                step.set_list_value(15, Some(1));
            } else {
                step.set_flag(0x800, true);
            }
            step.set_calculation(16, Some(&step_calculation("\"{}\"")));
            with_step(&mut root, step);
            let graph = graph(&root);
            assert!(graph
                .coverage()
                .by_name
                .iter()
                .any(|b| b.holder == OTHER && b.could_refer_to.contains(&Kind::Occurrence)));
            assert!(!graph.coverage().nothing_known_unread_for(Kind::Occurrence));
        }
    }

    /// Instance 6: a formula context whose key string does not read, and a
    /// value stored beside a step calculation's streams, are reported.
    #[test]
    fn a_bad_context_and_a_value_beside_a_calculation_are_reported() {
        let mut root = base();
        let b = uint_segment(B);
        put(
            &mut root,
            &[&uint_segment(TABLE), &[3], &[5], &b, &[5], &[4]],
            vec![0x05, 0x01],
        );
        let graph = graph(&root);
        assert!(graph.coverage().unread.iter().any(|u| {
            u.detail.contains("context whose key does not read")
                && u.could_refer_to == [Kind::Occurrence]
        }));
        assert!(!graph.coverage().nothing_known_unread_for(Kind::Occurrence));
        let mut root = base();
        let mut script = Script {
            id: 2,
            name: "Other".into(),
            ..Script::default()
        };
        script.insert_steps(0, vec![set_field(A)]).unwrap();
        let key = script.steps[0].data_key.segment().unwrap().to_vec();
        let mut node = script.encode();
        let calculation = node
            .child_mut(&[5])
            .and_then(|d| d.child_mut(&key))
            .and_then(|d| d.child_mut(&[0, 0]))
            .and_then(|slot| slot.child_mut(&[5]))
            .expect("slot 0's calculation");
        calculation.value = Some(vec![1]);
        let segment = node.segment.clone();
        root.put(&[&[17], &[5], &segment], node);
        let graph = self::graph(&root);
        assert!(graph
            .coverage()
            .unread
            .iter()
            .any(|u| { u.holder == OTHER && u.detail == "a value stored beside keys" }));
    }

    /// Instance 7: a key with no spec is accepted only in its exact
    /// observed form, and the field index's documented empty list at
    /// `01, 00` is accepted while an entry with another value is not.
    #[test]
    fn observed_forms_are_exact() {
        let l = uint_segment(LAYOUT);
        let mut root = base();
        put(&mut root, &[&[4], &[5], &l, &[0x17]], Vec::new());
        put(&mut root, &[&[4], &[5], &l, &[0x16]], vec![0, 0]);
        root.put(
            &[&[4], &[5], &l, &[1], &[0]],
            OwnedNode::list(vec![0], Vec::new()),
        );
        let o = uint_segment(OCCURRENCE);
        put(
            &mut root,
            &[
                &[4],
                &[5],
                &l,
                &[1],
                &o,
                &uint_segment(A),
                &serial::encode(OBJECT),
            ],
            Vec::new(),
        );
        let graph = graph(&root);
        for kind in Kind::ANY {
            assert!(
                graph.coverage().nothing_known_unread_for(kind),
                "{:#?}",
                graph.coverage()
            );
        }
        for (key, value) in [(0x17u8, vec![0u8]), (0x16, vec![0, 0, 0])] {
            let mut root = base();
            put(&mut root, &[&[4], &[5], &l, &[key]], value);
            let graph = self::graph(&root);
            assert!(
                !graph.coverage().nothing_known_unread_for(Kind::Field),
                "{key:02x}"
            );
        }
        let mut root = base();
        put(
            &mut root,
            &[
                &[4],
                &[5],
                &l,
                &[1],
                &o,
                &uint_segment(A),
                &serial::encode(OBJECT),
            ],
            vec![0x55],
        );
        assert!(!self::graph(&root)
            .coverage()
            .nothing_known_unread_for(Kind::Field));
    }

    /// Instance 8: Send Event can send an Apple event to FileMaker Pro
    /// itself, which names objects as an AppleScript can (inferred).
    #[test]
    fn send_event_names_what_an_applescript_can() {
        let mut root = base();
        with_step(&mut root, ScriptStep::new(57));
        let graph = graph(&root);
        assert!(graph.coverage().by_name.iter().any(|b| {
            b.how == NameUse::ExternalScriptText && b.could_refer_to.contains(&Kind::Script)
        }));
    }

    /// Round 5: a layout the layout index lists without its member is an
    /// anomaly, listed from its index entry, and the occurrence the entry
    /// names is still a use.
    #[test]
    fn a_layout_only_in_the_index_is_an_anomaly_and_its_occurrence_a_use() {
        let mut root = base();
        let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
        let l = uint_segment(9);
        text(&mut root, &[&[4], &[1], &[7], &l, &[0x10]], "Hidden");
        put(
            &mut root,
            &[&[4], &[1], &[7], &l, &[2]],
            encode::layout_entry(alias, 0),
        );
        let graph = graph(&root);
        assert!(!graph.coverage().read_cleanly());
        assert!(graph.object(ObjectId::Layout(9)).is_some());
        assert!(graph.uses().iter().any(|u| u.holder == ObjectId::Layout(9)
            && u.target == Target::Object(ObjectId::Occurrence(OCCURRENCE))));
    }

    /// Round 5: a field-index branch names its occurrence whether or not
    /// it has entries; that is a use, held by the layout.
    #[test]
    fn an_empty_field_index_branch_is_a_use_of_its_occurrence() {
        let mut root = base();
        root.put(
            &[
                &[4],
                &[5],
                &uint_segment(LAYOUT),
                &[1],
                &uint_segment(0xd0_0002),
            ],
            OwnedNode::list(uint_segment(0xd0_0002), Vec::new()),
        );
        let graph = graph(&root);
        assert!(
            graph.coverage().read_cleanly(),
            "{:#?}",
            graph.coverage().anomalies
        );
        assert!(graph
            .uses()
            .iter()
            .any(|u| u.holder == ObjectId::Layout(LAYOUT)
                && u.place.part == "field index"
                && u.target == Target::Object(ObjectId::Occurrence(0xd0_0002))));
    }

    /// Round 5: a table's catalog `[T]` stored as a value, above the field
    /// catalog no walk reaches, is an anomaly.
    #[test]
    fn a_failed_table_catalog_above_its_fields_is_reported() {
        let mut root = base();
        let other = uint_segment(0x8002);
        text(&mut root, &[&[3], &[0x10], &[5], &other, &[0x10]], "Other");
        put(&mut root, &[&other], vec![0x55]);
        let graph = graph(&root);
        assert!(!graph.coverage().read_cleanly());
        assert!(
            graph
                .coverage()
                .anomalies
                .iter()
                .any(|u| u.place.part == "stored 32770"),
            "{:#?}",
            graph.coverage().anomalies
        );
    }

    /// Round 5: a step's several keys (script-steps.md 12.1, 6 bytes each
    /// with their count) are read as uses; one that does not read, or whose
    /// count disagrees, is an anomaly, never an empty list.
    #[test]
    fn several_keys_in_a_slot_are_read_or_reported() {
        let keys = |scripts: &[u64]| -> Vec<u8> {
            scripts
                .iter()
                .flat_map(|s| {
                    let segment = serial::encode(*s);
                    let mut key = vec![segment.len() as u8];
                    key.extend(segment);
                    key.resize(6, 0);
                    key
                })
                .collect()
        };
        let step = |list: Vec<u8>, count: Option<u32>| {
            let mut step = ScriptStep::new(239);
            step.set_flag(0x100, true);
            step.set_slot_child(3, 0x11, Some(OwnedNode::leaf(vec![0x11], list)));
            if let Some(count) = count {
                step.set_slot_child(
                    3,
                    0x04,
                    Some(OwnedNode::leaf(vec![4], count.to_le_bytes().to_vec())),
                );
            }
            step
        };
        let mut root = base();
        add_script(&mut root, 3, "Third", Vec::new());
        with_step(&mut root, step(keys(&[SCRIPT, 3]), Some(2)));
        let graph = graph(&root);
        assert!(
            graph.coverage().read_cleanly(),
            "{:#?}",
            graph.coverage().anomalies
        );
        for script in [SCRIPT, 3] {
            assert!(
                graph
                    .uses()
                    .iter()
                    .any(|u| u.holder == OTHER
                        && u.target == Target::Object(ObjectId::Script(script))),
                "{:#?}",
                graph.uses()
            );
        }
        for (list, count) in [
            (keys(&[SCRIPT, 3]), Some(3)),
            (Vec::new(), None),
            (vec![9, 1, 0, 0, 0, 0], None),
        ] {
            let mut root = base();
            with_step(&mut root, step(list.clone(), count));
            let graph = self::graph(&root);
            assert!(!graph.coverage().read_cleanly(), "{list:02x?} {count:?}");
        }
    }

    /// Round 5: a source that saw a catalog change while it read says so;
    /// the read stays clean, and `consistent` is false.
    #[test]
    fn a_change_seen_during_the_read_makes_it_inconsistent() {
        let mut graph = graph(&base());
        assert!(graph.coverage().consistent());
        graph.note_changed_during_read(vec!["the layouts: its modification count changed".into()]);
        assert!(!graph.coverage().consistent());
        assert!(graph.coverage().read_cleanly());
    }

    /// The readers reach the catalogs only through the access layer: none
    /// names the key tree or its nodes, and only `build` takes the tree, to
    /// hand it to the layer.
    #[test]
    fn readers_reach_the_catalogs_only_through_the_access_layer() {
        // `name` as a word, followed by one of `then` (a type's generics or
        // path); `Item::Node(..)` of om_scripts is a step's own data.
        let word = |text: &str, name: &str, then: &[&str]| {
            text.match_indices(name).any(|(i, _)| {
                let before = text[..i].chars().next_back();
                let rest = &text[i + name.len()..];
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
                    && then.iter().any(|t| rest.starts_with(t))
            })
        };
        let tree_types = |text: &str| {
            word(text, "KeyTree", &["", "<"]) || word(text, "Node", &["<", "::", ",", ";", "}"])
        };
        for (file, text) in [
            ("catalogs.rs", include_str!("catalogs.rs")),
            ("layouts.rs", include_str!("layouts.rs")),
            ("steps.rs", include_str!("steps.rs")),
            ("formula.rs", include_str!("formula.rs")),
        ] {
            assert!(!tree_types(text), "{file} names the key tree or its nodes");
        }
        let read = include_str!("read.rs");
        assert!(
            !word(read, "Node", &["<", "::", ",", ";", "}"]),
            "read.rs names Node"
        );
        let takes_tree: Vec<&str> = read
            .lines()
            .filter(|l| l.contains("KeyTree<"))
            .map(str::trim)
            .collect();
        assert_eq!(
            takes_tree,
            [
                "pub fn build(tree: &KeyTree<'_>, unavailable: Vec<Unavailable>) -> DesignGraph {",
                "tree: &KeyTree<'_>,",
                "tree: &KeyTree<'_>,",
            ]
        );
    }
}
