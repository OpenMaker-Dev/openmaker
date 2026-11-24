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
            (field_a, "field definition: key 119"),
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
            (field_b, "calculation formula: key 126"),
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
            (ObjectId::Script(SCRIPT), "script: key 119"),
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
            (ObjectId::Script(SCRIPT), "script: key 19"),
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
            (ObjectId::Script(SCRIPT), "step data"),
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
            (layout, "layout: key 119"),
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
            (object, "key 119"),
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
            (layout, "object store"),
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
            (layout, "table view: object store"),
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
            (layout, "part 1 definition"),
        );
        check(
            "a field index entry naming an object the layout does not have",
            |root| put(root, &[&[4], &[5], &l, &[1], &[9], &[9], &[9]], Vec::new()),
            &[Kind::Field, Kind::Occurrence],
            (layout, "field index"),
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
            (ObjectId::File, "catalog 64 member"),
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
        let broken: Vec<_> = whole.broken().iter().map(|u| u.holder).collect();
        assert_eq!(broken, [OTHER], "{:#?}", whole.uses());
        text(
            &mut root,
            &[&[4], &[5], &[1, 2, 3, 4, 5, 6, 7, 8, 9], &[0x10]],
            "Lost",
        );
        let lost = graph(&root);
        assert!(lost.broken().is_empty(), "{:#?}", lost.broken());
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
        assert!(full.broken().is_empty(), "{:#?}", full.broken());
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
                    graph.broken().is_empty(),
                    "budget {entries} {work}: {:#?}",
                    graph.broken()
                );
                for kind in Kind::ALL {
                    assert!(!graph.coverage().complete_for(kind), "{kind:?}");
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
        root
    }

    /// Paths of every list node under the design catalogs, but a table's
    /// own catalog, which holds records as well as its fields `[T, 3]`.
    fn lists(node: &OwnedNode, path: &mut Vec<Vec<u8>>, out: &mut Vec<Vec<Vec<u8>>>) {
        for child in &node.children {
            if child.children.is_empty() {
                continue;
            }
            path.push(child.segment.clone());
            let table_catalog = path.len() == 1 && child.segment.len() > 1;
            if !table_catalog {
                out.push(path.clone());
            }
            lists(child, path, out);
            path.pop();
        }
    }

    /// Every list node of a file holding each structure the builder reads,
    /// given one key no reader knows, leaves an unread entry: the build
    /// cannot drop a stored key by omission. The exceptions say why.
    #[test]
    fn an_unknown_key_anywhere_is_reported() {
        let root = rich();
        let before = graph(&root);
        for kind in Kind::ANY {
            assert!(
                before.coverage().complete_for(kind),
                "{kind:?}: {:#?}",
                before.coverage()
            );
        }
        assert!(before.broken().is_empty(), "{:#?}", before.broken());
        let mut paths = Vec::new();
        lists(&root, &mut Vec::new(), &mut paths);
        // Places whose children are not read as keys, and why.
        let exempt = |path: &[Vec<u8>]| -> Option<&str> {
            let at = |i: usize| path.get(i).map(Vec::as_slice);
            match (path.len(), at(0), at(1)) {
                // Another file's name and paths: never an object of this
                // one, so only the name is read.
                (2 | 3, Some([0x20]), Some([5])) => Some("data source"),
                _ => None,
            }
        };
        let mut silent = Vec::new();
        let mut checked = 0;
        for path in &paths {
            if exempt(path).is_some() {
                continue;
            }
            let at: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            // One unknown key, then a value on the list itself.
            let mut keyed = root.clone();
            let mut key = at.clone();
            key.push(&[0x7e]);
            put(&mut keyed, &key, vec![1]);
            let mut valued = root.clone();
            let mut node = valued.at(&at).cloned().unwrap();
            node.value = Some(vec![1]);
            valued.put(&at, node);
            for (what, mutated) in [("key", keyed), ("value", valued)] {
                let after = graph(&mutated);
                checked += 1;
                if after.coverage().unread.len() <= before.coverage().unread.len() {
                    silent.push(format!("{what} at {path:02x?}"));
                }
            }
        }
        assert!(checked > 30, "{checked} places");
        assert!(
            silent.is_empty(),
            "unknown keys not reported at {silent:#?}"
        );
    }
}
