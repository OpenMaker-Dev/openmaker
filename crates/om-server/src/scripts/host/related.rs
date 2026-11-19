//! Related records in a server script session (file-metadata.md 4.3,
//! calc-values.md 8.7): the related sets calculations read (`Rel::Field`,
//! aggregates, `GetNthRecord`) and Go to Related Record shows, through the
//! file's relationship graph as `om_data::host` reads it, on the records
//! as the session sees them (its open record and transaction included).
//!
//! A hop whose predicates are all `=` (and `×`) is answered from the right
//! field's value index, built on demand and then stored as for a client's
//! join (query-program.md 7.1); other operators, and tables the session's
//! transaction changed, are matched by scanning the right table. A match
//! field on the side travelled from may be an unstored calculation, which is
//! evaluated on each record ([`super::calculated`]); on the other side it
//! never matches, as in FileMaker, where such a relationship is invalid.
//! The related set is in creation order unless the relationship sorts the
//! direction travelled (file-metadata.md 4.3 step 3): then in its sort
//! order, the order of a path being its last relationship's.

use std::collections::BTreeSet;
use std::sync::Arc;

use om_calc::ops::{self, CompareOp};
use om_calc::{CalcError, Ctx, Value, ValueType};
use om_data::host::{
    relationship_missing, CalcSchema, CalcSource, FieldClass, Hop, JoinOp, RecordValues, TableDef,
};
use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::{FieldKind, OwnedNode, RidList};
use om_scripts::runtime::{ErrorCode, HostResult, LayoutTarget, Mode, RelatedRecords};

use super::{values, ServerHost, Window, INDEXES, VALUE_INDEX};
use crate::modcount;

/// The session's records as calculation data (`om_data::host`).
pub(super) struct SessionSource<'a> {
    pub host: &'a ServerHost,
}

impl SessionSource<'_> {
    /// The values of `field` of `table` (shown by `occurrence`) on `record`
    /// that can match: every repetition but empty ones and `?`; an unstored
    /// or global calculation evaluated, a global field's session value.
    fn match_values(
        &self,
        table: &TableDef,
        occurrence: u64,
        record: u64,
        field: u64,
    ) -> Result<Vec<Value>, CalcError> {
        let Some(def) = table.fields.get(&field) else {
            return Ok(Vec::new());
        };
        let repetitions = 1..=def.repetitions.max(1);
        let values: Vec<Value> = if def.is_unstored_calculation() {
            repetitions
                .filter_map(|rep| self.host.read_field(occurrence, record, field, rep).ok())
                .collect()
        } else {
            let store = if def.global {
                Some(self.globals(table.id)?)
            } else {
                self.record(table.id, record)?
            };
            repetitions
                .filter_map(|rep| store.as_deref()?.get(field, rep).cloned())
                .collect()
        };
        Ok(values
            .into_iter()
            .filter(|v| !v.is_empty() && !v.is_invalid())
            .collect())
    }

    /// The records of `hop.to` related to `sources` of `hop.from` through
    /// the value index of each predicate's right field, when every
    /// predicate is `=` or `×`, the right fields are stored, and the
    /// session has not changed the right table's records; `None` otherwise.
    fn by_index(
        &self,
        schema: &CalcSchema,
        hop: &Hop,
        sources: &[u64],
        cx: &Ctx,
    ) -> Result<Option<Vec<u64>>, CalcError> {
        let (Some(from), Some(to)) = (schema.table_of(hop.from), schema.table_of(hop.to)) else {
            return Err(relationship_missing());
        };
        let host = self.host;
        let changed_here = host.pending_in(to.id).next().is_some()
            || host.record.as_ref().is_some_and(|r| r.table == to.id);
        let indexable = hop.predicates.iter().all(|p| {
            p.op == JoinOp::Cartesian
                || (p.op == JoinOp::Equal
                    && to.fields.get(&p.right).is_some_and(|f| {
                        f.class != FieldClass::Summary
                            && !f.global
                            && !f.is_unstored_calculation()
                            && f.value_type != ValueType::Container
                    }))
        });
        if changed_here || !indexable {
            return Ok(None);
        }
        let all: RidList = self.records(to.id)?.iter().copied().collect();
        let mut matched = RidList::new();
        for &record in sources {
            let mut these = all.clone();
            for predicate in &hop.predicates {
                if predicate.op == JoinOp::Cartesian {
                    continue;
                }
                let Some(right) = to.fields.get(&predicate.right) else {
                    these = RidList::new();
                    break;
                };
                // An empty match value never matches (file-metadata.md 4.3).
                let texts: Vec<String> = self
                    .match_values(from, hop.from, record, predicate.left)?
                    .iter()
                    .map(|v| v.to_text(cx).into_string())
                    .collect();
                let found = host.index_lookup(to.id, predicate.right, right.value_type, &texts);
                these = these.and(&found);
            }
            matched = matched.or(&these);
        }
        Ok(Some(matched.iter().collect()))
    }

    /// The records of `hop.to` related to `sources` of `hop.from` by
    /// comparing every record of its table as the session sees it: any
    /// operator, with the field types' comparison (file-metadata.md 4.3,
    /// `om_data::host::related_by_scan` for one hop). The right side reads
    /// stored values only.
    fn by_scan(
        &self,
        schema: &CalcSchema,
        hop: &Hop,
        sources: &[u64],
        cx: &Ctx,
    ) -> Result<Vec<u64>, CalcError> {
        let (Some(from), Some(to)) = (schema.table_of(hop.from), schema.table_of(hop.to)) else {
            return Err(relationship_missing());
        };
        let keys = sources
            .iter()
            .map(|&record| {
                hop.predicates
                    .iter()
                    .map(|p| self.match_values(from, hop.from, record, p.left))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for &candidate in self.records(to.id)?.iter() {
            let theirs = hop
                .predicates
                .iter()
                .map(|p| match to.fields.get(&p.right) {
                    Some(def) if def.is_unstored_calculation() => Ok(Vec::new()),
                    _ => self.match_values(to, hop.to, candidate, p.right),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let matches = keys.iter().any(|ours| {
                hop.predicates.iter().enumerate().all(|(i, p)| {
                    let op = match p.op {
                        JoinOp::Cartesian => return true,
                        JoinOp::Equal => CompareOp::Eq,
                        JoinOp::NotEqual => CompareOp::Ne,
                        JoinOp::Less => CompareOp::Lt,
                        JoinOp::LessEqual => CompareOp::Le,
                        JoinOp::Greater => CompareOp::Gt,
                        JoinOp::GreaterEqual => CompareOp::Ge,
                    };
                    ours[i].iter().any(|a| {
                        theirs[i]
                            .iter()
                            .any(|b| ops::compare_op(a, op, b, cx).is_true(cx))
                    })
                })
            });
            if matches {
                out.push(candidate);
            }
        }
        Ok(out)
    }
}

impl CalcSource for SessionSource<'_> {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        Ok(self.host.record_values(table, record).map(Arc::new))
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        Ok(self.host.all_records(table).into())
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        let mut values = RecordValues::default();
        for ((t, field, repetition), value) in &self.host.globals {
            if *t == table {
                values.set(*field, *repetition, value.clone());
            }
        }
        Ok(Arc::new(values))
    }

    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        let path = schema.path(from, to).ok_or_else(relationship_missing)?;
        let mut records = vec![record];
        for hop in &path {
            records = match self.by_index(schema, hop, &records, cx)? {
                Some(found) => found,
                None => self.by_scan(schema, hop, &records, cx)?,
            };
            if records.is_empty() {
                break;
            }
        }
        Ok(records.into())
    }

    fn interrupted(&self) -> bool {
        self.host.aborted()
    }
}

impl ServerHost {
    /// The file's calculation schema (occurrences, relationships, custom
    /// functions), when its catalogs could be read.
    pub(super) fn calc_schema(&self) -> Option<&CalcSchema> {
        self.schema.rules.as_ref().map(|(_, calc)| &**calc)
    }

    /// A record's stored values for calculations, typed by its table's
    /// fields; repetition n of field F is the cell `F * 256 + n`.
    pub(super) fn record_values(&self, table: u64, record: u64) -> Option<RecordValues> {
        let definition = self.schema.catalog.table(table)?;
        let cx = self.cx();
        self.with_record(table, record, |node| {
            let node = node?;
            let mut values = RecordValues {
                mod_count: Some(modcount::serial_value(node.child(&[modcount::MOD_COUNT]))),
                ..RecordValues::default()
            };
            for cell in &node.children {
                let Some(id) = be_uint(&cell.segment) else {
                    continue;
                };
                let (field, repetition) = match definition.field(id) {
                    Some(field) => (field, 1),
                    None => match definition.field(id >> 8) {
                        Some(field) if id & 0xff >= 2 => (field, (id & 0xff) as u32),
                        _ => continue,
                    },
                };
                if field.kind == FieldKind::Summary {
                    continue;
                }
                let ty = values::value_type(field.data_type);
                values.set(field.id, repetition, values::decode(ty, Some(cell), &cx));
            }
            Some(values)
        })
    }

    /// The records of `table` whose value index for `field` holds a key of
    /// one of `texts` (each keyed as a value of `value_type`).
    fn index_lookup(
        &self,
        table: u64,
        field: u64,
        value_type: ValueType,
        texts: &[String],
    ) -> RidList {
        let data_type = match value_type {
            ValueType::Number => om_hbam::DataType::Number,
            ValueType::Date => om_hbam::DataType::Date,
            ValueType::Time => om_hbam::DataType::Time,
            ValueType::Timestamp => om_hbam::DataType::Timestamp,
            _ => om_hbam::DataType::Text,
        };
        let (t, f) = (uint_segment(table), uint_segment(field));
        let mut found = RidList::new();
        let built = {
            let store = self.hosted.read_store();
            let keys: BTreeSet<Vec<u8>> = texts
                .iter()
                .flat_map(|text| store.value_keys(&t, &f, data_type, text))
                .collect();
            let stored = store.node(&[&t, &[INDEXES], &f, &[VALUE_INDEX]]);
            let on_demand;
            let index: Option<&OwnedNode> = match stored {
                Some(index) => Some(index),
                None => {
                    on_demand = om_query::Source::index_on_demand(&*store, &t, &f, VALUE_INDEX)
                        .ok()
                        .flatten();
                    on_demand.as_ref()
                }
            };
            for key in &keys {
                if let Some(ids) = index
                    .and_then(|i| i.child(key))
                    .and_then(|n| RidList::from_owned(n).ok())
                {
                    found = found.or(&ids);
                }
            }
            store.has_indexes_on_demand()
        };
        if built {
            self.hosted.write_store().store_indexes_on_demand();
            self.hosted.save();
        }
        found
    }

    /// The records of `related` related to `record` of `occurrence`, in
    /// the related set's order (calc-values.md 8.7, file-metadata.md 4.3).
    pub(super) fn related_set(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        let schema = self.calc_schema().ok_or(CalcError::NOT_SUPPORTED)?;
        let cx = self.cx();
        let source = SessionSource { host: self };
        let mut records = source
            .related(schema, occurrence, record, related, &cx)?
            .to_vec();
        // The order of a path is its last relationship's, when that sorts
        // the direction travelled (the shared relationship decode,
        // `om_data::meta`).
        let sort = schema
            .path(occurrence, related)
            .and_then(|path| path.last().and_then(|hop| hop.sort.clone()))
            .filter(|spec| !spec.fields.is_empty());
        if let Some(spec) = sort {
            records = self.sorted(related, records, &spec).map_err(|e| {
                CalcError::from_code(e.0 as u16).unwrap_or(CalcError::NOT_SUPPORTED)
            })?;
        }
        Ok(records)
    }

    /// Go to Related Record (script-steps.md 11.8): the records related to
    /// the current record, or to every record of the found set, on the
    /// layout given (or the first layout of the related table), as the
    /// found set or as the current record of all records. The server has
    /// one window: "show in new window" uses it.
    pub(super) fn go_to_related(&mut self, request: &RelatedRecords<'_>) -> HostResult {
        let related = request.occurrence.ok_or(ErrorCode::INVALID)?;
        self.require_privileges()?;
        self.leave()?;
        let window = self.window()?;
        let from = window.occurrence;
        let sources: Vec<u64> = if request.match_found_set {
            window.found.clone()
        } else {
            window.record().into_iter().collect()
        };
        let mut ids = Vec::new();
        let mut seen = BTreeSet::new();
        for record in sources {
            let found = self
                .related_set(from, record, related)
                .map_err(|e| ErrorCode(e.code()))?;
            ids.extend(found.into_iter().filter(|id| seen.insert(*id)));
        }
        if ids.is_empty() {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        let table = self.schema.table_of(related).ok_or(super::TABLE_MISSING)?;
        let layouts = &self.schema.layouts;
        let index = match &request.layout {
            Some(LayoutTarget::Key(key)) => layouts.iter().position(|l| l.id == *key),
            _ => layouts.iter().position(|l| {
                l.occurrence
                    .and_then(|o| self.schema.table_of(o))
                    .is_some_and(|t| t == table)
            }),
        }
        .ok_or(ErrorCode::LAYOUT_MISSING)?;
        let mut next: Window = self.window_on(index).ok_or(ErrorCode::LAYOUT_MISSING)?;
        if next.table != table {
            return Err(super::TABLE_MISSING.into());
        }
        next.mode = Mode::Browse;
        if request.show_only_related {
            next.found = ids;
            next.current = 0;
        } else {
            next.current = next.found.iter().position(|id| *id == ids[0]).unwrap_or(0);
        }
        self.requests.clear();
        self.window = Some(next);
        Ok(())
    }
}
