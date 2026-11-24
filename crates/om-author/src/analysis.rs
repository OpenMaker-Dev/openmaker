//! The design analysis commands (docs/authoring.md, "Design analysis"):
//! where an object is used, broken references, objects nothing refers to,
//! and a script's call chain. Each builds the reference graph of the
//! file's current design (`om_data::design_refs`) from the design catalogs
//! downloaded over a new connection (`NativeProvider::design_catalogs`),
//! and reads no record.
//!
//! Every result carries its coverage, for the kinds of object it is about:
//! what was read, the places whose references could not be read, the
//! references made by a name looked up when the file runs, and the
//! catalogs the host did not give. An object nothing decoded refers to is
//! `unreferenced` only when nothing of that coverage could refer to its
//! kind; otherwise it is `no_decoded_reference`.

use om_data::design_refs::{
    self, CallChain, DesignGraph, Direction, Kind, ObjectId, OmittedEntry, Place, Resolution,
    Target, Use,
};
use om_scripts::display::Lookup;
use serde_json::{json, Value};

use crate::api::{
    AnalysisParams, AuthorError, CallDirection, ErrorKind, ObjectKind, ObjectParams,
    ScriptCallsParams, UnusedParams,
};
use crate::refs::{self, reference};
use crate::service::Service;

/// How many references, objects and places a result lists by default,
/// and at most.
const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 100_000;

fn limit(limit: Option<usize>) -> Result<usize, AuthorError> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(n) => Err(AuthorError::new(
            ErrorKind::InvalidRequest,
            format!("limit is {n}; give 1 to {MAX_LIMIT}."),
        )),
    }
}

fn kind_of(kind: ObjectKind) -> Kind {
    match kind {
        ObjectKind::Table => Kind::Table,
        ObjectKind::Field => Kind::Field,
        ObjectKind::Occurrence => Kind::Occurrence,
        ObjectKind::Layout => Kind::Layout,
        ObjectKind::Script => Kind::Script,
        ObjectKind::ValueList => Kind::ValueList,
        ObjectKind::CustomFunction => Kind::CustomFunction,
        ObjectKind::DataSource => Kind::DataSource,
    }
}

/// The first `limit` of `items` as JSON, with their total.
fn listed<T>(items: &[T], limit: usize, json: impl Fn(&T) -> Value) -> (Vec<Value>, usize, bool) {
    let listed = items.iter().take(limit).map(json).collect();
    (listed, items.len(), items.len() > limit)
}

/// One file's graph, and its name for references.
struct Analysis {
    graph: DesignGraph,
    file: String,
}

impl Analysis {
    /// `id`'s reference, `kind:id@file`; the file's own options have none.
    fn reference(&self, id: ObjectId) -> Value {
        let file = &self.file;
        let text = match id {
            ObjectId::File => return Value::Null,
            ObjectId::Table(t) => reference("table", t, file),
            ObjectId::Field { table, field } => {
                reference("field", format!("{table}.{field}"), file)
            }
            ObjectId::Occurrence(o) => reference("occurrence", o, file),
            ObjectId::Relationship(r) => reference("relationship", r, file),
            ObjectId::Layout(l) => reference("layout", l, file),
            ObjectId::LayoutObject { layout, object } => {
                reference("layout_object", format!("{layout}.{object}"), file)
            }
            ObjectId::Script(s) => reference("script", s, file),
            ObjectId::ValueList(v) => reference("value_list", v, file),
            ObjectId::CustomFunction(c) => reference("custom_function", c, file),
            ObjectId::DataSource(d) => reference("data_source", d, file),
            ObjectId::CustomMenu(m) => reference("custom_menu", m, file),
            ObjectId::PrivilegeSet(p) => reference("privilege_set", p, file),
        };
        json!(text)
    }

    /// An object as results show it: its reference, kind and name, and
    /// whether the file has it; a field's table and a layout object's
    /// layout.
    fn object(&self, id: ObjectId) -> Value {
        let found = self.graph.object(id);
        let mut out = json!({
            "ref": self.reference(id),
            "kind": id.kind().name(),
            "name": match id {
                ObjectId::File => Some(self.file.clone()),
                _ => found.map(|o| o.name.clone()),
            },
            "found": self.found(id),
        });
        if let Some(reason) = self.not_known(id) {
            out["not_known_because"] = json!(reason);
        }
        if let Some(detail) = found.map(|o| &o.detail).filter(|d| !d.is_empty()) {
            out["detail"] = json!(detail);
        }
        match id {
            ObjectId::Field { table, .. } => out["table"] = self.short(ObjectId::Table(table)),
            ObjectId::LayoutObject { layout, .. } => {
                out["layout"] = self.short(ObjectId::Layout(layout))
            }
            _ => {}
        }
        out
    }

    /// Whether `id` was found among the objects read: true, false, or null
    /// when the graph does not have it and the file was not read cleanly.
    /// False says only that it was not found, never that the file lacks it.
    fn found(&self, id: ObjectId) -> Value {
        if id == ObjectId::File || self.graph.object(id).is_some() {
            return json!(true);
        }
        if self.not_known(id).is_some() {
            return Value::Null;
        }
        json!(false)
    }

    /// Why whether `id` is among the objects read is not known, when it is
    /// not: the file was not read cleanly.
    fn not_known(&self, id: ObjectId) -> Option<String> {
        if id == ObjectId::File || self.graph.object(id).is_some() {
            return None;
        }
        self.graph.unlisted_reason(id.kind())
    }

    /// The error for an object that cannot be found: `not_found` (not
    /// among the objects read) when the file was read cleanly, else
    /// `not_known` with the reason.
    fn missing(&self, kind: Kind, message: String) -> AuthorError {
        match self.graph.unlisted_reason(kind) {
            None => AuthorError::new(ErrorKind::NotFound, message),
            Some(reason) => AuthorError::new(
                ErrorKind::NotKnown,
                format!(
                    "{message} Whether it is there is not known: {} was not read cleanly, \
                     so not every {} was listed.",
                    self.file,
                    kind.name().replace('_', " "),
                ),
            )
            .with_details(json!({
                "kind": kind.name(),
                "reason": reason,
                "stopped": self.graph.coverage().stopped.is_some(),
            })),
        }
    }

    /// An object's reference and name.
    fn short(&self, id: ObjectId) -> Value {
        json!({
            "ref": self.reference(id),
            "name": self.graph.object(id).map(|o| o.name.clone()),
        })
    }

    fn place(&self, place: &Place) -> Value {
        let mut out = json!({ "part": place.part });
        if let Some(step) = &place.step {
            out["step"] = json!({
                "index": step.index,
                "line": step.line,
                "id": step.id,
                "name": step.name,
                "enabled": step.enabled,
            });
        }
        out
    }

    /// What a reference names: the object it resolves to, or as stored.
    fn target(&self, u: &Use) -> Value {
        if let Resolution::Resolved(id) = u.resolution {
            return self.object(id);
        }
        match u.target {
            Target::Object(id) => self.object(id),
            Target::Field { occurrence, field } => {
                let mut out = json!({ "kind": "field", "field_id": field });
                if let Some(table) = self.graph.occurrence_table(occurrence) {
                    out["ref"] = self.reference(ObjectId::Field { table, field });
                    out["table"] = self.short(ObjectId::Table(table));
                }
                out
            }
            Target::ExternalScript { script, .. } => json!({
                "kind": "script",
                "script_id": script,
            }),
        }
    }

    fn reference_use(&self, u: &Use) -> Value {
        let mut out = json!({
            "holder": self.object(u.holder),
            "place": self.place(&u.place),
            "target": self.target(u),
        });
        if let Some(via) = u.via() {
            out["via"] = self.short(via);
        }
        match &u.resolution {
            Resolution::Resolved(_) => out["resolution"] = json!("resolved"),
            Resolution::TargetNotFound { missing } => {
                out["resolution"] = json!("target_not_found");
                out["missing"] = self.object(*missing);
            }
            Resolution::External { data_source } => {
                out["resolution"] = json!("external");
                out["data_source"] = self.short(ObjectId::DataSource(*data_source));
            }
            Resolution::Unresolved { reason } => {
                out["resolution"] = json!("unresolved");
                out["reason"] = json!(reason);
            }
        }
        out
    }

    fn kinds(kinds: &[Kind]) -> Vec<&'static str> {
        kinds.iter().map(|k| k.name()).collect()
    }

    /// The coverage of a result about objects of `kinds`.
    fn coverage(&self, kinds: &[Kind], limit: usize) -> Value {
        let coverage = self.graph.coverage();
        let relevant = |could: &[Kind]| could.iter().any(|k| kinds.contains(k));
        let unread: Vec<_> = coverage
            .unread
            .iter()
            .filter(|u| relevant(&u.could_refer_to))
            .collect();
        let by_name: Vec<_> = coverage
            .by_name
            .iter()
            .filter(|b| relevant(&b.could_refer_to))
            .collect();
        let (unread_listed, unread_total, unread_truncated) = listed(&unread, limit, |u| {
            json!({
                "holder": self.object(u.holder),
                "place": self.place(&u.place),
                "reason": u.reason.code(),
                "detail": u.detail,
                "could_refer_to": Self::kinds(&u.could_refer_to),
            })
        });
        let (by_name_listed, by_name_total, by_name_truncated) = listed(&by_name, limit, |b| {
            let mut out = json!({
                "holder": self.object(b.holder),
                "place": self.place(&b.place),
                "how": b.how.code(),
                "could_refer_to": Self::kinds(&b.could_refer_to),
            });
            if let Some(function) = &b.function {
                out["function"] = json!(function);
            }
            out
        });
        // Places past the first few of their kind, counted: they count
        // in the totals, listed by kind of place.
        let mut unread_more = 0;
        let mut by_name_more = 0;
        let omitted: Vec<Value> = coverage
            .omitted
            .iter()
            .filter(|o| relevant(&o.could_refer_to))
            .take(limit)
            .map(|o| {
                let (entry, code) = match o.entry {
                    OmittedEntry::Unread(reason) => ("unread", reason.code()),
                    OmittedEntry::ByName(how) => ("by_name", how.code()),
                };
                json!({
                    "entry": entry,
                    "code": code,
                    "holder_kind": o.holder.name(),
                    "place": o.place,
                    "could_refer_to": Self::kinds(&o.could_refer_to),
                    "count": o.count,
                })
            })
            .collect();
        for o in coverage
            .omitted
            .iter()
            .filter(|o| relevant(&o.could_refer_to))
        {
            match o.entry {
                OmittedEntry::Unread(_) => unread_more += o.count,
                OmittedEntry::ByName(_) => by_name_more += o.count,
            }
        }
        // Whether the file was read cleanly, with its first anomalies: a
        // fixed part of every result, never cut by `limit`, since every
        // strong claim depends on it.
        let anomalies: Vec<Value> = coverage
            .anomalies
            .iter()
            .take(limit)
            .map(|u| {
                json!({
                    "holder": self.object(u.holder),
                    "place": self.place(&u.place),
                    "reason": u.reason.code(),
                    "detail": u.detail,
                })
            })
            .collect();
        // Where the build stopped at a budget: a fixed part of every
        // result, never cut by `limit`, since it governs all of it.
        let stopped = coverage.stopped.as_ref().map(|u| {
            json!({
                "holder": self.object(u.holder),
                "place": self.place(&u.place),
                "detail": u.detail,
            })
        });
        json!({
            "kinds": Self::kinds(kinds),
            "nothing_known_unread": kinds.iter().all(|k| coverage.nothing_known_unread_for(*k)),
            "read_cleanly": coverage.read_cleanly(),
            "consistent": coverage.consistent(),
            "changed_during_read": coverage.changed_during_read,
            "anomalies": {
                "total": coverage.anomaly_count,
                "places": anomalies,
            },
            "stopped": stopped,
            "read": coverage.read,
            "unread": {
                "total": unread_total + unread_more,
                "truncated": unread_truncated || unread_more > 0,
                "places": unread_listed,
            },
            "by_name": {
                "total": by_name_total + by_name_more,
                "truncated": by_name_truncated || by_name_more > 0,
                "places": by_name_listed,
            },
            "omitted": omitted,
            "unavailable": coverage.unavailable.iter().map(|u| json!({
                "path": u.path,
                "what": u.what,
                "reason": u.reason,
            })).collect::<Vec<_>>(),
        })
    }

    /// The one object of `kind` named `name` among `candidates` (id and
    /// name), matched as FileMaker matches names.
    fn named(
        &self,
        kind: Kind,
        what: &str,
        name: &str,
        candidates: Vec<(ObjectId, &str)>,
    ) -> Result<ObjectId, AuthorError> {
        let ids: Vec<(u64, &str)> = candidates
            .iter()
            .enumerate()
            .map(|(i, (_, n))| (i as u64, *n))
            .collect();
        match Lookup::by_name(ids, name) {
            Lookup::One(i) => Ok(candidates[i as usize].0),
            Lookup::Missing => Err(self.missing(
                kind,
                format!(
                    "No {what} named “{name}” was found among the objects read from {}.",
                    self.file
                ),
            )),
            Lookup::Ambiguous => {
                let folded = name.to_lowercase();
                let refs: Vec<Value> = candidates
                    .iter()
                    .filter(|(_, n)| n.to_lowercase() == folded)
                    .map(|(id, _)| self.reference(*id))
                    .collect();
                Err(AuthorError::new(
                    ErrorKind::AmbiguousReference,
                    format!(
                        "Several {what}s of {} are named “{name}”; name one by its reference.",
                        self.file
                    ),
                )
                .with_details(json!({ "candidates": refs })))
            }
        }
    }

    /// The objects of `kind`, with their names.
    fn of_kind(&self, kind: Kind) -> Vec<(ObjectId, &str)> {
        self.graph
            .objects()
            .filter(|o| o.id.kind() == kind)
            .map(|o| (o.id, o.name.as_str()))
            .collect()
    }

    /// The object a reference of any analysed kind names.
    fn by_reference(&self, text: &str) -> Result<ObjectId, AuthorError> {
        let file = &self.file;
        let kind = text.split_once(':').map(|(k, _)| k).unwrap_or_default();
        Ok(match kind {
            "script" => ObjectId::Script(refs::script_id(text, file)?),
            "table" => ObjectId::Table(refs::table_id(text, file)?),
            "field" => {
                let (table, field) = refs::field_id(text, file)?;
                ObjectId::Field { table, field }
            }
            "occurrence" => ObjectId::Occurrence(refs::occurrence_id(text, file)?),
            "layout" => ObjectId::Layout(refs::layout_id(text, file)?),
            "layout_object" => {
                let (layout, object) = refs::layout_object_id(text, file)?;
                ObjectId::LayoutObject { layout, object }
            }
            "value_list" => ObjectId::ValueList(refs::value_list_id(text, file)?),
            "custom_function" => ObjectId::CustomFunction(refs::custom_function_id(text, file)?),
            "data_source" => ObjectId::DataSource(refs::data_source_id(text, file)?),
            "relationship" => ObjectId::Relationship(refs::relationship_id(text, file)?),
            _ => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    format!(
                        "“{text}” is not a reference to a script, table, field, occurrence, \
                         layout, layout object, value list, custom function, data source or \
                         relationship of {file}."
                    ),
                ))
            }
        })
    }

    /// The object `params` names: one reference or one name.
    fn resolve(&self, params: &ObjectParams) -> Result<ObjectId, AuthorError> {
        let invalid = |message: &str| AuthorError::new(ErrorKind::InvalidRequest, message);
        let names = [
            (&params.script_name, Kind::Script, "script"),
            (
                &params.occurrence_name,
                Kind::Occurrence,
                "table occurrence",
            ),
            (&params.layout_name, Kind::Layout, "layout"),
            (&params.value_list_name, Kind::ValueList, "value list"),
            (
                &params.custom_function_name,
                Kind::CustomFunction,
                "custom function",
            ),
            (&params.data_source_name, Kind::DataSource, "data source"),
        ];
        let given = names.iter().filter(|(n, ..)| n.is_some()).count()
            + usize::from(params.object.is_some())
            + usize::from(params.field_name.is_some())
            + usize::from(params.table_name.is_some() && params.field_name.is_none());
        if given != 1 {
            return Err(invalid(
                "Name one object: `object` (a reference), or one of `script_name`, \
                 `table_name`, `occurrence_name`, `layout_name`, `value_list_name`, \
                 `custom_function_name`, `data_source_name`, or `field_name` with `table` \
                 or `table_name`.",
            ));
        }
        if params.table.is_some() && params.field_name.is_none() {
            return Err(invalid("`table` names a field's table, with `field_name`."));
        }
        if let Some(text) = &params.object {
            return self.by_reference(text);
        }
        for (name, kind, what) in names {
            if let Some(name) = name {
                return self.named(kind, what, name, self.of_kind(kind));
            }
        }
        let table = match (&params.table, &params.table_name) {
            (Some(text), None) => ObjectId::Table(refs::table_id(text, &self.file)?),
            (None, Some(name)) => {
                self.named(Kind::Table, "table", name, self.of_kind(Kind::Table))?
            }
            (Some(_), Some(_)) => return Err(invalid("Give `table` or `table_name`, not both.")),
            (None, None) => {
                return Err(invalid(
                    "`field_name` needs the field's table: `table` or `table_name`.",
                ))
            }
        };
        let Some(name) = &params.field_name else {
            return Ok(table);
        };
        let ObjectId::Table(id) = table else {
            return Ok(table);
        };
        let fields = self
            .graph
            .objects()
            .filter(|o| matches!(o.id, ObjectId::Field { table, .. } if table == id))
            .map(|o| (o.id, o.name.as_str()))
            .collect();
        self.named(Kind::Field, "field", name, fields)
    }
}

impl Service {
    /// The reference graph of the file's design, read now.
    fn analysis(&self) -> Result<Analysis, AuthorError> {
        let file = self.file();
        let mut opened = self.open()?;
        let catalogs = opened.design_catalogs().map_err(Self::read_error)?;
        let tree = catalogs.tree().map_err(Self::read_error)?;
        let mut graph = design_refs::build(&tree, catalogs.unavailable.clone());
        graph.note_changed_during_read(catalogs.changed.clone());
        Ok(Analysis { graph, file })
    }

    pub(crate) fn find_uses(&mut self, params: &ObjectParams) -> Result<Value, AuthorError> {
        let limit = limit(params.limit)?;
        let analysis = self.analysis()?;
        let id = analysis.resolve(params)?;
        let graph = &analysis.graph;
        if graph.object(id).is_none() && params.object.is_none() {
            return Err(analysis.missing(
                id.kind(),
                format!(
                    "That object was not found among the objects read from {}.",
                    analysis.file
                ),
            ));
        }
        // An object the file no longer has: the references still naming it.
        let uses: Vec<&Use> = graph
            .uses()
            .iter()
            .filter(|u| {
                u.holder != id
                    && (u.uses(id)
                        || matches!(u.resolution, Resolution::TargetNotFound { missing } if missing == id))
            })
            .collect();
        let (listed, total, truncated) = listed(&uses, limit, |u| analysis.reference_use(u));
        let kind = id.kind();
        Ok(json!({
            "file": analysis.file,
            "object": analysis.object(id),
            "uses": listed,
            "total": total,
            "truncated": truncated,
            "nothing_known_unread": graph.coverage().nothing_known_unread_for(kind),
            "coverage": analysis.coverage(&[kind], limit),
        }))
    }

    pub(crate) fn broken_references(
        &mut self,
        params: &AnalysisParams,
    ) -> Result<Value, AuthorError> {
        let limit = limit(params.limit)?;
        let analysis = self.analysis()?;
        let not_found = analysis.graph.targets_not_found();
        let (listed, total, truncated) = listed(&not_found, limit, |u| analysis.reference_use(u));
        let kinds = Kind::REFERABLE;
        Ok(json!({
            "file": analysis.file,
            "references": listed,
            "total": total,
            "truncated": truncated,
            "meaning": "References whose target was not found among the objects read, in a \
                        file read cleanly: findings to check before acting on them. How far \
                        to trust them is in coverage (read_cleanly, consistent, anomalies).",
            "coverage": analysis.coverage(&kinds, limit),
        }))
    }

    pub(crate) fn unused_objects(&mut self, params: &UnusedParams) -> Result<Value, AuthorError> {
        let limit = limit(params.limit)?;
        let kinds: Vec<Kind> = match &params.kinds {
            Some(kinds) if kinds.is_empty() => {
                return Err(AuthorError::new(
                    ErrorKind::InvalidRequest,
                    "kinds is empty; leave it out for every kind.",
                ))
            }
            Some(kinds) => {
                let mut kinds: Vec<Kind> = kinds.iter().map(|k| kind_of(*k)).collect();
                kinds.sort();
                kinds.dedup();
                kinds
            }
            None => Kind::REFERABLE.to_vec(),
        };
        let analysis = self.analysis()?;
        let mut objects = Vec::new();
        let mut nothing_known_unread = serde_json::Map::new();
        for kind in &kinds {
            nothing_known_unread.insert(
                kind.name().into(),
                json!(analysis.graph.coverage().nothing_known_unread_for(*kind)),
            );
            objects.extend(
                analysis
                    .graph
                    .no_reference_found(*kind)
                    .into_iter()
                    .map(|object| object.id),
            );
        }
        let (listed, total, truncated) = listed(&objects, limit, |id| {
            let mut out = analysis.object(*id);
            out["finding"] = json!("no_reference_found");
            out
        });
        Ok(json!({
            "file": analysis.file,
            "kinds": Analysis::kinds(&kinds),
            "objects": listed,
            "total": total,
            "truncated": truncated,
            "nothing_known_unread": nothing_known_unread,
            "meaning": "Objects for which no reference was found in what was read: \
                        candidates to check before acting on them, never objects known to be \
                        unused. How far to trust them is in coverage (read_cleanly, consistent, \
                        anomalies, unread places, references by name).",
            "coverage": analysis.coverage(&kinds, limit),
        }))
    }

    pub(crate) fn script_calls(
        &mut self,
        params: &ScriptCallsParams,
    ) -> Result<Value, AuthorError> {
        let limit = limit(params.limit)?;
        let analysis = self.analysis()?;
        let object = ObjectParams {
            object: params.script.clone(),
            script_name: params.script_name.clone(),
            ..ObjectParams::default()
        };
        if params.script.is_some() == params.script_name.is_some() {
            return Err(AuthorError::new(
                ErrorKind::InvalidRequest,
                "Name the script with either `script` (a reference from inspect) or \
                 `script_name`.",
            ));
        }
        let ObjectId::Script(script) = analysis.resolve(&object)? else {
            return Err(AuthorError::new(
                ErrorKind::InvalidRequest,
                "`script` must be a script reference, as inspect gives it.",
            ));
        };
        if analysis.graph.object(ObjectId::Script(script)).is_none() {
            return Err(analysis.missing(
                Kind::Script,
                format!(
                    "Script {script} was not found among the objects read from {}.",
                    analysis.file
                ),
            ));
        }
        let chain = |direction: Direction| -> Value {
            let chain: CallChain<'_> = analysis.graph.call_chain(script, direction);
            let scripts: Vec<Value> = chain
                .scripts
                .iter()
                .map(|r| {
                    let mut out = analysis.short(ObjectId::Script(r.script));
                    out["depth"] = json!(r.depth);
                    out
                })
                .collect();
            let (performs, total, truncated) = listed(&chain.calls, limit, |c| {
                json!({
                    "from": analysis.short(ObjectId::Script(c.from)),
                    "to": analysis.short(ObjectId::Script(c.to)),
                    "place": analysis.place(c.place),
                })
            });
            let cycles: Vec<Vec<Value>> = chain
                .cycles
                .iter()
                .map(|cycle| {
                    cycle
                        .iter()
                        .map(|s| analysis.reference(ObjectId::Script(*s)))
                        .collect()
                })
                .collect();
            json!({
                "scripts": scripts,
                "performs": performs,
                "total_performs": total,
                "truncated": truncated,
                "cycles": cycles,
                "cycles_truncated": chain.cycles_truncated,
            })
        };
        let direction = params.direction.unwrap_or(CallDirection::Both);
        let mut out = json!({
            "file": analysis.file,
            "script": analysis.object(ObjectId::Script(script)),
            "nothing_known_unread": analysis.graph.coverage().nothing_known_unread_for(Kind::Script),
        });
        if matches!(direction, CallDirection::Calls | CallDirection::Both) {
            out["calls"] = chain(Direction::Calls);
        }
        if matches!(direction, CallDirection::Callers | CallDirection::Both) {
            out["callers"] = chain(Direction::Callers);
        }
        out["coverage"] = analysis.coverage(&[Kind::Script], limit);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use om_hbam::{KeySegment, KeyTree, Node, OwnedNode};

    use super::*;

    fn view(node: &OwnedNode) -> Node<'_> {
        Node {
            segment: KeySegment(&node.segment),
            value: node.value.as_deref(),
            push: node.push,
            children: node.children.iter().map(view).collect(),
        }
    }

    fn analysis(root: &OwnedNode) -> Analysis {
        let tree = KeyTree { root: view(root) };
        Analysis {
            graph: design_refs::build(&tree, Vec::new()),
            file: "Tasks.fmp12".into(),
        }
    }

    /// A file not read cleanly (here a script member list stored as a
    /// value): whether any object exists is not known, so a lookup that
    /// finds nothing is `not_known` with the reason, never `not_found`, for
    /// every kind, and an object the graph does not have has `exists:
    /// null`. A file read cleanly answers plainly.
    #[test]
    fn an_object_of_a_file_not_read_cleanly_is_not_known() {
        // The privilege set catalog, given and empty: nothing else is read.
        let mut clean = OwnedNode::default();
        clean.put(&[&[23], &[2], &[5]], OwnedNode::list(vec![5], Vec::new()));
        let mut not_clean = clean.clone();
        not_clean.put(&[&[17], &[5]], OwnedNode::leaf(Vec::new(), vec![0x55]));
        let not_clean = analysis(&not_clean);
        assert!(!not_clean.graph.coverage().read_cleanly());
        for params in [
            ObjectParams {
                script_name: Some("Post".into()),
                ..ObjectParams::default()
            },
            ObjectParams {
                layout_name: Some("Main".into()),
                ..ObjectParams::default()
            },
        ] {
            let error = not_clean.resolve(&params).unwrap_err();
            assert_eq!(error.kind, ErrorKind::NotKnown, "{error:?}");
            assert!(error.details["reason"].is_string());
        }
        let object = not_clean.object(ObjectId::Layout(7));
        assert_eq!(object["found"], Value::Null);
        assert!(object["not_known_because"].is_string());
        let clean = analysis(&clean);
        assert!(
            clean.graph.coverage().read_cleanly(),
            "{:?}",
            clean.graph.coverage().anomalies
        );
        let params = ObjectParams {
            layout_name: Some("Main".into()),
            ..ObjectParams::default()
        };
        assert_eq!(
            clean.resolve(&params).unwrap_err().kind,
            ErrorKind::NotFound
        );
        assert_eq!(clean.object(ObjectId::Layout(7))["found"], json!(false));
    }
}
