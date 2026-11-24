//! The reference graph of a file's design (docs/authoring.md, "Design
//! analysis"; docs/server-storage.md 9): which design objects refer to
//! which, built from the file's catalogs as a key tree gives them, with
//! what could not be read.
//!
//! [`build`] takes a [`KeyTree`](om_hbam::KeyTree) holding the catalogs
//! [`DESIGN_CATALOGS`] lists. It is not tied to a connection: `om-author`
//! builds it from the catalogs it downloads, and a server can build it
//! from a version of a file it hosts. Every reference comes from a decoder
//! OpenMaker already has: script steps through their parameter schema
//! (`om_scripts`), layouts and their objects (`om_layout`), formulas
//! (`om_formula`), import and export orders (`om_transfer::spec`), field
//! options, relationships, value lists, custom functions and file options
//! (`crate::meta`, `crate::field_options`).
//! Nothing is inferred from bytes those decoders do not read: such places
//! are listed in [`Coverage::unread`], and references made by a name that
//! is only looked up when the file runs (a script performed by a
//! calculated name, `Evaluate`, `ExecuteSQL`) in [`Coverage::by_name`],
//! never resolved by guessing.
//!
//! Formulas store the fields, custom functions and plug-in functions they
//! name by id (calc-encoding.md 4.1, 4.2): those are references. A name
//! inside a formula's text literal is a reference only for the function
//! that looks it up at run time, and is listed as such.
//!
//! Completeness is checked, not assumed: readers reach the catalogs only
//! through one access layer (`access`), whose accessors mark every stored
//! node they give, and a sweep at the end reports each node nothing
//! accounted for, each that was present but not readable, and each
//! holding a value beside keys. The results are findings about what was
//! read, never facts about the whole file. Those that depend on having
//! read everything (a target not found among the objects read, an object
//! not among them, nothing known unread for a kind) are made only when
//! the file was read cleanly ([`Coverage::read_cleanly`]); one anomaly
//! anywhere withdraws them all.
//!
//! Bounds: every decode is charged by the bytes it reads before it runs;
//! each script is read up to the text form's step limit
//! ([`MAX_PASTE_STEPS`](om_scripts::display::MAX_PASTE_STEPS)), formulas
//! through `om_formula`'s limits and walked without recursion, a stored
//! value many steps share is decoded once, repeated coverage entries are
//! counted past the first few of their kind ([`Coverage::omitted`]), the
//! build stops at [`MAX_ENTRIES`] entries or [`MAX_WORK`] units of work
//! saying where ([`Coverage::stopped`]), and the call chain walk visits
//! each script once.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod access;
mod catalogs;
mod formula;
mod layouts;
mod read;
mod steps;

pub use read::{build, DESIGN_CATALOGS};

/// The most references, unread places and references by name one build
/// records, together. Past it the build stops, with an unread entry where.
pub const MAX_ENTRIES: usize = 1_000_000;
/// The most work one build does: formula nodes walked, steps, layout
/// objects and stored keys read. Past it the build stops, with an unread
/// entry where.
pub const MAX_WORK: usize = 50_000_000;

/// The kinds of design object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Table,
    Field,
    Occurrence,
    Relationship,
    Layout,
    LayoutObject,
    Script,
    ValueList,
    CustomFunction,
    DataSource,
    /// A custom menu (`[65, 5, M]`), whose items are not decoded.
    CustomMenu,
    /// A privilege set (`[23, 2, 5, P]`), as the holder of its record
    /// access calculations.
    PrivilegeSet,
    /// The file's own options: the layout it opens on and its script
    /// triggers.
    File,
}

impl Kind {
    /// Every kind an object is referred to by id: what "nothing refers to
    /// it" is asked of.
    pub const REFERABLE: [Kind; 8] = [
        Kind::Table,
        Kind::Field,
        Kind::Occurrence,
        Kind::Layout,
        Kind::Script,
        Kind::ValueList,
        Kind::CustomFunction,
        Kind::DataSource,
    ];

    /// Every kind a place can refer to, by id or by name: what a place
    /// that could not be read is taken to refer to.
    pub const ANY: [Kind; 9] = [
        Kind::Table,
        Kind::Field,
        Kind::Occurrence,
        Kind::Layout,
        Kind::LayoutObject,
        Kind::Script,
        Kind::ValueList,
        Kind::CustomFunction,
        Kind::DataSource,
    ];

    /// Every kind: what a build that stopped early leaves incomplete.
    pub const ALL: [Kind; 13] = [
        Kind::Table,
        Kind::Field,
        Kind::Occurrence,
        Kind::Relationship,
        Kind::Layout,
        Kind::LayoutObject,
        Kind::Script,
        Kind::ValueList,
        Kind::CustomFunction,
        Kind::DataSource,
        Kind::CustomMenu,
        Kind::PrivilegeSet,
        Kind::File,
    ];

    /// The name references and results use (`script`, `value_list`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Kind::Table => "table",
            Kind::Field => "field",
            Kind::Occurrence => "occurrence",
            Kind::Relationship => "relationship",
            Kind::Layout => "layout",
            Kind::LayoutObject => "layout_object",
            Kind::Script => "script",
            Kind::ValueList => "value_list",
            Kind::CustomFunction => "custom_function",
            Kind::DataSource => "data_source",
            Kind::CustomMenu => "custom_menu",
            Kind::PrivilegeSet => "privilege_set",
            Kind::File => "file",
        }
    }
}

/// A design object, by the file's own ids: the ids `om-author`'s
/// references carry (docs/authoring.md, "References"). Tables, fields,
/// occurrences, relationships, layouts, value lists and custom functions
/// are numbered as their catalog keys read big-endian; scripts and data
/// sources by their keys' serial decode (file-metadata.md 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectId {
    File,
    Table(u64),
    Field { table: u64, field: u64 },
    Occurrence(u64),
    Relationship(u64),
    Layout(u64),
    LayoutObject { layout: u64, object: u64 },
    Script(u64),
    ValueList(u64),
    CustomFunction(u64),
    DataSource(u64),
    CustomMenu(u64),
    PrivilegeSet(u64),
}

impl ObjectId {
    pub fn kind(self) -> Kind {
        match self {
            ObjectId::File => Kind::File,
            ObjectId::Table(_) => Kind::Table,
            ObjectId::Field { .. } => Kind::Field,
            ObjectId::Occurrence(_) => Kind::Occurrence,
            ObjectId::Relationship(_) => Kind::Relationship,
            ObjectId::Layout(_) => Kind::Layout,
            ObjectId::LayoutObject { .. } => Kind::LayoutObject,
            ObjectId::Script(_) => Kind::Script,
            ObjectId::ValueList(_) => Kind::ValueList,
            ObjectId::CustomFunction(_) => Kind::CustomFunction,
            ObjectId::DataSource(_) => Kind::DataSource,
            ObjectId::CustomMenu(_) => Kind::CustomMenu,
            ObjectId::PrivilegeSet(_) => Kind::PrivilegeSet,
        }
    }
}

/// An object of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub id: ObjectId,
    /// Its name; empty when it has none (an unnamed layout object).
    pub name: String,
    /// What it is within its kind: a field's `normal`, `calculation` or
    /// `summary`, a layout object's kind, an occurrence's table; empty when
    /// nothing more is known.
    pub detail: String,
}

/// A script step a reference is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepPlace {
    /// The step's place in the script (from 1).
    pub index: usize,
    /// The line of `get_script`'s source text the step starts on (from 1).
    pub line: usize,
    pub id: u16,
    pub name: String,
    pub enabled: bool,
}

/// Where in its holder a reference is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The step, for a reference in a script.
    pub step: Option<StepPlace>,
    /// What holds it: a step's parameter, a field's formula
    /// (`calculation`, `auto-enter calculation`, `validation
    /// calculation`, ...), a layout object's `field`, `button`, `hide
    /// condition`, a relationship's `left occurrence`, ...
    pub part: String,
}

impl Place {
    pub(crate) fn new(part: impl Into<String>) -> Place {
        Place {
            step: None,
            part: part.into(),
        }
    }
}

/// What a reference names, as it is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// An object by its id.
    Object(ObjectId),
    /// A field named through a table occurrence, as formulas, script steps,
    /// layouts, relationships and value lists name fields: the field of
    /// the occurrence's table.
    Field { occurrence: u64, field: u64 },
    /// A script of another file, through a data source; `None` when the
    /// script's key does not read.
    ExternalScript {
        data_source: u64,
        script: Option<u64>,
    },
}

/// What a reference's target is in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// An object of the file.
    Resolved(ObjectId),
    /// Not found among the objects read, in a file read cleanly: `missing`
    /// is the object not found (a field, or the occurrence or table the
    /// field is named through). A finding about what was read, not a proof
    /// that the file lacks it (docs/authoring.md, "Design analysis").
    TargetNotFound { missing: ObjectId },
    /// An object of another file, through this data source.
    External { data_source: u64 },
    /// The catalog that would hold the target was not read
    /// ([`Coverage::unavailable`]): neither found nor known missing.
    Unresolved { reason: String },
}

/// What a holder does with a script it names. Only a perform is a call:
/// [`DesignGraph::call_chain`] follows [`ScriptRole::Performs`] and
/// [`ScriptRole::PerformsOnServer`]; the other references are uses all the
/// same. Each step that stores a script has its role from its
/// specification (`steps::script_role`; docs/authoring.md, "Call chains").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScriptRole {
    /// Performs it, in the session that runs the holder: Perform Script,
    /// a button's Perform Script.
    Performs,
    /// Has the host perform it: Perform Script on Server, with or without
    /// a callback.
    PerformsOnServer,
    /// Has it performed later, on an event: a script trigger, Install
    /// OnTimer Script, the callback of Perform Script on Server with
    /// Callback or of Execute URL.
    Installs,
    /// Names it and performs it in no way: Validate Scripts, which sends
    /// the scripts to an AI model to check.
    Names,
}

impl ScriptRole {
    pub fn code(self) -> &'static str {
        match self {
            ScriptRole::Performs => "performs",
            ScriptRole::PerformsOnServer => "performs_on_server",
            ScriptRole::Installs => "installs",
            ScriptRole::Names => "names",
        }
    }

    /// Whether the holder performs the script: a call.
    pub fn performs(self) -> bool {
        matches!(self, ScriptRole::Performs | ScriptRole::PerformsOnServer)
    }
}

/// One reference: the object that holds it, where, and what it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Use {
    pub holder: ObjectId,
    pub place: Place,
    pub target: Target,
    pub resolution: Resolution,
    /// For a reference to a script (of this file or another), what the
    /// holder does with it; `None` for every other target.
    pub script_role: Option<ScriptRole>,
}

impl Use {
    /// The occurrence or data source the target is named through.
    pub fn via(&self) -> Option<ObjectId> {
        match self.target {
            Target::Field { occurrence, .. } => Some(ObjectId::Occurrence(occurrence)),
            Target::ExternalScript { data_source, .. } => Some(ObjectId::DataSource(data_source)),
            Target::Object(_) => None,
        }
    }

    /// Whether this reference uses `id`: names it, or names something
    /// through it (a field through an occurrence, another file's object
    /// through a data source).
    pub fn uses(&self, id: ObjectId) -> bool {
        match &self.resolution {
            Resolution::Resolved(target) if *target == id => return true,
            Resolution::External { data_source } if ObjectId::DataSource(*data_source) == id => {
                return true
            }
            _ => {}
        }
        self.via() == Some(id)
    }
}

/// Why a place's references could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreadReason {
    /// OpenMaker carries it without decoding its references.
    NotDecoded,
    /// It does not decode.
    Malformed,
    /// It is past a reader's limit.
    Limit,
}

impl UnreadReason {
    pub fn code(self) -> &'static str {
        match self {
            UnreadReason::NotDecoded => "not_decoded",
            UnreadReason::Malformed => "malformed",
            UnreadReason::Limit => "limit",
        }
    }
}

/// A place whose references could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unread {
    pub holder: ObjectId,
    pub place: Place,
    pub reason: UnreadReason,
    /// What it is and why it is not read.
    pub detail: String,
    /// The kinds of object it could refer to.
    pub could_refer_to: Vec<Kind>,
}

/// How a name is looked up when the file runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameUse {
    /// Perform Script and its variants "by name".
    ScriptByName,
    /// Go to Layout and Go to Related Record by a calculated name or
    /// number.
    LayoutByCalculation,
    /// A layout by name (`GetFieldsOnLayout`).
    LayoutByName,
    /// Set Field By Name, Replace Field Contents by Name.
    FieldByName,
    /// A layout object named by a calculation (Go to Object, Refresh
    /// Object, Set Web Viewer, ...).
    ObjectByName,
    /// A table occurrence by a calculated name (Go to Related Record's
    /// table by calculation, Perform SQL Query by Natural Language,
    /// `GetTableDDL`, `GetRecordIDsFromFoundSet`).
    OccurrenceByName,
    /// A function by name (`GetToolDefinitions`).
    FunctionByName,
    /// A URL a step opens or calls: an `fmp://` URL performs a script by
    /// name, and the Data API and OData name layouts, tables and fields.
    Url,
    /// A web viewer allowed to perform scripts from JavaScript
    /// (`FileMaker.PerformScript`).
    WebViewerScript,
    /// Script text another application runs (Perform AppleScript, Send
    /// Event, Send DDE Execute), which can name a script, a layout or a
    /// field.
    ExternalScriptText,
    /// Text a step sends to be read at run time (Execute FileMaker Data
    /// API, the natural-language steps).
    RequestText,
    /// `Evaluate`'s text.
    Evaluate,
    /// `GetField`'s field name.
    GetField,
    /// `ExecuteSQL`'s query.
    ExecuteSql,
    /// `IsValidExpression`'s text.
    ExpressionCheck,
    /// A Design function, which takes object names as text.
    DesignFunction,
    /// A plug-in's function, which can do anything by name.
    PluginFunction,
}

impl NameUse {
    pub fn code(self) -> &'static str {
        match self {
            NameUse::ScriptByName => "script_by_name",
            NameUse::LayoutByCalculation => "layout_by_calculation",
            NameUse::LayoutByName => "layout_by_name",
            NameUse::FieldByName => "field_by_name",
            NameUse::ObjectByName => "object_by_name",
            NameUse::OccurrenceByName => "occurrence_by_name",
            NameUse::FunctionByName => "function_by_name",
            NameUse::Url => "url",
            NameUse::WebViewerScript => "web_viewer_script",
            NameUse::ExternalScriptText => "external_script_text",
            NameUse::RequestText => "request_text",
            NameUse::Evaluate => "evaluate",
            NameUse::GetField => "get_field",
            NameUse::ExecuteSql => "execute_sql",
            NameUse::ExpressionCheck => "expression_check",
            NameUse::DesignFunction => "design_function",
            NameUse::PluginFunction => "plugin_function",
        }
    }
}

/// A reference made by a name looked up when the file runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByName {
    pub holder: ObjectId,
    pub place: Place,
    pub how: NameUse,
    /// The function, for a Design or plug-in function.
    pub function: Option<String>,
    /// The kinds of object the name could be.
    pub could_refer_to: Vec<Kind>,
}

/// A catalog the source could not give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    /// The catalog's path in the file's key tree (`[17]` the scripts,
    /// `[32769, 3]` a table's fields).
    pub path: Vec<u64>,
    pub what: String,
    pub reason: String,
}

impl Unavailable {
    /// The kinds of object the catalog holds.
    pub fn kinds(&self) -> Vec<Kind> {
        match self.path.as_slice() {
            [2, ..] => vec![Kind::File],
            [3, ..] => vec![Kind::Table, Kind::Occurrence, Kind::Relationship],
            [4, ..] => vec![Kind::Layout, Kind::LayoutObject],
            [16, ..] => vec![Kind::CustomFunction],
            [17, ..] => vec![Kind::Script],
            [32, ..] => vec![Kind::DataSource],
            [33, ..] => vec![Kind::ValueList],
            [65, ..] => vec![Kind::CustomMenu],
            [23, ..] => vec![Kind::PrivilegeSet],
            [_, 3] => vec![Kind::Field],
            _ => Kind::ANY.to_vec(),
        }
    }

    /// Whether it would hold `id`.
    fn holds(&self, id: ObjectId) -> bool {
        match (self.path.as_slice(), id) {
            (_, ObjectId::Field { table, .. }) => self.path == [table, 3],
            // One layout's member.
            ([4, 5, member], ObjectId::Layout(layout) | ObjectId::LayoutObject { layout, .. }) => {
                *member == layout
            }
            ([4, 5, _], _) => false,
            _ => self.kinds().contains(&id.kind()),
        }
    }
}

/// What was read and what was not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// How many of each were read: `tables`, `fields`, `occurrences`,
    /// `relationships`, `layouts`, `layout_objects`, `scripts`, `steps`,
    /// `value_lists`, `custom_functions`, `data_sources`, `formulas`.
    pub read: BTreeMap<&'static str, usize>,
    pub unread: Vec<Unread>,
    pub by_name: Vec<ByName>,
    pub unavailable: Vec<Unavailable>,
    /// Where the build stopped when a budget ([`MAX_ENTRIES`],
    /// [`MAX_WORK`]) was spent: that place and everything after it were not
    /// read, and no kind is complete.
    pub stopped: Option<Unread>,
    /// Unread places and references by name past the first few of their
    /// kind of place, counted rather than kept: each still counts for
    /// [`Self::nothing_known_unread_for`].
    pub omitted: Vec<Omitted>,
    /// The first anomalies met ([`Self::read_cleanly`]), each also an
    /// unread place; [`Self::anomaly_count`] counts them all.
    pub anomalies: Vec<Unread>,
    pub anomaly_count: usize,
    /// Catalogs the source saw change while it read them: their
    /// modification counts differed at the start and the end of the read
    /// ([`DesignGraph::note_changed_during_read`]). Empty when nothing was
    /// seen to change, or the source could not tell.
    pub changed_during_read: Vec<String>,
}

/// Entries of one kind of place past the first few, counted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Omitted {
    pub entry: OmittedEntry,
    /// The kind of object holding them.
    pub holder: Kind,
    /// Their place, with digits folded to `#`.
    pub place: String,
    pub could_refer_to: Vec<Kind>,
    pub count: usize,
}

/// What an [`Omitted`] group counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmittedEntry {
    Unread(UnreadReason),
    ByName(NameUse),
}

impl Coverage {
    pub(crate) fn count(&mut self, what: &'static str, n: usize) {
        *self.read.entry(what).or_default() += n;
    }

    /// The unread places that could refer to an object of `kind`.
    pub fn unread_for(&self, kind: Kind) -> impl Iterator<Item = &Unread> {
        self.unread
            .iter()
            .filter(move |u| u.could_refer_to.contains(&kind))
    }

    /// The references by name that could name an object of `kind`.
    pub fn by_name_for(&self, kind: Kind) -> impl Iterator<Item = &ByName> {
        self.by_name
            .iter()
            .filter(move |b| b.could_refer_to.contains(&kind))
    }

    /// Whether nothing known to be unread could refer to an object of
    /// `kind`: the file was read cleanly, and no place OpenMaker did not
    /// decode and no name looked up at run time could name one. Not a
    /// guarantee that nothing refers to it: a place OpenMaker reads wrongly
    /// is not known to be unread.
    pub fn nothing_known_unread_for(&self, kind: Kind) -> bool {
        self.read_cleanly()
            && self.unread_for(kind).next().is_none()
            && self.by_name_for(kind).next().is_none()
            && !self
                .omitted
                .iter()
                .any(|o| o.could_refer_to.contains(&kind))
            && self.unavailable.is_empty()
    }

    /// Whether the file was read cleanly: the source gave every catalog
    /// asked for and reported no failure of its own ([`Self::unavailable`]
    /// empty), no stored node was failed or left unread, none held a value
    /// beside keys, no value was rejected, and no budget stopped anything.
    /// Only then is a finding made that depends on having read everything:
    /// a target not found among the objects read, an object not among
    /// them, nothing known unread for a kind. A place OpenMaker knows and
    /// does not decode (a custom menu, a reference by name) is coverage,
    /// not an anomaly.
    pub fn read_cleanly(&self) -> bool {
        self.anomaly_count == 0 && self.stopped.is_none() && self.unavailable.is_empty()
    }

    /// Whether the design was seen not to change while it was read
    /// ([`Self::changed_during_read`]).
    pub fn consistent(&self) -> bool {
        self.changed_during_read.is_empty()
    }

    /// Why the file was not read cleanly, when it was not.
    pub fn not_clean_reason(&self) -> Option<String> {
        if let Some(stop) = &self.stopped {
            return Some(format!(
                "the analysis stopped at its budget: {}",
                stop.detail
            ));
        }
        if let Some(u) = self.unavailable.first() {
            return Some(format!("{}: not read ({})", u.what, u.reason));
        }
        if self.anomaly_count == 0 {
            return None;
        }
        Some(match (self.anomaly_count, self.anomalies.first()) {
            (1, Some(a)) => format!("1 anomaly, at {}: {}", a.place.part, a.detail),
            (n, Some(a)) => format!("{n} anomalies, the first at {}: {}", a.place.part, a.detail),
            (n, None) => format!("{n} anomalies"),
        })
    }

    /// How many unread places there are, those counted in [`Self::omitted`]
    /// included.
    pub fn unread_total(&self) -> usize {
        self.unread.len()
            + self
                .omitted
                .iter()
                .filter(|o| matches!(o.entry, OmittedEntry::Unread(_)))
                .map(|o| o.count)
                .sum::<usize>()
    }

    /// How many references by name there are, those counted in
    /// [`Self::omitted`] included.
    pub fn by_name_total(&self) -> usize {
        self.by_name.len()
            + self
                .omitted
                .iter()
                .filter(|o| matches!(o.entry, OmittedEntry::ByName(_)))
                .map(|o| o.count)
                .sum::<usize>()
    }
}

/// The direction of a call chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The scripts it performs.
    Calls,
    /// The scripts that perform it.
    Callers,
}

/// A script reached in a call chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reached {
    pub script: u64,
    /// The fewest performs between it and the start (1: directly).
    pub depth: usize,
}

/// One reference from a script to a script: a perform in
/// [`CallChain::calls`], any other role in [`CallChain::others`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call<'g> {
    /// The script whose step names `to`.
    pub from: u64,
    pub to: u64,
    pub role: ScriptRole,
    pub place: &'g Place,
}

/// A script's calls or callers, transitively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallChain<'g> {
    pub direction: Direction,
    /// Each script reached once, nearest first; the start only when a
    /// cycle comes back to it.
    pub scripts: Vec<Reached>,
    /// Every perform between the scripts walked, in the file's script
    /// order.
    pub calls: Vec<Call<'g>>,
    /// The references between a script walked and another script that are
    /// not performs (a trigger, timer or callback installed, a script only
    /// named): uses, not calls, so not walked further. For
    /// [`Direction::Calls`] those a walked script holds; for
    /// [`Direction::Callers`] those naming a walked script.
    pub others: Vec<Call<'g>>,
    /// Each cycle found, as the scripts on it in walk order: the first
    /// performs (or is performed by) the second, ..., the last the first.
    pub cycles: Vec<Vec<u64>>,
    /// More cycles than [`MAX_CYCLES`] were found; the rest are left out.
    pub cycles_truncated: bool,
}

/// The most cycles a call chain lists.
pub const MAX_CYCLES: usize = 100;

/// The base of a table occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Base {
    Table(u64),
    External(u64),
    /// Not known, and why: its definition is not stored, does not read or
    /// does not decode, or its data source key does not read.
    Unknown(&'static str),
}

/// The reference graph of one file's design.
#[derive(Clone, Debug, Default)]
pub struct DesignGraph {
    objects: BTreeMap<ObjectId, Object>,
    uses: Vec<Use>,
    coverage: Coverage,
    bases: BTreeMap<u64, Base>,
}

impl DesignGraph {
    /// Every object, by kind and id.
    pub fn objects(&self) -> impl Iterator<Item = &Object> {
        self.objects.values()
    }

    pub fn object(&self, id: ObjectId) -> Option<&Object> {
        self.objects.get(&id)
    }

    /// Every reference, in the order read.
    pub fn uses(&self) -> &[Use] {
        &self.uses
    }

    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// Whether an object of `kind` the graph does not have may be said to
    /// be not among the objects read: only when the file was read cleanly
    /// ([`Coverage::read_cleanly`]), whatever the kind. A finding about
    /// what was read, not proof that the file lacks it.
    pub fn listed_whole(&self, _kind: Kind) -> bool {
        self.coverage.read_cleanly()
    }

    /// Why not, when not: what made the read not clean
    /// ([`Coverage::not_clean_reason`]), whatever the kind; it says nothing
    /// of the objects of `kind` in particular.
    pub fn unlisted_reason(&self, _kind: Kind) -> Option<String> {
        self.coverage.not_clean_reason()
    }

    /// The table occurrence `id` shows, when it is one of this file's.
    pub fn occurrence_table(&self, id: u64) -> Option<u64> {
        match self.bases.get(&id) {
            Some(Base::Table(table)) => Some(*table),
            _ => None,
        }
    }

    /// Every reference that uses `id` ([`Use::uses`]), but those the
    /// object holds itself.
    pub fn uses_of(&self, id: ObjectId) -> Vec<&Use> {
        self.uses
            .iter()
            .filter(|u| u.holder != id && u.uses(id))
            .collect()
    }

    /// Every reference whose target was not found among the objects read,
    /// in a file read cleanly.
    pub fn targets_not_found(&self) -> Vec<&Use> {
        self.uses
            .iter()
            .filter(|u| matches!(u.resolution, Resolution::TargetNotFound { .. }))
            .collect()
    }

    /// Record the catalogs the source saw change while it read them
    /// ([`Coverage::changed_during_read`]).
    pub fn note_changed_during_read(&mut self, changed: Vec<String>) {
        self.coverage.changed_during_read = changed;
    }

    /// The objects of `kind` for which no reference was found in what was
    /// read, but references they hold themselves ([`Self::uses_of`]):
    /// candidates to check, not objects known to be unused. Script folders
    /// and layout folders are not objects.
    pub fn no_reference_found(&self, kind: Kind) -> Vec<&Object> {
        let mut used = BTreeSet::new();
        for u in &self.uses {
            let mut mark = |id: ObjectId| {
                if id != u.holder && id.kind() == kind {
                    used.insert(id);
                }
            };
            match &u.resolution {
                Resolution::Resolved(id) => mark(*id),
                Resolution::External { data_source } => mark(ObjectId::DataSource(*data_source)),
                _ => {}
            }
            if let Some(via) = u.via() {
                mark(via);
            }
        }
        self.objects
            .values()
            .filter(|o| o.id.kind() == kind && !used.contains(&o.id))
            .collect()
    }

    /// The references between scripts of this file: holder, target, role
    /// and the reference.
    fn script_references(&self) -> impl Iterator<Item = (u64, u64, ScriptRole, &Use)> {
        self.uses
            .iter()
            .filter_map(|u| match (u.holder, &u.resolution) {
                (ObjectId::Script(from), Resolution::Resolved(ObjectId::Script(to))) => {
                    Some((from, *to, u.script_role?, u))
                }
                _ => None,
            })
    }

    /// The scripts `script` performs ([`Direction::Calls`]) or that
    /// perform it ([`Direction::Callers`]), transitively: Perform Script
    /// and Perform Script on Server ([`ScriptRole::performs`]). A trigger,
    /// timer or callback installed and a script only named are listed in
    /// [`CallChain::others`] and not followed. Each script is walked once,
    /// so the walk ends whatever cycles there are; cycles are reported.
    pub fn call_chain(&self, script: u64, direction: Direction) -> CallChain<'_> {
        // The performs between scripts, in both directions.
        let mut next: BTreeMap<u64, Vec<(u64, &Use)>> = BTreeMap::new();
        for (from, to, role, u) in self.script_references() {
            if !role.performs() {
                continue;
            }
            let to = &to;
            let (a, b) = match direction {
                Direction::Calls => (from, *to),
                Direction::Callers => (*to, from),
            };
            next.entry(a).or_default().push((b, u));
        }
        let empty = Vec::new();
        let edges = |s: u64| next.get(&s).unwrap_or(&empty);
        // Nearest first.
        let mut depth: BTreeMap<u64, usize> = BTreeMap::from([(script, 0)]);
        let mut order = Vec::new();
        let mut queue = VecDeque::from([script]);
        let mut back_to_start = false;
        while let Some(s) = queue.pop_front() {
            let d = depth[&s];
            for (t, _) in edges(s) {
                if *t == script {
                    back_to_start = true;
                }
                if !depth.contains_key(t) {
                    depth.insert(*t, d + 1);
                    order.push(Reached {
                        script: *t,
                        depth: d + 1,
                    });
                    queue.push_back(*t);
                }
            }
        }
        if back_to_start {
            // The start comes back: list it, at the depth of the shortest
            // cycle through it.
            let back = depth
                .iter()
                .filter(|(s, _)| edges(**s).iter().any(|(t, _)| *t == script))
                .map(|(_, d)| d + 1)
                .min()
                .unwrap_or(1);
            order.push(Reached {
                script,
                depth: back,
            });
        }
        let mut calls = Vec::new();
        let mut others = Vec::new();
        for (from, to, role, u) in self.script_references() {
            let walked = match direction {
                Direction::Calls => depth.contains_key(&from),
                Direction::Callers => depth.contains_key(&to),
            };
            if !walked {
                continue;
            }
            let call = Call {
                from,
                to,
                role,
                place: &u.place,
            };
            if role.performs() {
                calls.push(call);
            } else {
                others.push(call);
            }
        }
        // Cycles: each edge to a script on the depth-first path closes one.
        // Each script is expanded once, so each closing edge is met once
        // (twice only for two performs between the same scripts); a path
        // is copied only for the first MAX_CYCLES.
        let mut cycles = Vec::new();
        let mut truncated = false;
        let mut done: BTreeSet<u64> = BTreeSet::new();
        let mut path: Vec<u64> = vec![script];
        let mut on_path: BTreeSet<u64> = BTreeSet::from([script]);
        let mut stack: Vec<(u64, usize)> = vec![(script, 0)];
        let mut closing: BTreeSet<(u64, u64)> = BTreeSet::new();
        while let Some((s, i)) = stack.pop() {
            let out = edges(s);
            if i < out.len() {
                stack.push((s, i + 1));
                let t = out[i].0;
                if on_path.contains(&t) {
                    if closing.insert((s, t)) {
                        if cycles.len() < MAX_CYCLES {
                            let at = path.iter().position(|p| *p == t).unwrap_or(0);
                            cycles.push(path[at..].to_vec());
                        } else {
                            truncated = true;
                        }
                    }
                } else if !done.contains(&t) {
                    path.push(t);
                    on_path.insert(t);
                    stack.push((t, 0));
                }
            } else {
                done.insert(s);
                on_path.remove(&s);
                path.pop();
            }
        }
        CallChain {
            direction,
            scripts: order,
            calls,
            others,
            cycles,
            cycles_truncated: truncated,
        }
    }

    /// What `target` names in this file.
    fn resolve(&self, target: Target) -> Resolution {
        let unavailable = |id: ObjectId| {
            self.coverage
                .unavailable
                .iter()
                .find(|u| u.holds(id))
                .map(|u| Resolution::Unresolved {
                    reason: format!("{}: not read ({})", u.what, u.reason),
                })
        };
        // Missing only when the file was read cleanly; otherwise not known.
        let absent = |id: ObjectId| {
            unavailable(id).unwrap_or_else(|| match self.coverage.not_clean_reason() {
                None => Resolution::TargetNotFound { missing: id },
                Some(reason) => Resolution::Unresolved {
                    reason: format!("the file was not read cleanly: {reason}"),
                },
            })
        };
        let object = |id: ObjectId| {
            if self.objects.contains_key(&id) {
                Resolution::Resolved(id)
            } else {
                absent(id)
            }
        };
        match target {
            Target::Object(ObjectId::Field { table, field }) => {
                if !self.objects.contains_key(&ObjectId::Table(table)) {
                    return absent(ObjectId::Table(table));
                }
                object(ObjectId::Field { table, field })
            }
            Target::Object(id) => object(id),
            Target::Field { occurrence, field } => match self.bases.get(&occurrence) {
                None => object(ObjectId::Occurrence(occurrence)),
                Some(Base::External(source)) => match object(ObjectId::DataSource(*source)) {
                    Resolution::Resolved(_) => Resolution::External {
                        data_source: *source,
                    },
                    other => other,
                },
                Some(Base::Unknown(why)) => Resolution::Unresolved {
                    reason: format!(
                        "the table of table occurrence {occurrence} is not known: {why}"
                    ),
                },
                Some(Base::Table(table)) => self.resolve(Target::Object(ObjectId::Field {
                    table: *table,
                    field,
                })),
            },
            Target::ExternalScript { data_source, .. } => {
                match object(ObjectId::DataSource(data_source)) {
                    Resolution::Resolved(_) => Resolution::External { data_source },
                    other => other,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
