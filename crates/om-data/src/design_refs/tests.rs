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
