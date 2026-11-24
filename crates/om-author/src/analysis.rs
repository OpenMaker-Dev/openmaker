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
    self, CallChain, DesignGraph, Direction, Kind, ObjectId, Place, Resolution, Target, Use,
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
            "exists": found.is_some() || id == ObjectId::File,
        });
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
            Resolution::Broken { missing } => {
                out["resolution"] = json!("broken");
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
        json!({
            "kinds": Self::kinds(kinds),
            "complete": kinds.iter().all(|k| coverage.complete_for(*k)),
            "read": coverage.read,
            "unread": {
                "total": unread_total,
                "truncated": unread_truncated,
                "places": unread_listed,
            },
            "by_name": {
                "total": by_name_total,
                "truncated": by_name_truncated,
                "places": by_name_listed,
            },
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
            Lookup::Missing => Err(AuthorError::new(
                ErrorKind::NotFound,
                format!("{} has no {what} named “{name}”.", self.file),
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
                return self.named(what, name, self.of_kind(kind));
            }
        }
        let table = match (&params.table, &params.table_name) {
            (Some(text), None) => ObjectId::Table(refs::table_id(text, &self.file)?),
            (None, Some(name)) => self.named("table", name, self.of_kind(Kind::Table))?,
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
        self.named("field", name, fields)
    }
}

impl Service {
    /// The reference graph of the file's design, read now.
    fn analysis(&self) -> Result<Analysis, AuthorError> {
        let file = self.file();
        let mut opened = self.open()?;
        let catalogs = opened.design_catalogs().map_err(Self::read_error)?;
        let tree = catalogs.tree().map_err(Self::read_error)?;
        let graph = design_refs::build(&tree, catalogs.unavailable.clone());
        Ok(Analysis { graph, file })
    }

    pub(crate) fn find_uses(&mut self, params: &ObjectParams) -> Result<Value, AuthorError> {
        let limit = limit(params.limit)?;
        let analysis = self.analysis()?;
        let id = analysis.resolve(params)?;
        let graph = &analysis.graph;
        if graph.object(id).is_none() && params.object.is_none() {
            return Err(AuthorError::new(
                ErrorKind::NotFound,
                format!("{} has no such object.", analysis.file),
            ));
        }
        // An object the file no longer has: the references still naming it.
        let uses: Vec<&Use> = graph
            .uses()
            .iter()
            .filter(|u| {
                u.holder != id
                    && (u.uses(id)
                        || matches!(u.resolution, Resolution::Broken { missing } if missing == id))
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
            "complete": graph.coverage().complete_for(kind),
            "coverage": analysis.coverage(&[kind], limit),
        }))
    }

    pub(crate) fn broken_references(
        &mut self,
        params: &AnalysisParams,
    ) -> Result<Value, AuthorError> {
        let limit = limit(params.limit)?;
        let analysis = self.analysis()?;
        let broken = analysis.graph.broken();
        let (listed, total, truncated) = listed(&broken, limit, |u| analysis.reference_use(u));
        let kinds = Kind::REFERABLE;
        Ok(json!({
            "file": analysis.file,
            "broken": listed,
            "total": total,
            "truncated": truncated,
            "complete": kinds.iter().all(|k| analysis.graph.coverage().complete_for(*k)),
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
        let mut findings = serde_json::Map::new();
        for kind in &kinds {
            let unused = analysis.graph.unused(*kind);
            let finding = if analysis.graph.coverage().complete_for(*kind) {
                "unreferenced"
            } else {
                "no_decoded_reference"
            };
            findings.insert(kind.name().into(), json!(finding));
            objects.extend(
                unused
                    .into_iter()
                    .map(|(object, finding)| (object.id, finding)),
            );
        }
        let (listed, total, truncated) = listed(&objects, limit, |(id, finding)| {
            let mut out = analysis.object(*id);
            out["finding"] = json!(finding.code());
            out
        });
        Ok(json!({
            "file": analysis.file,
            "kinds": Analysis::kinds(&kinds),
            "objects": listed,
            "total": total,
            "truncated": truncated,
            "findings": findings,
            "meaning": {
                "unreferenced": "No reference in the file names it: every place that could \
                                 was read.",
                "no_decoded_reference": "No reference OpenMaker decodes names it, but some \
                                         place that could was not read (coverage): it may \
                                         still be used.",
            },
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
            return Err(AuthorError::new(
                ErrorKind::NotFound,
                format!("{} has no script {script}.", analysis.file),
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
            "complete": analysis.graph.coverage().complete_for(Kind::Script),
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
