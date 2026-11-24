//! Building the graph from a file's catalogs: each catalog read with the
//! decoder OpenMaker has for it, each reference resolved once every object
//! is known.
//!
//! The catalogs are reached only through the access layer
//! ([`super::access`]), which marks every stored node a reader takes,
//! looks into or declares; [`Layer::sweep`] reports the rest at the end.
//! A value a decoder rejects and a variant a reader does not handle are
//! reported where they are met.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use om_formula::{NameTable, Names};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, KeyTree};
use om_scripts::display::StepNames;

use super::access::{At, Declared, Layer, Read};
use super::formula::{self, FormulaRef};
use super::{catalogs, layouts, steps};
use super::{
    ByName, Coverage, DesignGraph, Kind, NameUse, Object, ObjectId, Omitted, OmittedEntry, Place,
    Target, Unavailable, Unread, UnreadReason, Use, MAX_ENTRIES, MAX_WORK,
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
        lost: BTreeSet::new(),
        groups: HashMap::new(),
    };
    let layer = Layer::new(tree);
    catalogs::data_sources(&mut b, &layer);
    catalogs::tables(&mut b, &layer);
    catalogs::occurrences(&mut b, &layer);
    catalogs::relationships(&mut b, &layer);
    catalogs::custom_functions(&mut b, &layer);
    catalogs::value_lists(&mut b, &layer);
    layouts::layouts(&mut b, &layer);
    steps::scripts(&mut b, &layer);
    catalogs::file_options(&mut b, &layer);
    catalogs::custom_menus(&mut b, &layer);
    catalogs::privilege_sets(&mut b, &layer);
    catalogs::unknown_catalog(&mut b, &layer);
    // What no reader accounted for, by its path. After a stop every kind
    // is incomplete already, and the rest was not read by design.
    if !b.stopped {
        layer.sweep(&mut b);
    }
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
    // A context is a key string naming the occurrence; one that does not
    // read is unread, not taken for "no context".
    if let Some(key) = context.filter(|key| !key.is_empty()) {
        match om_formula::Key(key.to_vec()).id() {
            Some(0) => {}
            Some(occurrence) => found.push(Found::Context(occurrence)),
            None => found.push(Found::unread(
                UnreadReason::Malformed,
                "a formula context whose key does not read",
                &[Kind::Occurrence],
            )),
        }
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

/// A stored formula node's references, read through the layer: its
/// context `04` and compiled stream `06`; its formatted stream `05` is the
/// text it shows. One without a compiled stream is unread (the text is not
/// what runs); a context or stream that does not read is unread, never
/// taken for an absent one. Any other key is left to the sweep.
pub(super) fn formula_found(node: At<'_, '_>) -> Vec<Found> {
    let mut found = Vec::new();
    if let Some(text) = node.get(5) {
        text.declare(Declared::FormattedFormula);
    }
    let context = match node.bytes_at(4) {
        Read::Ok(context) => Some(context),
        Read::Unreadable => {
            found.push(Found::unread(
                UnreadReason::Malformed,
                "a formula's context that does not read",
                &[Kind::Occurrence],
            ));
            None
        }
        Read::Absent => None,
    };
    match node.bytes_at(6) {
        Read::Ok(polish) => found.extend(polish_found(&polish, context.as_deref()).0),
        Read::Unreadable => found.push(Found::unread(
            UnreadReason::Malformed,
            "a formula whose compiled stream does not read",
            &Kind::ANY,
        )),
        Read::Absent => found.push(Found::unread(
            UnreadReason::NotDecoded,
            "a formula stored without its compiled form",
            &Kind::ANY,
        )),
    }
    found
}

/// The bytes and nodes an owned subtree (a step's data) holds: what
/// reading it costs. Counted without recursion.
pub(super) fn owned_size(node: &om_hbam::OwnedNode) -> usize {
    let mut total = 0usize;
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        total = total.saturating_add(1 + node.value.as_ref().map_or(0, Vec::len));
        stack.extend(node.children.iter());
    }
    total
}

/// Entries of one kind of place kept in full; past it they are counted.
const KEPT_PER_PLACE: usize = 20;
/// The most of a detail kept.
const DETAIL_CHARS: usize = 300;

/// A place's shape for counting repeated entries: its text with digits
/// folded, so the same kind of place in each object counts together.
fn shape(part: &str) -> String {
    part.chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

fn bounded(detail: String) -> String {
    if detail.chars().count() <= DETAIL_CHARS {
        return detail;
    }
    let mut out: String = detail.chars().take(DETAIL_CHARS).collect();
    out.push_str("...");
    out
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
    /// Kinds with a member that could not be listed: never read to the end.
    lost: BTreeSet<Kind>,
    /// Entries recorded per kind of place, to keep the first few in full,
    /// and where the rest are counted in [`Coverage::omitted`].
    groups: HashMap<String, (usize, Option<usize>)>,
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

    /// Whether a budget was spent, so that readers stop.
    pub(super) fn stopped(&self) -> bool {
        self.stopped
    }

    /// Mark `kinds` as read to the end, unless the build stopped or a
    /// member of theirs could not be listed: only then can a reference to
    /// an object of theirs be broken, or an object of theirs be known not
    /// to exist.
    pub(super) fn inventoried(&mut self, kinds: &[Kind]) {
        if !self.stopped {
            let whole = kinds.iter().filter(|k| !self.lost.contains(k));
            self.graph.inventoried.extend(whole.copied());
        }
    }

    /// A member of `kind` that could not be listed (its key, its record or
    /// its catalog's member list does not read): the kind is never listed
    /// whole.
    pub(super) fn lost(&mut self, kind: Kind) {
        self.lost.insert(kind);
    }

    /// The members of a catalog's member list `[.., 5]` with their ids,
    /// for a catalog of `kind`. A member list that does not read, and a
    /// member whose key does not read, keep `kind` from being listed whole
    /// (the sweep reports them); the `00` entry and emptied (deleted)
    /// members are not members.
    pub(super) fn members<'t, 'a>(
        &mut self,
        list: Option<At<'t, 'a>>,
        kind: Kind,
    ) -> Vec<(u64, At<'t, 'a>)> {
        let Some(list) = list else {
            return Vec::new();
        };
        let children = match list.children() {
            Read::Ok(children) => children,
            Read::Absent => Vec::new(),
            Read::Unreadable => {
                self.lost(kind);
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for member in children {
            if member.is_empty() {
                member.declare(Declared::Deleted);
                continue;
            }
            match member.id() {
                Some(0) => {}
                Some(id) => out.push((id, member)),
                None => self.lost(kind),
            }
        }
        out
    }

    /// Charge what decoding `node` costs, before it is decoded: its stored
    /// bytes and nodes, counted no further than the budget left. False once
    /// the work budget is spent: the reader stops there.
    pub(super) fn charge(&mut self, node: At<'_, '_>, holder: ObjectId, place: &Place) -> bool {
        if self.stopped {
            return false;
        }
        let left = self.max_work.saturating_sub(self.work);
        let cost = node.size_within(left).unwrap_or(left.saturating_add(1));
        self.spend(cost, holder, place)
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

    /// Stop the build at `place`: [`Coverage::stopped`] says where, and
    /// that nothing after it was read; every kind is incomplete.
    fn stop(&mut self, holder: ObjectId, place: &Place, budget: String) {
        self.stopped = true;
        self.graph.coverage.stopped = Some(Unread {
            holder,
            place: place.clone(),
            reason: UnreadReason::Limit,
            detail: format!(
                "the analysis's {budget} was spent here: this place and every place after it \
                 are not read"
            ),
            could_refer_to: Kind::ALL.to_vec(),
        });
    }

    /// Whether an entry of this kind of place is kept in full, or counted
    /// with the others like it ([`Coverage::omitted`]).
    fn keep(&mut self, group: String, omitted: impl FnOnce() -> Omitted) -> bool {
        let (seen, at) = self.groups.entry(group).or_default();
        *seen += 1;
        if *seen <= KEPT_PER_PLACE {
            return true;
        }
        let list = &mut self.graph.coverage.omitted;
        match at {
            Some(i) => list[*i].count += 1,
            None => {
                *at = Some(list.len());
                list.push(omitted());
            }
        }
        false
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
        if !self.admit(holder, &place) {
            return;
        }
        let group = format!(
            "by_name|{:?}|{}|{:?}|{:?}",
            holder.kind(),
            shape(&place.part),
            how,
            could_refer_to
        );
        let omitted = || Omitted {
            entry: OmittedEntry::ByName(how),
            holder: holder.kind(),
            place: shape(&place.part),
            could_refer_to: could_refer_to.clone(),
            count: 1,
        };
        if self.keep(group, omitted) {
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
        if !self.admit(holder, &place) {
            return;
        }
        let group = format!(
            "unread|{:?}|{}|{:?}|{:?}",
            holder.kind(),
            shape(&place.part),
            reason,
            could_refer_to
        );
        let omitted = || Omitted {
            entry: OmittedEntry::Unread(reason),
            holder: holder.kind(),
            place: shape(&place.part),
            could_refer_to: could_refer_to.to_vec(),
            count: 1,
        };
        if self.keep(group, omitted) {
            self.graph.coverage.unread.push(Unread {
                holder,
                place,
                reason,
                detail: bounded(detail.into()),
                could_refer_to: could_refer_to.to_vec(),
            });
        }
    }

    /// Report the stored `node` unread at `place`, as its reader sees it,
    /// and account for it: the sweep does not report it again.
    pub(super) fn unread_node(
        &mut self,
        node: At<'_, '_>,
        holder: ObjectId,
        place: Place,
        reason: UnreadReason,
        detail: impl Into<String>,
        could_refer_to: &[Kind],
    ) {
        node.declare(Declared::Reported);
        self.unread(holder, place, reason, detail, could_refer_to);
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

    /// The formula stored at `node`, at `place`: charged by its size
    /// before it is decoded ([`formula_found`]).
    pub(super) fn formula(&mut self, holder: ObjectId, place: Place, node: At<'_, '_>) {
        if !self.charge(node, holder, &place) {
            return;
        }
        let found = formula_found(node);
        self.count("formulas");
        self.apply(holder, &place, &found);
    }

    /// Account for a key whose meaning is not specified when it is in the
    /// one form every observation shows (`expected`, exactly); otherwise it
    /// is failed and the sweep reports it.
    pub(super) fn observed(&mut self, node: Option<At<'_, '_>>, expected: &[u8]) {
        if let Some(node) = node {
            node.observed(expected);
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

/// A member's name `10` as text; one that does not read is failed on the
/// node (and reported by the sweep), and the object is listed unnamed.
pub(super) fn name_of(member: At<'_, '_>) -> String {
    match member.text_at(NAME) {
        Read::Ok(name) => name,
        Read::Absent | Read::Unreadable => String::new(),
    }
}

/// Account for a member's stamps ([`MEMBER_STAMPS`]).
pub(super) fn stamps(member: At<'_, '_>) {
    for key in MEMBER_STAMPS {
        if let Some(stamp) = member.get(key) {
            stamp.declare(Declared::Stamp);
        }
    }
}
