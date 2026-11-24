//! Building the graph from a file's catalogs: each catalog read with the
//! decoder OpenMaker has for it, each reference resolved once every object
//! is known.
//!
//! What the builder reads is accounted for, not assumed: a catalog member
//! is read through a [`Member`], which records each child key the reader
//! takes or knows to name no object, and [`Builder::close`] reports every
//! other key unread. A value a decoder rejects, a variant the reader does
//! not handle and a value it finds absent where one is expected are
//! reported unread where they are met.

use std::collections::BTreeMap;

use om_formula::{NameTable, Names};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, KeyTree, Node};
use om_scripts::display::StepNames;

use super::formula::{self, FormulaRef};
use super::{catalogs, layouts, steps};
use super::{
    ByName, Coverage, DesignGraph, Kind, NameUse, Object, ObjectId, Place, Target, Unavailable,
    Unread, UnreadReason, Use, MAX_ENTRIES, MAX_WORK,
};

/// The catalogs [`build`] reads, by their paths in the file's key tree, and
/// besides them each table's field catalog `[T, 3]`. A source gives these;
/// one it cannot give is passed to `build` as [`Unavailable`]. Catalogs
/// outside the list (themes, fonts, the image library, accounts) hold no
/// reference to a design object the graph has.
pub const DESIGN_CATALOGS: [(&[u64], &str); 10] = [
    (&[2], "the file's options"),
    (&[3], "tables, table occurrences and relationships"),
    (&[4], "layouts"),
    (&[16], "custom functions"),
    (&[17], "scripts"),
    (&[23, 2], "privilege sets"),
    (&[32], "data sources"),
    (&[33], "value lists"),
    (&[64], "catalog 64, whose meaning is not known"),
    (&[65], "custom menus"),
];

/// Keys every catalog member may carry that name no object: its owner,
/// UUIDs and tags (`d7`-`da`, script-steps.md 4.3) and its modification
/// stamps (`fc`, `fc01`-`fc03`, schema-path.md 8.1).
pub(super) const MEMBER_STAMPS: [u64; 8] = [0xd7, 0xd8, 0xd9, 0xda, 0xfc, 0xfc01, 0xfc02, 0xfc03];
/// A catalog member's name (file-format.md 3.4).
pub(super) const NAME: u64 = 0x10;

/// The data source id of a key read big-endian, as occurrence definitions
/// and value lists store it: its segment's serial decode, the id `inspect`
/// gives data sources.
pub(super) fn data_source_id(key: u64) -> Option<u64> {
    serial::decode(&uint_segment(key)).filter(|id| *id != 0)
}

/// Build the reference graph of the file whose catalogs `tree` holds
/// ([`DESIGN_CATALOGS`]). `unavailable` lists the catalogs the source
/// could not give: references into them are left unresolved, and every
/// "nothing refers to it" becomes "nothing decoded refers to it".
pub fn build(tree: &KeyTree<'_>, unavailable: Vec<Unavailable>) -> DesignGraph {
    build_within(tree, unavailable, MAX_ENTRIES, MAX_WORK)
}

/// [`build`] with other budgets, for tests.
pub(super) fn build_within(
    tree: &KeyTree<'_>,
    unavailable: Vec<Unavailable>,
    max_entries: usize,
    max_work: usize,
) -> DesignGraph {
    let mut b = Builder {
        graph: DesignGraph {
            coverage: Coverage {
                unavailable,
                ..Coverage::default()
            },
            ..DesignGraph::default()
        },
        pending: Vec::new(),
        entries: 0,
        work: 0,
        max_entries,
        max_work,
        stopped: false,
    };
    catalogs::data_sources(&mut b, tree);
    catalogs::tables(&mut b, tree);
    catalogs::occurrences(&mut b, tree);
    catalogs::relationships(&mut b, tree);
    catalogs::custom_functions(&mut b, tree);
    catalogs::value_lists(&mut b, tree);
    layouts::layouts(&mut b, tree);
    steps::scripts(&mut b, tree);
    catalogs::file_options(&mut b, tree);
    catalogs::custom_menus(&mut b, tree);
    catalogs::privilege_sets(&mut b, tree);
    catalogs::unknown_catalog(&mut b, tree);
    let Builder {
        mut graph, pending, ..
    } = b;
    let uses: Vec<Use> = pending
        .into_iter()
        .map(|(holder, place, target)| Use {
            holder,
            resolution: graph.resolve(target),
            place,
            target,
        })
        .collect();
    graph.uses = uses;
    graph
}

/// What reading one stored value found, to be recorded at each place that
/// holds it: a value many steps share is read once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Found {
    Reference(Target),
    /// A formula's context occurrence: recorded under the place's part
    /// with ` context` after it.
    Context(u64),
    ByName {
        how: NameUse,
        function: Option<String>,
        kinds: Vec<Kind>,
    },
    Unread {
        reason: UnreadReason,
        detail: String,
        kinds: Vec<Kind>,
    },
}

impl Found {
    pub(super) fn unread(reason: UnreadReason, detail: impl Into<String>, kinds: &[Kind]) -> Found {
        Found::Unread {
            reason,
            detail: detail.into(),
            kinds: kinds.to_vec(),
        }
    }
}

/// A Polish stream's references (calc-encoding.md 1, 2.5), with its
/// context occurrence, and the nodes walked.
pub(super) fn polish_found(polish: &[u8], context: Option<&[u8]>) -> (Vec<Found>, usize) {
    let expr = match om_formula::polish::decode(polish) {
        Ok(expr) => expr,
        Err(e) => {
            return (
                vec![Found::unread(
                    UnreadReason::Malformed,
                    format!("a formula that does not decode: {e}"),
                    &Kind::ANY,
                )],
                1,
            )
        }
    };
    let mut found = Vec::new();
    if let Some(occurrence) = context
        .map(|key| om_formula::Key(key.to_vec()))
        .and_then(|key| key.id())
        .filter(|id| *id != 0)
    {
        found.push(Found::Context(occurrence));
    }
    let (references, nodes) = formula::references(&expr);
    found.extend(references.into_iter().map(|r| match r {
        FormulaRef::Field { occurrence, field } => {
            Found::Reference(Target::Field { occurrence, field })
        }
        FormulaRef::CustomFunction(id) => {
            Found::Reference(Target::Object(ObjectId::CustomFunction(id)))
        }
        FormulaRef::ByName {
            how,
            function,
            could_refer_to,
        } => Found::ByName {
            how,
            function,
            kinds: could_refer_to,
        },
        FormulaRef::Unreadable(what) => Found::unread(
            UnreadReason::Malformed,
            what,
            &[Kind::Field, Kind::Occurrence, Kind::CustomFunction],
        ),
    }));
    (found, nodes)
}

/// A stored formula node's references (children `04` context, `06`
/// Polish), and the nodes walked. One without a Polish stream is unread:
/// its formatted stream (`05`) is the source text, not what runs.
pub(super) fn formula_found(node: &Node<'_>) -> (Vec<Found>, usize) {
    match node.get(6).and_then(Node::assembled) {
        Some(polish) => polish_found(&polish, node.get(4).and_then(Node::assembled).as_deref()),
        None => (
            vec![Found::unread(
                UnreadReason::NotDecoded,
                "a formula stored without its compiled form",
                &Kind::ANY,
            )],
            1,
        ),
    }
}

/// One stored member whose child keys the reader accounts for: those it
/// [`Member::get`]s and those it marks [`Member::known`]. The rest are
/// reported unread when it is [`Builder::close`]d.
pub(super) struct Member<'n, 'a> {
    holder: ObjectId,
    node: &'n Node<'a>,
    what: String,
    read: Vec<u64>,
}

impl<'n, 'a> Member<'n, 'a> {
    /// The child `key`, accounted for whether present or not.
    pub(super) fn get(&mut self, key: u64) -> Option<&'n Node<'a>> {
        self.read.push(key);
        self.node.get(key)
    }

    /// Mark keys that name no design object, as the spec cited at each
    /// call says.
    pub(super) fn known(&mut self, keys: &[u64]) {
        self.read.extend_from_slice(keys);
    }
}

/// The graph as it is being read.
pub(super) struct Builder {
    pub(super) graph: DesignGraph,
    pending: Vec<(ObjectId, Place, Target)>,
    /// References, unread places and names recorded, against
    /// [`MAX_ENTRIES`].
    entries: usize,
    /// Work done, against [`MAX_WORK`].
    work: usize,
    max_entries: usize,
    max_work: usize,
    /// A budget was spent: nothing more is read or recorded.
    stopped: bool,
}

impl Builder {
    pub(super) fn object(
        &mut self,
        id: ObjectId,
        name: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.graph.objects.insert(
            id,
            Object {
                id,
                name: name.into(),
                detail: detail.into(),
            },
        );
    }

    pub(super) fn count(&mut self, what: &'static str) {
        self.graph.coverage.count(what, 1);
    }

    pub(super) fn unavailable(&mut self, path: Vec<u64>, what: &str, reason: String) {
        self.graph.coverage.unavailable.push(Unavailable {
            path,
            what: what.into(),
            reason,
        });
    }

    /// Whether a budget was spent, so that readers stop.
    pub(super) fn stopped(&self) -> bool {
        self.stopped
    }

    /// Spend `units` of work at `place` in `holder`. False once the work
    /// budget is spent: the reader stops there.
    pub(super) fn spend(&mut self, units: usize, holder: ObjectId, place: &Place) -> bool {
        if self.stopped {
            return false;
        }
        self.work = self.work.saturating_add(units);
        if self.work > self.max_work {
            let budget = format!("work budget ({} units)", self.max_work);
            self.stop(holder, place, budget);
            return false;
        }
        true
    }

    /// Room for one more entry at `place`; recording stops at
    /// [`MAX_ENTRIES`].
    fn admit(&mut self, holder: ObjectId, place: &Place) -> bool {
        if self.stopped {
            return false;
        }
        if self.entries >= self.max_entries {
            let budget = format!("entry budget ({})", self.max_entries);
            self.stop(holder, place, budget);
            return false;
        }
        self.entries += 1;
        true
    }

    /// Stop the build at `place`: one unread entry says where, and that
    /// nothing after it was read.
    fn stop(&mut self, holder: ObjectId, place: &Place, budget: String) {
        self.stopped = true;
        self.graph.coverage.unread.push(Unread {
            holder,
            place: place.clone(),
            reason: UnreadReason::Limit,
            detail: format!(
                "the analysis's {budget} was spent here: this place and every place after it \
                 are not read"
            ),
            could_refer_to: Kind::ANY.to_vec(),
        });
    }

    pub(super) fn reference(&mut self, holder: ObjectId, place: Place, target: Target) {
        if self.admit(holder, &place) {
            self.pending.push((holder, place, target));
        }
    }

    pub(super) fn by_name(
        &mut self,
        holder: ObjectId,
        place: Place,
        how: NameUse,
        function: Option<String>,
        could_refer_to: Vec<Kind>,
    ) {
        if self.admit(holder, &place) {
            self.graph.coverage.by_name.push(ByName {
                holder,
                place,
                how,
                function,
                could_refer_to,
            });
        }
    }

    pub(super) fn unread(
        &mut self,
        holder: ObjectId,
        place: Place,
        reason: UnreadReason,
        detail: impl Into<String>,
        could_refer_to: &[Kind],
    ) {
        if self.admit(holder, &place) {
            self.graph.coverage.unread.push(Unread {
                holder,
                place,
                reason,
                detail: detail.into(),
                could_refer_to: could_refer_to.to_vec(),
            });
        }
    }

    /// Record what reading one stored value found, at `place`.
    pub(super) fn apply(&mut self, holder: ObjectId, place: &Place, found: &[Found]) {
        for found in found {
            match found {
                Found::Reference(target) => self.reference(holder, place.clone(), *target),
                Found::Context(occurrence) => self.reference(
                    holder,
                    Place {
                        step: place.step.clone(),
                        part: format!("{} context", place.part),
                    },
                    Target::Object(ObjectId::Occurrence(*occurrence)),
                ),
                Found::ByName {
                    how,
                    function,
                    kinds,
                } => self.by_name(holder, place.clone(), *how, function.clone(), kinds.clone()),
                Found::Unread {
                    reason,
                    detail,
                    kinds,
                } => self.unread(holder, place.clone(), *reason, detail.clone(), kinds),
            }
        }
    }

    /// The formula stored at `node`, at `place`.
    pub(super) fn formula(&mut self, holder: ObjectId, place: Place, node: &Node<'_>) {
        let (found, nodes) = formula_found(node);
        if self.spend(nodes, holder, &place) {
            self.count("formulas");
            self.apply(holder, &place, &found);
        }
    }

    /// Start accounting for the child keys of `node`, a member of
    /// `holder` described as `what`.
    pub(super) fn member<'n, 'a>(
        &mut self,
        holder: ObjectId,
        node: &'n Node<'a>,
        what: impl Into<String>,
    ) -> Member<'n, 'a> {
        Member {
            holder,
            node,
            what: what.into(),
            read: Vec::new(),
        }
    }

    /// Report each child key of `member` the reader did not account for,
    /// and a value on the member itself, as unread: what it holds is not
    /// known, so it may refer to anything.
    pub(super) fn close(&mut self, member: Member<'_, '_>) {
        let Member {
            holder,
            node,
            what,
            read,
        } = member;
        if node.value.is_some_and(|v| !v.is_empty()) {
            self.unread(
                holder,
                Place::new(format!("{what}: its own value")),
                UnreadReason::NotDecoded,
                format!("a value on the {what} itself, which OpenMaker does not read"),
                &Kind::ANY,
            );
        }
        for child in &node.children {
            if !self.spend(1, holder, &Place::new(what.clone())) {
                return;
            }
            if child.id().is_some_and(|id| read.contains(&id)) {
                continue;
            }
            self.unread(
                holder,
                Place::new(format!("{what}: key {}", child.segment)),
                UnreadReason::NotDecoded,
                format!("a stored key of the {what} that OpenMaker does not read"),
                &Kind::ANY,
            );
        }
    }
}

/// The names a step's text shows, from the objects read: what numbering
/// a step's lines as `get_script` does needs.
pub(super) struct GraphNames {
    calc: NameTable,
    layouts: BTreeMap<u64, String>,
    scripts: BTreeMap<u64, String>,
    tables: BTreeMap<u64, String>,
    data_sources: BTreeMap<u64, String>,
}

impl GraphNames {
    pub(super) fn new(graph: &DesignGraph) -> GraphNames {
        let mut names = GraphNames {
            calc: NameTable::default(),
            layouts: BTreeMap::new(),
            scripts: BTreeMap::new(),
            tables: BTreeMap::new(),
            data_sources: BTreeMap::new(),
        };
        for object in graph.objects() {
            let name = object.name.clone();
            match object.id {
                ObjectId::Table(id) => {
                    names.tables.insert(id, name);
                }
                ObjectId::Field { table, field } => names.calc.add_field(table, field, &name),
                ObjectId::Occurrence(id) => {
                    if let Some(table) = graph.occurrence_table(id) {
                        names.calc.add_occurrence(id, &name, table);
                    }
                }
                ObjectId::Layout(id) => {
                    names.layouts.insert(id, name);
                }
                ObjectId::Script(id) => {
                    names.scripts.insert(id, name);
                }
                ObjectId::CustomFunction(id) => names.calc.add_custom_function(id, &name, 0),
                ObjectId::DataSource(id) => {
                    names.data_sources.insert(id, name);
                }
                _ => {}
            }
        }
        names
    }
}

impl StepNames for GraphNames {
    fn calc_names(&self) -> &dyn Names {
        &self.calc
    }
    fn layout_name(&self, id: u64) -> Option<&str> {
        self.layouts.get(&id).map(String::as_str)
    }
    fn script_name(&self, id: u64) -> Option<&str> {
        self.scripts.get(&id).map(String::as_str)
    }
    fn table_name(&self, id: u64) -> Option<&str> {
        self.tables.get(&id).map(String::as_str)
    }
    fn data_source_name(&self, id: u64) -> Option<&str> {
        self.data_sources.get(&id).map(String::as_str)
    }
}

/// The members of the catalog at `path` (`[.., 5]`), but the `0` entry,
/// each with its id. A member whose key does not read as an id is passed
/// with `None`, for the caller to report.
pub(super) fn members<'t, 'a>(
    tree: &'t KeyTree<'a>,
    path: &[u64],
) -> impl Iterator<Item = (Option<u64>, &'t Node<'a>)> {
    tree.at(path)
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|n| n.id() != Some(0))
        .map(|n| (n.id(), n))
}

pub(super) fn text_at(node: Option<&Node<'_>>) -> Option<String> {
    node.and_then(Node::text).and_then(Result::ok)
}
