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
        .unused(Kind::Script)
        .into_iter()
        .map(|(o, _)| o.id)
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

    use super::super::read::build_within;
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

    /// `mutate` the base file: the kinds `kinds` lose complete coverage
    /// and nothing of them is unreferenced.
    fn check(case: &str, mutate: impl FnOnce(&mut OwnedNode), kinds: &[Kind]) {
        let before = graph(&base());
        for kind in kinds {
            assert!(
                before.coverage().complete_for(*kind),
                "{case}: the base reads every place"
            );
        }
        let mut root = base();
        mutate(&mut root);
        let after = graph(&root);
        for kind in kinds {
            assert!(
                !after.coverage().complete_for(*kind),
                "{case}: {kind:?} coverage is complete\n{:#?}",
                after.coverage()
            );
            assert!(
                after
                    .unused(*kind)
                    .iter()
                    .all(|(_, finding)| *finding == Finding::NoDecodedReference),
                "{case}: a {kind:?} is unreferenced"
            );
        }
    }

    fn with_step(root: &mut OwnedNode, step: ScriptStep) {
        add_script(root, 2, "Other", vec![step]);
    }

    #[test]
    fn the_base_file_is_read_completely() {
        let graph = graph(&base());
        for kind in Kind::ANY {
            assert!(
                graph.coverage().complete_for(kind),
                "{kind:?}: {:#?}",
                graph.coverage()
            );
        }
        assert!(graph.broken().is_empty(), "{:#?}", graph.broken());
        let unused: Vec<(ObjectId, Finding)> = graph
            .unused(Kind::Script)
            .into_iter()
            .map(|(o, f)| (o.id, f))
            .collect();
        assert_eq!(unused, [(ObjectId::Script(SCRIPT), Finding::Unreferenced)]);
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
        check(
            "an unknown key of a field definition",
            |root| put(root, &[&t, &[3], &[5], &uint_segment(A), &[0x77]], vec![1]),
            any,
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
        );
        check(
            "a formula stored without its compiled stream",
            |root| {
                root.remove(&[&t, &[3], &[5], &uint_segment(B), &[5], &[6]]);
            },
            any,
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
        );
        check(
            "a step without a parameter schema",
            |root| with_step(root, ScriptStep::new(999)),
            any,
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
        );
        check(
            "Open URL, which may perform a script by name",
            |root| with_step(root, ScriptStep::new(111)),
            &[Kind::Script, Kind::Layout, Kind::Field],
        );
        check(
            "an unknown key of a layout",
            |root| put(root, &[&[4], &[5], &l, &[0x77]], vec![1]),
            any,
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
        );
        check(
            "a relationship that does not read",
            |root| put(root, &[&[3], &[0xfb], &[5], &[1], &[2]], vec![0xff]),
            &[Kind::Occurrence, Kind::Field],
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
        );
        check(
            "privilege sets not given",
            |root| {
                root.remove(&[&[23]]);
            },
            any,
        );
        check(
            "a custom menu",
            |root| text(root, &[&[0x41], &[5], &[1], &[0x10]], "Menu"),
            any,
        );
        check(
            "catalog 64 holding more than a UUID",
            |root| put(root, &[&[0x40], &[5], &[1], &[0x77]], vec![1]),
            any,
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

    /// A build that meets its budget stops with an entry saying where, and
    /// no kind is complete.
    #[test]
    fn a_build_stops_at_its_budgets_and_says_where() {
        let root = base();
        for (entries, work) in [(2, MAX_WORK), (MAX_ENTRIES, 5)] {
            let graph = build_within(&KeyTree { root: view(&root) }, Vec::new(), entries, work);
            let limits: Vec<&Unread> = graph
                .coverage()
                .unread
                .iter()
                .filter(|u| u.reason == UnreadReason::Limit)
                .collect();
            assert_eq!(limits.len(), 1, "{:#?}", graph.coverage());
            assert!(graph.uses().len() <= entries);
            for kind in Kind::REFERABLE {
                assert!(!graph.coverage().complete_for(kind));
            }
        }
    }
}
