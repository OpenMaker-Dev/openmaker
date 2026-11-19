//! Found sets of the hosted file's tables (`om_data::found`). A find is one
//! op86 SimpleFind per request (query-program.md 6.5), with the finders
//! FileMaker Pro builds from Find mode criteria (6.4, `om_query::find`);
//! criteria the finders cannot express exactly (wildcards, phrases, `?`,
//! partial dates, ...) are evaluated on the client over the candidates the
//! host returns, as FileMaker Pro finds locally (architecture.md 7), and so
//! are criteria on unstored calculations, which have no index: the
//! calculation is computed on each candidate (script-runtime.md 13.19).
//! Criteria on summary and global fields are not criteria. A find on an
//! index the host does not have yet finds nothing (7.1), so each program
//! checks the indexes it reads first; when one was missing, the client
//! generates each record's keys from stored values and answers the same
//! finders or join while streaming the records. The requests and
//! Constrain/Extend combine on the client, as the RID-list algebra would
//! on the host (8.1). A sort keys
//! each found record's stored value of each sort field with the index key
//! forms (10.3) on the client.

use std::collections::{BTreeMap, BTreeSet};

use om_collate::DateOrder;
use om_data::calc::Ctx;
use om_data::found::{
    self, code, FindKind, FindRequest, Findable, FoundSet, FoundSetChange, SortField,
};
use om_data::host::{CalcSource, FileCalc, Hop};
use om_data::{DataError, Record};
use om_hbam::{RidList, Table};
use om_query::find::{
    self as finders, ClientFind, ClientFindError, Criterion, FieldIndex, FindContext, IndexKind,
};
use om_query::keys::{record_value_keys, word_keys, Reading, TextKeys};

use crate::calc::NativeSource;
use crate::{download_records, download_tree, failure, record_ids, NativeProvider};

/// Records downloaded per request while sorting.
const SORT_BATCH: usize = 500;

/// One table's found set. `ids` is in display order (sorted, when `sort`
/// is not empty); `None` is every record in creation order.
#[derive(Clone, Debug, Default)]
pub(crate) struct TableFound {
    ids: Option<Vec<u64>>,
    sort: Vec<SortField>,
}

impl TableFound {
    /// Record `old` is record `new` now, in its place: a new record held
    /// again under another id after a reconnect (#152).
    pub(crate) fn renumber(&mut self, old: u64, new: u64) {
        for id in self.ids.iter_mut().flatten() {
            if *id == old {
                *id = new;
            }
        }
    }
}

/// The found sets by base table.
pub(crate) type FoundSets = BTreeMap<u64, TableFound>;

/// The found sets a local file was last closed with (file-metadata.md 11),
/// applied once per window and table after the file opens; keyed by
/// window and base table, as the windows' found sets are.
#[derive(Clone, Debug, Default)]
pub(crate) struct SavedFound {
    /// Tables whose saved found set each window looked for.
    consulted: BTreeSet<(u32, u64)>,
    /// The record each restored table was closed on, until the window
    /// asks for it.
    current: BTreeMap<(u32, u64), u64>,
    /// The windows that changed the file (wrote a record, or had the host
    /// build an index for a find): only they keep their found set when
    /// they close ([`Self::keep_window_found_set`]).
    changed: BTreeSet<u32>,
}

impl NativeProvider {
    /// The names of the fields `table`'s found set is sorted by, in
    /// priority order and as requested (`Occurrence::Field` or a bare
    /// field name); empty when unsorted.
    pub(crate) fn found_sort(&self, table: u64) -> Vec<String> {
        self.found
            .get(&table)
            .map(|f| f.sort.iter().map(|s| s.field.clone()).collect())
            .unwrap_or_default()
    }

    /// The found set of `table` in display order, without records deleted
    /// since, and every record of the table.
    pub(crate) fn found_ids(&mut self, table: u64) -> Result<(Vec<u64>, RidList), DataError> {
        let all = self.with_read_table(table, record_ids)?;
        let all = self.with_held(table, all);
        let all = self.with_routed_held(table, all);
        let found = match self.found.get(&table).and_then(|f| f.ids.as_ref()) {
            Some(ids) => ids.iter().copied().filter(|id| all.contains(*id)).collect(),
            None => all.iter().collect(),
        };
        Ok((found, all))
    }

    /// The first time a window of a local file reads `table` through a
    /// layout, the window starts on the found set, sort and current record
    /// FileMaker Pro saved for the layout's occurrence when the file was
    /// last closed (file-metadata.md 11), without records deleted since. A
    /// hosted file, a related file's table, a window that already has a
    /// found set of the table, and a saved found set that does not decode
    /// keep every record. FileMaker keeps a found set per occurrence; this
    /// provider keeps one per base table, so the occurrence the table is
    /// first read through decides.
    pub(crate) fn restore_saved_found(&mut self, layout: &om_layout::Layout, table: &Table) {
        // Not through a layout the account may not see: the saved records
        // (a find's) and their order (a sort's) follow their values (#696).
        if !self.shows_layout(layout.id) {
            return;
        }
        if self.local.is_none()
            || self.external.route(table.id).is_some()
            || !self.saved_found.consulted.insert((self.window, table.id))
            || self.found.contains_key(&table.id)
        {
            return;
        }
        let Ok(occurrence) = crate::layout_occurrence(layout) else {
            return;
        };
        let Some(saved) = self.read_saved_found(occurrence) else {
            return;
        };
        let Ok((_, all)) = self.found_ids(table.id) else {
            return;
        };
        let ids: Vec<u64> = if saved.all {
            all.iter().collect()
        } else {
            saved
                .records
                .iter()
                .filter(|id| all.contains(*id))
                .collect()
        };
        let sort = saved
            .sort
            .as_ref()
            .filter(|_| saved.sorted)
            .and_then(|spec| self.saved_sort(occurrence, table, spec));
        // A sort the provider cannot apply leaves the records unsorted.
        let sorted = sort.and_then(|sort| Some((self.sorted(table, &ids, &sort).ok()?, sort)));
        let state = match sorted {
            Some((ids, sort)) => TableFound {
                ids: Some(ids),
                sort,
            },
            None => TableFound {
                ids: (!saved.all).then(|| ids.clone()),
                sort: Vec::new(),
            },
        };
        self.found.insert(table.id, state);
        if let Some(current) = saved.current.filter(|id| ids.contains(id)) {
            self.saved_found
                .current
                .insert((self.window, table.id), current);
        }
    }

    /// The record `table`'s saved found set was closed on, once, in this
    /// window ([`Self::restore_saved_found`]).
    pub(crate) fn take_saved_current(&mut self, table: u64) -> Option<u64> {
        self.saved_found.current.remove(&(self.window, table))
    }

    /// A window copied from `from` has looked at what `from` has, and
    /// starts on the same record of a table `from` has not shown yet.
    pub(crate) fn copy_saved_found(&mut self, from: u32, to: u32) {
        let saved = &mut self.saved_found;
        saved.consulted.retain(|(window, _)| *window != to);
        saved.current.retain(|(window, _), _| *window != to);
        let consulted: Vec<_> = saved
            .consulted
            .iter()
            .filter(|(w, _)| *w == from)
            .copied()
            .collect();
        saved
            .consulted
            .extend(consulted.into_iter().map(|(_, table)| (to, table)));
        let current: Vec<_> = saved
            .current
            .iter()
            .filter(|((w, _), _)| *w == from)
            .map(|((_, table), record)| (*table, *record))
            .collect();
        saved.current.extend(
            current
                .into_iter()
                .map(|(table, record)| ((to, table), record)),
        );
    }

    /// A closed window's bookkeeping goes with it.
    pub(crate) fn drop_saved_found(&mut self, window: u32) {
        self.saved_found.consulted.retain(|(w, _)| *w != window);
        self.saved_found.current.retain(|(w, _), _| *w != window);
        self.saved_found.changed.remove(&window);
    }

    /// Whether the local host now has one of `table`'s indexes in
    /// `missing`, which a find just reported missing: the host built and
    /// stored it. A field that cannot be indexed stays without one, and the
    /// file has not changed. A hosted file is not asked.
    fn built_any(&mut self, table: u64, missing: &[FieldIndex]) -> Result<bool, DataError> {
        if self.local.is_none() {
            return Ok(false);
        }
        for (field, kind) in missing {
            let counts = match kind {
                IndexKind::Value => 0x10,
                IndexKind::Word => 0x11,
            };
            let path = vec![
                crate::key_bytes(table),
                vec![0x01],
                crate::key_bytes(*field),
                vec![counts],
            ];
            if crate::key_exists(self.session_mut()?, path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The window changed `table`'s file: it wrote a record of `table`, or
    /// its find had the host build one of `table`'s indexes. When the table
    /// is the local file's own, the window keeps its found set as it
    /// closes.
    pub(crate) fn saw_own_write(&mut self, table: u64) {
        if self.local.is_some() && self.external.route(table).is_none() {
            self.saved_found.changed.insert(self.window);
        }
    }

    /// The window closes on the layout's table with `record` current: when
    /// it changed the local file ([`Self::saw_own_write`]), the file keeps its found set
    /// of the table and that record at the layout's occurrence
    /// (file-metadata.md 11), as FileMaker Pro does; a window that wrote
    /// nothing leaves what another window kept. Every record found is kept
    /// as every record. Only a local file's own table is written, and
    /// anything that stops the write leaves the file as it was.
    pub(crate) fn keep_window_found_set(
        &mut self,
        layout: &om_layout::Layout,
        table: &Table,
        record: Option<i64>,
    ) {
        if !self.saved_found.changed.contains(&self.window)
            || self.external.route(table.id).is_some()
        {
            return;
        }
        let Ok(occurrence) = crate::layout_occurrence(layout) else {
            return;
        };
        let Ok((found, all)) = self.found_ids(table.id) else {
            return;
        };
        let records = (found.len() != all.len()).then(|| {
            let mut records = RidList::new();
            for id in &found {
                records.insert(*id);
            }
            records
        });
        let current = record
            .and_then(|r| u64::try_from(r).ok())
            .filter(|r| found.contains(r));
        if let Some(host) = self.local.as_ref() {
            host.save_found_set(occurrence, table.id, records, current);
        }
    }

    /// `table`'s found set's size and sort, from its records in display
    /// order and every record of the table ([`Self::found_ids`]).
    pub(crate) fn summary_of(&self, table: u64, found: &[u64], all: &RidList) -> FoundSet {
        FoundSet {
            found: found.len() as u64,
            total: all.len() as u64,
            sorted: !self.found_sort(table).is_empty(),
        }
    }

    /// Forget what the previous file's saved found sets restored.
    pub(crate) fn reset_saved_found(&mut self) {
        self.saved_found = SavedFound::default();
    }

    /// `[occurrence, 07, 01]`, when the file has one that decodes.
    fn read_saved_found(&mut self, occurrence: u64) -> Option<om_data::meta::SavedFoundSet> {
        let mut path = vec![crate::key_bytes(occurrence)];
        path.extend(om_data::meta::SAVED_FOUND_SET.iter().map(|k| vec![*k]));
        let data = crate::download_node(self.session_mut().ok()?, path).ok()??;
        let tree = download_tree(std::slice::from_ref(&data)).ok()?;
        let refs: Vec<&[u8]> = data.path.iter().map(Vec::as_slice).collect();
        om_data::meta::SavedFoundSet::from_node(tree.node(&refs)?)
    }

    /// A saved sort as this provider sorts: fields of `table` reached
    /// through `occurrence` itself, by value or by a value list's order.
    /// `None` for anything else (a related field, a summary order, a
    /// repetition), which leaves the found set unsorted.
    fn saved_sort(
        &mut self,
        occurrence: u64,
        table: &Table,
        spec: &om_data::specs::SortSpec,
    ) -> Option<Vec<SortField>> {
        let mut sort = Vec::with_capacity(spec.fields.len());
        for entry in &spec.fields {
            let field = &entry.field;
            if entry.summary.is_some()
                || field.repetition > 1
                || field.occurrence.is_some_and(|o| o != occurrence)
            {
                return None;
            }
            let value_list = match entry.value_list {
                Some(id) => Some(
                    self.read_value_list(id)
                        .ok()?
                        .into_iter()
                        .filter(|v| v.trim() != "-")
                        .collect(),
                ),
                None => None,
            };
            sort.push(SortField {
                field: table.field(field.field)?.name.clone(),
                order: if entry.descending {
                    found::SortOrder::Descending
                } else {
                    found::SortOrder::Ascending
                },
                blanks_last: spec.blanks_last,
                value_list,
                language: (entry.locale != 0).then_some(entry.locale),
                summary: None,
            });
        }
        (!sort.is_empty()).then_some(sort)
    }

    /// A record created in the window joins an explicit found set at its
    /// end.
    pub(crate) fn found_add(&mut self, table: u64, record: u64) {
        if let Some(ids) = self.found.get_mut(&table).and_then(|f| f.ids.as_mut()) {
            if !ids.contains(&record) {
                ids.push(record);
            }
        }
    }

    pub(crate) fn change_found(
        &mut self,
        layout_id: u32,
        change: FoundSetChange,
    ) -> Result<FoundSet, DataError> {
        // Also through a layout the account may not see: the found set is
        // ids, no value (#696). A change whose result follows the records'
        // values (a find, a sort) is refused there (3) [I: unmeasured],
        // as its ids and order would tell those values.
        if !self.shows_layout(u64::from(layout_id))
            && matches!(
                change,
                FoundSetChange::Find { .. }
                    | FoundSetChange::ManualFind { .. }
                    | FoundSetChange::QuickFind(_)
                    | FoundSetChange::Sort(_)
            )
        {
            return Err(DataError::with_code(
                3,
                "A find or sort through a layout this account may not see is not supported.",
            ));
        }
        let layout = self.decoded_layout_for_found_set(layout_id)?;
        let occurrence = crate::layout_occurrence(&layout)?;
        // Reading the table asks for what the layout's occurrence waits for
        // (`grow_related`): its file, or its table's fields (#772).
        let table = self.read_table_for_layout(&layout)?;
        let schema = self.calc_schema_cached()?;
        // A table whose fields could still not be read would make every
        // criterion and sort key read as a missing field (102): refused
        // instead, the found set unchanged.
        if schema.are_fields_pending(table.id) {
            return Err(DataError::with_code(
                code::UNSUPPORTED,
                "The fields of the layout's table in its related file could not be read. The \
                 found set is unchanged.",
            ));
        }
        // Validate every key in the caller's occurrence namespace before
        // reading records or changing its found set. The owner receives only
        // unqualified base-field names, never caller occurrences or table ids.
        let imported_sort = match &change {
            FoundSetChange::Sort(sort) if schema.table_context(table.id).is_some() => {
                let base = schema.occurrence(occurrence).ok_or_else(|| {
                    DataError::with_code(code::FIELD_MISSING, "The layout's occurrence is missing.")
                })?;
                Some(imported_sort_fields(&table, &base.name, sort)?)
            }
            _ => None,
        };
        self.restore_saved_found(&layout, &table);
        let (current, all) = self.found_ids(table.id)?;
        let manual = matches!(change, FoundSetChange::ManualFind { .. });
        let mut state = self.found.get(&table.id).cloned().unwrap_or_default();
        let mut no_match = false;
        match change {
            FoundSetChange::ShowAll => state = TableFound::default(),
            FoundSetChange::Unsort => {
                // Creation order of the same records.
                if state.ids.is_some() {
                    state.ids = Some(all.iter().filter(|id| current.contains(id)).collect());
                }
                state.sort.clear();
            }
            FoundSetChange::Sort(sort) => {
                let ids = if let Some(translated) = imported_sort {
                    let route = self.external.route(table.id).ok_or_else(|| {
                        DataError::with_code(code::UNSUPPORTED, "The related file is unavailable.")
                    })?;
                    self.external.with_owner(route, |owner, wire| {
                        let native = owner.table_def(wire)?;
                        owner.sorted(&native, &current, &translated)
                    })?
                } else {
                    self.sorted(&table, &current, &sort)?
                };
                state = TableFound {
                    ids: Some(ids),
                    sort,
                };
            }
            FoundSetChange::Omit(records) => {
                state.ids = Some(
                    current
                        .into_iter()
                        .filter(|id| !records.iter().any(|r| *r as u64 == *id))
                        .collect(),
                );
            }
            FoundSetChange::ShowOmitted => {
                state = TableFound {
                    ids: Some(all.iter().filter(|id| !current.contains(id)).collect()),
                    sort: Vec::new(),
                };
            }
            FoundSetChange::Records(records) => {
                state = TableFound {
                    ids: Some(
                        records
                            .into_iter()
                            .filter_map(|id| u64::try_from(id).ok())
                            .filter(|id| all.contains(*id))
                            .collect(),
                    ),
                    sort: Vec::new(),
                };
            }
            FoundSetChange::Find { requests, kind }
            | FoundSetChange::ManualFind { requests, kind } => {
                let matched = if schema.table_context(table.id).is_some() {
                    self.find_imported(&table, occurrence, &all, &requests)?
                } else {
                    self.find(&table, occurrence, &all, &requests)?
                };
                let ids: Vec<u64> = match kind {
                    FindKind::Replace => matched.iter().collect(),
                    FindKind::Constrain => current
                        .iter()
                        .copied()
                        .filter(|id| matched.contains(*id))
                        .collect(),
                    FindKind::Extend => all
                        .iter()
                        .filter(|id| current.contains(id) || matched.contains(*id))
                        .collect(),
                };
                // A find from the window keeps the found set (and its
                // sort) when nothing matches.
                if manual && ids.is_empty() {
                    return Err(found::no_records());
                }
                no_match = ids.is_empty();
                state = TableFound {
                    ids: Some(ids),
                    sort: Vec::new(),
                };
            }
            FoundSetChange::QuickFind(groups) => {
                // Each word's requests, ORed; the words ANDed. A word no
                // field can read finds nothing.
                let mut matched = all.clone();
                for group in &groups {
                    if group.is_empty() {
                        matched = RidList::new();
                        break;
                    }
                    let found = if schema.table_context(table.id).is_some() {
                        self.find_imported(&table, occurrence, &all, group)?
                    } else {
                        self.find(&table, occurrence, &all, group)?
                    };
                    matched = matched.and(&found);
                }
                // No match keeps the found set and its sort (query-program.md 6.6).
                if groups.is_empty() || matched.is_empty() {
                    return Err(found::no_records());
                }
                state = TableFound {
                    ids: Some(matched.iter().collect()),
                    sort: Vec::new(),
                };
            }
        }
        let found = state.ids.as_ref().map_or(all.len(), Vec::len) as u64;
        let sorted = !state.sort.is_empty();
        self.calc.page = om_data::host::CalcData::default();
        self.found.insert(table.id, state);
        if no_match {
            return Err(DataError::with_code(
                code::NO_MATCH,
                "No records match this find criteria.",
            ));
        }
        Ok(FoundSet {
            found,
            total: all.len() as u64,
            sorted,
        })
    }

    /// Stored base-field requests need no relationship or calculation context
    /// translation. Plan and execute them with the owner's catalog, locale,
    /// indexes and connection. Only the caller installs the resulting ids, in
    /// its imported table namespace and active window's found set.
    fn find_imported(
        &mut self,
        table: &Table,
        occurrence: u64,
        all: &RidList,
        requests: &[FindRequest],
    ) -> Result<RidList, DataError> {
        let schema = self.calc_schema_cached()?;
        let base = schema.occurrence(occurrence).ok_or_else(|| {
            DataError::with_code(code::FIELD_MISSING, "The layout's occurrence is missing.")
        })?;
        let unsupported = || {
            DataError::with_code(
                code::UNSUPPORTED,
                "Related-file finds support stored scalar base fields only; relationship, \
                 repeating and evaluated calculation criteria are not supported yet.",
            )
        };
        // Check every request before the first find, including later omit
        // requests. Never interpret a caller occurrence in the peer's graph,
        // even if its numeric id or name happens to collide there.
        let mut translated = Vec::with_capacity(requests.len());
        for request in requests {
            let mut criteria = Vec::with_capacity(request.criteria.len());
            for (name, text) in &request.criteria {
                if text.trim().is_empty() {
                    continue;
                }
                let name = match name.split_once("::") {
                    Some((qualifier, field)) if qualifier == base.name => field,
                    Some(_) => return Err(unsupported()),
                    None => name,
                };
                let field = table.field_by_name(name).ok_or_else(|| {
                    DataError::with_code(
                        code::FIELD_MISSING,
                        format!("The table has no field named {name:?}."),
                    )
                })?;
                match found::findable(field) {
                    Findable::Ignored => continue,
                    Findable::Evaluated => return Err(unsupported()),
                    Findable::Indexed => {}
                }
                if !matches!(
                    field.data_type,
                    om_hbam::DataType::Text
                        | om_hbam::DataType::Number
                        | om_hbam::DataType::Date
                        | om_hbam::DataType::Time
                        | om_hbam::DataType::Timestamp
                ) || !field
                    .decoded_options()
                    .is_some_and(|options| options.max_repetitions == 1)
                {
                    return Err(unsupported());
                }
                criteria.push((name.to_owned(), text.clone()));
            }
            translated.push(FindRequest {
                omit: request.omit,
                criteria,
            });
        }
        let route = self.external.route(table.id).ok_or_else(|| {
            DataError::with_code(code::UNSUPPORTED, "The related file is unavailable.")
        })?;
        self.external.with_owner(route, |owner, wire| {
            let native = owner.table_def(wire)?;
            let occurrence = owner
                .catalog
                .table_occurrences
                .iter()
                .find(|o| o.reference() == Some((0, wire)))
                .ok_or_else(unsupported)?
                .id;
            owner.find(&native, occurrence, all, &translated)
        })
    }

    /// The records of `table` the requests select (find requests add,
    /// omit requests remove; a first omit request starts from every
    /// record).
    fn find(
        &mut self,
        table: &Table,
        occurrence: u64,
        all: &RidList,
        requests: &[FindRequest],
    ) -> Result<RidList, DataError> {
        // Criteria by occurrence, per request: the layout's (`None`) and
        // related ones (`Occurrence::Field`), each solved in its own table;
        // those on summary and global fields left out (13.19). Every
        // related occurrence and its path are checked here, before any
        // request runs a query (`local_path`).
        let mut grouped = Vec::with_capacity(requests.len());
        let mut related_tables: BTreeMap<String, (u64, Table, Vec<Hop>)> = BTreeMap::new();
        for request in requests {
            let mut groups: BTreeMap<Option<String>, Vec<(&str, &str)>> = BTreeMap::new();
            for (name, text) in &request.criteria {
                if text.trim().is_empty() {
                    continue;
                }
                let (related, field) = match name.split_once("::") {
                    Some((related, field)) => (Some(related.to_owned()), field),
                    None => (None, name.as_str()),
                };
                let ignored = match &related {
                    None => table.field_by_name(field).map(found::findable),
                    Some(name) => {
                        if !related_tables.contains_key(name) {
                            let (related_occurrence, related_table) = self.related(name)?;
                            let path = self.local_path(occurrence, related_occurrence)?;
                            related_tables
                                .insert(name.clone(), (related_occurrence, related_table, path));
                        }
                        related_tables[name]
                            .1
                            .field_by_name(field)
                            .map(found::findable)
                    }
                } == Some(Findable::Ignored);
                if !ignored {
                    groups.entry(related).or_default().push((field, text));
                }
            }
            grouped.push((request.omit, groups));
        }
        if grouped.iter().all(|(_, groups)| groups.is_empty()) {
            return Err(DataError::with_code(
                code::EMPTY_CRITERIA,
                "Provide at least one criterion to find records.",
            ));
        }
        let context = FindContext::new(
            DateOrder::MonthDayYear,
            Some(found::today(crate::local_utc_offset())),
        );
        let mut found = if requests[0].omit {
            all.clone()
        } else {
            RidList::new()
        };
        for (omit, groups) in grouped {
            let mut matched = all.clone();
            for (related, criteria) in groups {
                match related {
                    None => {
                        let found = self.find_in(table, occurrence, all, &criteria, &context)?;
                        matched = matched.and(&found);
                    }
                    Some(name) => {
                        let (related_occurrence, related_table, path) = &related_tables[&name];
                        let (related_occurrence, related_table) =
                            (*related_occurrence, related_table.clone());
                        let records = record_ids(self.session_mut()?, related_table.id)?;
                        let records = self.with_held(related_table.id, records);
                        let found = self.find_in(
                            &related_table,
                            related_occurrence,
                            &records,
                            &criteria,
                            &context,
                        )?;
                        let joined = self.join_back(path, found)?;
                        matched = matched.and(&joined);
                    }
                }
            }
            found = if omit {
                found.and_not(&matched)
            } else {
                found.or(&matched)
            };
        }
        Ok(found)
    }

    /// How the file's stored numbers and dates read for their index keys
    /// (query-program.md 7.3): its locale's date order and decimal and
    /// thousands separators.
    fn reading(&mut self) -> Result<Reading, DataError> {
        let schema = self.calc_schema_cached()?;
        Ok(reading_of(&schema.locale))
    }

    /// The file's own language (`Locale::Type`), which an index in
    /// language 0 keys text in (query-program.md 7.2).
    fn file_language(&mut self) -> Result<u32, DataError> {
        Ok(u32::from(self.calc_schema_cached()?.locale.language.id()))
    }

    /// The records of `table` (out of `all`) that match `criteria`, ANDed:
    /// one SimpleFind of the finders, then the client's filters on stored
    /// values, then the criteria on unstored calculations of `occurrence`
    /// evaluated on what is left. A find on an index the host does not
    /// have yet is answered as the records stream past on the client
    /// ([`Self::find_groups`]).
    fn find_in(
        &mut self,
        table: &Table,
        occurrence: u64,
        all: &RidList,
        criteria: &[(&str, &str)],
        context: &FindContext,
    ) -> Result<RidList, DataError> {
        // The finders, each with the index it reads.
        let mut keep: Vec<(FieldIndex, Vec<u8>)> = Vec::new();
        let mut remove: Vec<(FieldIndex, Vec<u8>)> = Vec::new();
        // Criteria the client evaluates on the host's candidates.
        let mut filters: Vec<(usize, Criterion)> = Vec::new();
        // Criteria on unstored calculations, by field id.
        let mut evaluated: Vec<(u64, Criterion)> = Vec::new();
        let file_language = self.file_language()?;
        for (name, text) in criteria {
            let index = table
                .fields
                .iter()
                .position(|f| f.name == *name)
                .ok_or_else(|| {
                    DataError::with_code(
                        code::FIELD_MISSING,
                        format!("The table has no field named {name:?}."),
                    )
                })?;
            let field = &table.fields[index];
            // Text keyed as the field's index keys it (query-program.md 7.2).
            let context = context.with_text_keys(read_text_keys(field, file_language));
            let criterion = Criterion::parse(text, field.data_type, &context)
                .map_err(|_| found::unreadable(text))?;
            let Some(criterion) = criterion else {
                continue;
            };
            if found::findable(field) == Findable::Evaluated {
                evaluated.push((field.id, criterion));
                continue;
            }
            let plan = finders::plan(table.id, field.id, &criterion);
            for finder in plan.finders {
                let read = (field.id, finder.reads());
                if finder.keep {
                    keep.push((read, finder.bytes));
                } else {
                    remove.push((read, finder.bytes));
                }
            }
            if !plan.exact {
                filters.push((index, criterion));
            }
        }
        // One SimpleFind of the finders that keep records, then one of each
        // that removes records.
        let keeps = !keep.is_empty();
        let mut groups = Vec::with_capacity(remove.len() + 1);
        if keeps {
            groups.push(keep);
        }
        groups.extend(remove.into_iter().map(|finder| vec![finder]));
        let mut answers = self.find_groups(table, &groups)?.into_iter();
        let mut matched = match answers.next() {
            Some(kept) if keeps => kept.and(all),
            removed => all.and_not(&removed.unwrap_or_default()),
        };
        for removed in answers {
            matched = matched.and_not(&removed);
        }
        if !filters.is_empty() {
            matched = self.filter(table, &matched, &filters)?;
        }
        if !evaluated.is_empty() {
            matched = self.filter_evaluated(table.id, occurrence, &matched, all, &evaluated)?;
        }
        Ok(matched)
    }

    /// The records of `table` that each group of finders selects (each the
    /// finders of one SimpleFind, ANDed): the host's SimpleFinds, which
    /// first check the indexes they read ([`checked_find`]). A finder on an
    /// index the host does not have yet finds nothing while FileMaker
    /// Server builds it for later finds (query-program.md 7.1), and a field
    /// marked indexed can lack its index node (schema-path.md 6). Then
    /// every group is answered on the client instead
    /// ([`Self::find_on_client`]).
    fn find_groups(
        &mut self,
        table: &Table,
        groups: &[Vec<(FieldIndex, Vec<u8>)>],
    ) -> Result<Vec<RidList>, DataError> {
        let mut answers = Vec::with_capacity(groups.len());
        let mut needs_client = false;
        for group in groups {
            let (checks, bytes): (Vec<FieldIndex>, Vec<Vec<u8>>) = group.iter().cloned().unzip();
            let (found, missing) = checked_find(self.session_mut()?, table.id, &checks, &bytes)?;
            // Issue every group so the host can build every index this
            // request reads, including the remove finders, for later finds.
            // Building one changes the file, as FileMaker Pro's first find
            // does: the window keeps its found set (file-metadata.md 11).
            if self.built_any(table.id, &missing)? {
                self.saw_own_write(table.id);
            }
            needs_client |= !missing.is_empty();
            answers.push(found);
        }
        if needs_client {
            self.find_on_client(table, groups)
        } else {
            Ok(answers)
        }
    }

    /// [`Self::find_groups`] answered from every record of `table`, read in
    /// batches and keyed as the host's indexes hold it (`om_query::keys`:
    /// every repetition, the file's date order, the field's index
    /// language), each finder decided as the records stream past
    /// ([`ClientFind`]), keeping only the matches.
    fn find_on_client(
        &mut self,
        table: &Table,
        groups: &[Vec<(FieldIndex, Vec<u8>)>],
    ) -> Result<Vec<RidList>, DataError> {
        let finders: Vec<Vec<Vec<u8>>> = groups
            .iter()
            .map(|group| group.iter().map(|(_, bytes)| bytes.clone()).collect())
            .collect();
        let failed = |e: ClientFindError| client_find_error(table, e);
        let mut client = ClientFind::new(&finders, CLIENT_FIND_BUDGET).map_err(failed)?;
        let reading = self.reading()?;
        let file_language = self.file_language()?;
        let reads: Vec<(om_hbam::Field, IndexKind, TextKeys)> = client
            .reads()
            .iter()
            .map(|&(id, index)| {
                let field = table.field(id).cloned().ok_or_else(|| {
                    DataError::with_code(code::FIELD_MISSING, "A find names a missing field.")
                })?;
                let text = text_keys(&field, file_language)?;
                Ok((field, index, text))
            })
            .collect::<Result<_, DataError>>()?;
        let session = self.session_mut()?;
        let ids: Vec<u64> = record_ids(session, table.id)?.iter().collect();
        for batch in ids.chunks(SORT_BATCH) {
            let data = download_records(session, table.id, batch)?;
            let tree = download_tree(std::slice::from_ref(&data))?;
            for row in om_hbam::record::records(&tree, table.id) {
                let keys: Vec<BTreeSet<Vec<u8>>> = reads
                    .iter()
                    .map(|(field, index, text)| {
                        let texts = field_texts(&row, field);
                        match index {
                            IndexKind::Value => {
                                record_value_keys(field.data_type, &texts, reading, *text)
                            }
                            IndexKind::Word => word_keys(&texts, *text),
                        }
                    })
                    .collect();
                client.add(row.id, &keys).map_err(failed)?;
            }
        }
        Ok(client.finish())
    }

    /// The `candidates` whose unstored calculations every criterion of
    /// `evaluated` accepts, each calculation computed on the record in
    /// `occurrence` as the window shows it (script-runtime.md 13.19);
    /// duplicates (`!`) compare with every record of `all`.
    fn filter_evaluated(
        &mut self,
        table: u64,
        occurrence: u64,
        candidates: &RidList,
        all: &RidList,
        evaluated: &[(u64, Criterion)],
    ) -> Result<RidList, DataError> {
        // A calculation that read the objects of a layout listed from the
        // catalog's index only (#1019), or a related file not open yet
        // (#1125): its member or its file, then the find again.
        let mut asked = BTreeSet::new();
        loop {
            let (kept, needs) =
                self.filter_evaluated_once(table, occurrence, candidates, all, evaluated)?;
            let read: Vec<u64> = needs.occurrences.iter().copied().collect();
            if self.grow_for(needs, &mut asked)? {
                continue;
            }
            // One whose file still did not open (its host not reached,
            // another host's account not given) read as missing what the
            // file holds: what it compared is not the record's value, so the
            // find keeps and leaves out no record by it.
            let schema = self.calc_schema_cached()?;
            if let Some(waiting) = read.iter().find(|o| schema.is_pending(**o)) {
                let file = schema
                    .data_source(*waiting)
                    .map_or_else(|| "a related file".to_owned(), |name| format!("\"{name}\""));
                return Err(DataError::with_code(
                    om_data::rules::error::UNAVAILABLE,
                    format!(
                        "A find criterion's calculation reads {file}, which could not be \
                         opened. No records were found."
                    ),
                ));
            }
            return Ok(kept);
        }
    }

    /// [`Self::filter_evaluated`] once, with what its calculations asked
    /// for.
    fn filter_evaluated_once(
        &mut self,
        table: u64,
        occurrence: u64,
        candidates: &RidList,
        all: &RidList,
        evaluated: &[(u64, Criterion)],
    ) -> Result<(RidList, crate::calc::SchemaNeeds), DataError> {
        let schema = self.calc_schema_cached()?;
        let session_info = self.session_info();
        let today = om_data::host::session::today(session_info.utc_offset);
        let (found, _) = self.found_ids(table)?;
        let candidates: Vec<u64> = candidates.iter().collect();
        let duplicates = evaluated.iter().any(|(_, c)| c.finds_duplicates());
        let all: Vec<u64> = if duplicates {
            all.iter().collect()
        } else {
            Vec::new()
        };
        let globals = self.desktop_globals(&schema);
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DataError::new("No hosted file is open."))?;
        let source = NativeSource::new(session, &schema, today)
            .with_fonts(&self.calc.fonts)
            .with_calculation_session(session_info.clone())
            .with_session_globals(globals)
            .with_external(&self.external);
        let mut seed = om_data::host::CalcData::default();
        seed.found.insert(occurrence, found.into_iter().collect());
        source.seed(&seed);
        source.prefetch_records(table, if duplicates { &all } else { &candidates });
        let file = FileCalc::new(&schema, &source, today).with_session(&session_info);
        let cx = Ctx::new(&schema.locale, today);
        let criteria: Vec<Criterion> = evaluated.iter().map(|(_, c)| c.clone()).collect();
        let kept = found::filter_evaluated(
            &criteria,
            candidates,
            &all,
            DateOrder::MonthDayYear,
            |record, i| -> Result<String, DataError> {
                let value = file
                    .read_field(occurrence, record, evaluated[i].0, 1)
                    .unwrap_or_else(|_| om_data::calc::Value::invalid());
                Ok(value.to_text(&cx).into_string())
            },
        )?;
        // A record or table that could not be read fails the find rather
        // than leaving records out.
        if let Some(error) = source.take_failure() {
            return Err(error);
        }
        Ok((kept.into_iter().collect(), source.take_needs()))
    }

    /// A related occurrence named in a criterion, and its base table.
    fn related(&mut self, name: &str) -> Result<(u64, Table), DataError> {
        let missing = || {
            DataError::with_code(
                code::FIELD_MISSING,
                format!("The file has no table occurrence named {name:?}."),
            )
        };
        let occurrence = self
            .catalog
            .table_occurrences
            .iter()
            .find(|o| o.name == name)
            .ok_or_else(missing)?;
        let table = match occurrence.reference() {
            Some((0, table)) => table,
            // A find runs on this file's connection, which cannot read a
            // related file's table.
            Some(_) => return Err(related_find()),
            None => return Err(missing()),
        };
        let id = occurrence.id;
        let table = self.catalog.table(table).ok_or_else(missing)?.clone();
        Ok((id, table))
    }

    /// The relationships from `from` to `to`, all between this file's
    /// tables. A path through a related file's occurrence (back to this
    /// file's included) is refused before a find sends anything: a related
    /// table's client-only id means nothing on this file's connection.
    fn local_path(&mut self, from: u64, to: u64) -> Result<Vec<Hop>, DataError> {
        let schema = self.calc_schema_cached()?;
        let path = schema.path(from, to).ok_or_else(|| {
            DataError::with_code(
                code::FIELD_MISSING,
                "The related field's occurrence is not related to the layout's.",
            )
        })?;
        let related = |occurrence| crate::shows_related_file(&schema, occurrence);
        if path.iter().any(|hop| related(hop.from) || related(hop.to)) {
            return Err(related_find());
        }
        Ok(path)
    }

    /// The records of the first table of `path` ([`Self::local_path`])
    /// related to `records` of its last, relationship by relationship back
    /// along it (query-program.md 6.5, 9.2): each step a JoinProjection
    /// whose source rows are the records' keys of the predicates' fields.
    /// Missing nearer-table indexes use the streamed local-key fallback;
    /// the complete path has already been checked for related-file hops.
    fn join_back(&mut self, path: &[Hop], records: RidList) -> Result<RidList, DataError> {
        let schema = self.calc_schema_cached()?;
        // Stored dates and timestamps read in the file's order.
        let reading = reading_of(&schema.locale);
        let mut current = records;
        for hop in path.iter().rev() {
            let (Some(near), Some(far)) = (schema.table_of(hop.from), schema.table_of(hop.to))
            else {
                return Err(DataError::with_code(
                    code::FIELD_MISSING,
                    "A relationship names a missing table.",
                ));
            };
            let (near, far) = (near.id, far.id);
            let far_table = self.table_def(far)?;
            let predicates: Vec<(u8, u64, u64)> = hop
                .predicates
                .iter()
                .map(|p| (join_type(p.op), p.left, p.right))
                .collect();
            // Each related record's keys for the predicates' fields.
            let ids: Vec<u64> = current.iter().collect();
            let fields: Vec<u64> = predicates.iter().map(|(_, _, b)| *b).collect();
            let rows = self.join_rows(&far_table, &ids, &fields, |field, row| {
                row.cells
                    .get(&field.id)
                    .map(|cell| join_keys(field.data_type, &cell.display(), reading))
                    .unwrap_or_default()
            })?;
            if rows.is_empty() {
                return Ok(RidList::new());
            }
            let mut reads: Vec<FieldIndex> = predicates
                .iter()
                .filter(|(kind, _, _)| *kind != CARTESIAN)
                .map(|(_, a, _)| (*a, IndexKind::Value))
                .collect();
            reads.sort();
            reads.dedup();
            let session = self.session_mut()?;
            let file_id = session
                .opened_file()
                .ok_or_else(|| DataError::new("No hosted file is open."))?
                .reply
                .file_id;
            // The join follows the checks.
            let instruction = reads.len() as u32 + 1;
            let key = om_crypto::query_access_key(instruction, session.connection_key(), file_id);
            let program =
                finders::checked_join_program(file_id, near, &reads, &rows, &predicates, Some(key))
                    .ok_or_else(|| DataError::new("The related records are too many to join."))?;
            let reply = session
                .query(&program)
                .map_err(|e| failure("join related records", e))?;
            if let Some(code) = query_error(&reply.slots) {
                return Err(DataError::with_code(
                    i32::from(code),
                    format!("The host could not join related records (error {code})."),
                ));
            }
            let unreadable = || DataError::new("The host's join reply could not be read.");
            let existed =
                finders::indexes_existed(&reply.slots, reads.len()).ok_or_else(unreadable)?;
            current = if existed.into_iter().all(|existed| existed) {
                finders::found(&reply.slots).ok_or_else(unreadable)?
            } else {
                let near_table = self.table_def(near)?;
                let session = self.session_mut()?;
                let all: Vec<u64> = record_ids(session, near)?.iter().collect();
                let fields: Vec<u64> = reads.iter().map(|(id, _)| *id).collect();
                // The keys the field's value index holds for each record.
                let file_language = self.file_language()?;
                let text: Vec<(u64, TextKeys)> = fields
                    .iter()
                    .filter_map(|id| near_table.field(*id))
                    .map(|field| Ok((field.id, text_keys(field, file_language)?)))
                    .collect::<Result<_, DataError>>()?;
                let mut related = RidList::new();
                for batch in all.chunks(SORT_BATCH) {
                    let targets = self.join_rows(&near_table, batch, &fields, |field, row| {
                        let texts = field_texts(row, field);
                        let form = text
                            .iter()
                            .find(|(id, _)| *id == field.id)
                            .map_or(TextKeys::Collated, |(_, form)| *form);
                        record_value_keys(field.data_type, &texts, reading, form)
                            .into_iter()
                            .collect()
                    })?;
                    for id in finders::join_on_keys(&targets, &rows, &predicates).iter() {
                        related.insert(id);
                    }
                }
                related
            };
        }
        Ok(current)
    }

    /// A base table of the file.
    fn table_def(&self, table: u64) -> Result<Table, DataError> {
        self.catalog
            .table(table)
            .cloned()
            .ok_or_else(|| DataError::new("A related table is missing."))
    }

    /// The records `ids` of `table` as join rows: each record's `keys` of
    /// each of `fields`, read in batches.
    fn join_rows(
        &mut self,
        table: &Table,
        ids: &[u64],
        fields: &[u64],
        keys: impl Fn(&om_hbam::Field, &om_hbam::record::Record) -> Vec<Vec<u8>>,
    ) -> Result<Vec<finders::JoinRow>, DataError> {
        let session = self.session_mut()?;
        let mut rows = Vec::with_capacity(ids.len());
        for batch in ids.chunks(SORT_BATCH) {
            let data = download_records(session, table.id, batch)?;
            let tree = download_tree(std::slice::from_ref(&data))?;
            for row in om_hbam::record::records(&tree, table.id) {
                let row_keys = fields
                    .iter()
                    .map(|&id| {
                        let found = table.field(id).map(|field| keys(field, &row));
                        (id, found.unwrap_or_default())
                    })
                    .collect();
                rows.push((row.id, row_keys));
            }
        }
        Ok(rows)
    }

    /// The `candidates` whose stored values every criterion of `filters`
    /// accepts (by field position), read in batches. A duplicates criterion
    /// among them already has the host's FindDuplicates finder applied.
    fn filter(
        &mut self,
        table: &Table,
        candidates: &RidList,
        filters: &[(usize, Criterion)],
    ) -> Result<RidList, DataError> {
        let order = self.calc_schema_cached()?.locale.date_order;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DataError::new("No hosted file is open."))?;
        let ids: Vec<u64> = candidates.iter().collect();
        let mut kept = RidList::new();
        for batch in ids.chunks(SORT_BATCH) {
            let data = download_records(session, table.id, batch)?;
            let tree = download_tree(std::slice::from_ref(&data))?;
            for row in om_hbam::record::records(&tree, table.id) {
                let values = row.values_for(table);
                let accepted = filters.iter().all(|(index, criterion)| {
                    let value = values.get(*index).cloned().flatten().unwrap_or_default();
                    criterion.matches(&value, order, &|_| true)
                });
                if accepted {
                    kept.insert(row.id);
                }
            }
        }
        Ok(kept)
    }

    /// `ids` of `table` in the order of `sort` (query-program.md 10.3):
    /// each record's stored values, compared as FileMaker's sort keys.
    fn sorted(
        &mut self,
        table: &Table,
        ids: &[u64],
        sort: &[SortField],
    ) -> Result<Vec<u64>, DataError> {
        for field in sort {
            let names =
                std::iter::once(&field.field).chain(field.summary.as_ref().map(|s| &s.field));
            for name in names {
                if table.field_by_name(name).is_none() {
                    return Err(DataError::with_code(
                        code::FIELD_MISSING,
                        format!("The table has no field named {name:?}."),
                    ));
                }
            }
        }
        let date_order = self.calc_schema_cached()?.locale.date_order;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DataError::new("No hosted file is open."))?;
        let mut rows = Vec::with_capacity(ids.len());
        for batch in ids.chunks(SORT_BATCH) {
            let data = download_records(session, table.id, batch)?;
            let tree = download_tree(std::slice::from_ref(&data))?;
            rows.extend(om_hbam::record::records(&tree, table.id));
        }
        let page = crate::record_page(rows, table, ids.len() as u64)?;
        // Keep the found set's order for equal keys.
        let position = |id: i64| ids.iter().position(|i| *i as i64 == id);
        let mut records: Vec<&Record> = page.records.iter().collect();
        records.sort_by_key(|r| position(r.id));
        found::sort_rows_with_date_order(&page.fields, &mut records, sort, date_order);
        Ok(records.iter().map(|r| r.id as u64).collect())
    }
}

/// Supported imported sort keys, checked against the owning field metadata.
/// Qualified names belong to the root layout's base occurrence, not the
/// owner's graph. Keep all caller options and names in `TableFound` while
/// passing these translated keys to the same sorter used by local tables.
fn imported_sort_fields(
    table: &Table,
    base: &str,
    sort: &[SortField],
) -> Result<Vec<SortField>, DataError> {
    let unsupported = || {
        DataError::with_code(
            code::UNSUPPORTED,
            "Related-file sorts support stored scalar base fields only; relationship, \
             repeating, container, evaluated calculation and summary sorts are not supported yet.",
        )
    };
    sort.iter()
        .map(|key| {
            if key.summary.is_some() {
                return Err(unsupported());
            }
            let name = match key.field.split_once("::") {
                Some((qualifier, field)) if qualifier == base => field,
                Some(_) => return Err(unsupported()),
                None => &key.field,
            };
            if name.contains("::") {
                return Err(unsupported());
            }
            let field = table.field_by_name(name).ok_or_else(|| {
                DataError::with_code(
                    code::FIELD_MISSING,
                    format!("The table has no field named {name:?}."),
                )
            })?;
            if !matches!(
                field.kind,
                om_hbam::FieldKind::Normal | om_hbam::FieldKind::Calculation
            ) || !matches!(
                field.data_type,
                om_hbam::DataType::Text
                    | om_hbam::DataType::Number
                    | om_hbam::DataType::Date
                    | om_hbam::DataType::Time
                    | om_hbam::DataType::Timestamp
            ) || !field.decoded_options().is_some_and(|options| {
                options.max_repetitions == 1 && options.options & 0x3_0000 == 0
            }) {
                return Err(unsupported());
            }
            let mut key = key.clone();
            key.field = name.to_owned();
            Ok(key)
        })
        .collect()
}

/// `DBJoinPred::Type` 6, `×`: every record, without an index (9.2).
const CARTESIAN: u8 = 6;

/// `DBJoinPred::Type` of a relationship operator (query-program.md 9.2).
fn join_type(op: om_data::host::JoinOp) -> u8 {
    use om_data::host::JoinOp;
    match op {
        JoinOp::Equal => 0,
        JoinOp::NotEqual => 1,
        JoinOp::Less => 2,
        JoinOp::LessEqual => 3,
        JoinOp::Greater => 4,
        JoinOp::GreaterEqual => 5,
        JoinOp::Cartesian => CARTESIAN,
    }
}

/// Error 3 for a find criterion in a related file's table, or through one.
fn related_find() -> DataError {
    DataError::with_code(
        code::UNSUPPORTED,
        "Finding by fields of related files' tables is not supported yet.",
    )
}

/// One SimpleFind of `finders` of `table` on the host (op86), after a
/// GetKey of the counts leaf of each index in `reads` in the same program
/// (`om_query::find::checked_find_program`, query-program.md 0, 8.5): the
/// records the finders select, and the indexes of `reads` that were
/// missing as the find ran. A finder on a missing index finds nothing,
/// FileMaker Server building the index only afterwards (7.1); a field
/// marked indexed can still lack its index node (schema-path.md 6).
pub(crate) fn checked_find(
    session: &mut om_session::Session,
    table: u64,
    reads: &[FieldIndex],
    finders: &[Vec<u8>],
) -> Result<(RidList, Vec<FieldIndex>), DataError> {
    let mut reads = reads.to_vec();
    reads.sort();
    reads.dedup();
    let file_id = session
        .opened_file()
        .ok_or_else(|| DataError::new("No hosted file is open."))?
        .reply
        .file_id;
    // The find follows the checks.
    let instruction = reads.len() as u32 + 1;
    let key = om_crypto::query_access_key(instruction, session.connection_key(), file_id);
    let program = finders::checked_find_program(file_id, table, &reads, finders, Some(key));
    let reply = session
        .query(&program)
        .map_err(|e| failure("find records", e))?;
    if let Some(code) = query_error(&reply.slots) {
        return Err(DataError::with_code(
            i32::from(code),
            format!("The host could not run the find (error {code})."),
        ));
    }
    let unreadable = || DataError::new("The host's find reply could not be read.");
    let existed = finders::indexes_existed(&reply.slots, reads.len()).ok_or_else(unreadable)?;
    let found = finders::found(&reply.slots).ok_or_else(unreadable)?;
    let missing = reads
        .into_iter()
        .zip(existed)
        .filter(|(_, existed)| !existed)
        .map(|(index, _)| index)
        .collect();
    Ok((found, missing))
}

/// [`checked_find`] of `finders` on `field`'s value index, answered only
/// from an index that already existed: `None` when the host had no counts
/// leaf for it as the find ran.
pub(crate) fn indexed_find(
    session: &mut om_session::Session,
    table: u64,
    field: u64,
    finders: &[Vec<u8>],
) -> Result<Option<RidList>, DataError> {
    let (found, missing) = checked_find(session, table, &[(field, IndexKind::Value)], finders)?;
    Ok(missing.is_empty().then_some(found))
}

/// The duplicate-state accounting budget for a client find ([`ClientFind`]).
const CLIENT_FIND_BUDGET: usize = 64 << 20;

/// How a find reads `field`'s index when the host has one: its index
/// language's form, language 0 the file's own (`file_language`), English
/// where OpenMaker builds no form ([`om_data::rules::read_text_keys`]).
fn read_text_keys(field: &om_hbam::Field, file_language: u32) -> TextKeys {
    match field.decoded_options() {
        Some(options) if field.data_type == om_hbam::DataType::Text => {
            om_data::rules::read_text_keys(options.index_language(), file_language)
        }
        _ => TextKeys::Collated,
    }
}

/// How `field`'s indexes key text: the form of its index language
/// (query-program.md 7.2), language 0 the file's own (`file_language`). A
/// text field in a language whose keys are not established cannot be found
/// without the host's index.
fn text_keys(field: &om_hbam::Field, file_language: u32) -> Result<TextKeys, DataError> {
    if field.data_type != om_hbam::DataType::Text {
        return Ok(TextKeys::Collated);
    }
    let language = field
        .decoded_options()
        .ok_or_else(|| {
            DataError::with_code(
                code::UNSUPPORTED,
                format!(
            "Field {:?} has no readable index language for a find without the host's index.",
            field.name
        ),
            )
        })?
        .index_language();
    om_data::rules::written_text_keys(language, file_language).ok_or_else(|| {
        DataError::with_code(
            code::UNSUPPORTED,
            format!(
                "Field {:?} has no index on the host yet, and OpenMaker cannot key its \
                 index language ({language}) to find without one.",
                field.name
            ),
        )
    })
}

/// A find on the client that could not answer.
fn client_find_error(table: &Table, error: ClientFindError) -> DataError {
    match error {
        ClientFindError::OverBudget { budget } => DataError::new(format!(
            "Finding duplicates in table {:?} without the host's index exceeded its {} MiB \
             state budget. Try again after the host has built the index.",
            table.name,
            budget >> 20
        )),
        ClientFindError::Query(e) => {
            DataError::new(format!("The find could not be evaluated: {e}"))
        }
    }
}

/// The text of each repetition of `field` in `row`, empty for one the
/// record does not store: the first under the field's id, repetition n
/// under `id * 256 + n` (execute-sql.md, reading `.fmp12` files), as a
/// host's indexes read them. A repetition whose key would pass `u64::MAX`
/// cannot have a cell and is empty ([`om_data::rules::cell_of`]).
fn field_texts(row: &om_hbam::record::Record, field: &om_hbam::Field) -> Vec<String> {
    let repetitions = field
        .decoded_options()
        .map_or(1, |o| o.max_repetitions.max(1));
    let text = |id: Option<u64>| {
        id.and_then(|id| row.text(id))
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    (1..=repetitions)
        .map(|n| text(om_data::rules::cell_of(field.id, u32::from(n))))
        .collect()
}

/// The value-index keys of a stored value (one per line), as a join
/// compares them; empty lines have none (an empty value never relates,
/// file-metadata.md 4).
fn join_keys(data_type: om_hbam::DataType, value: &str, reading: Reading) -> Vec<Vec<u8>> {
    use om_hbam::DataType;
    value
        .split(['\r', '\n'])
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match data_type {
            // The reading treats spaces itself (`1e3 ` is 13 where the
            // thousands separator is a space).
            DataType::Number => om_collate::number_key_in(line, reading.numbers),
            DataType::Date => om_collate::date_key(line.trim(), reading.order),
            DataType::Time => om_collate::time_key(line.trim()),
            DataType::Timestamp => om_collate::timestamp_key(line.trim(), reading.order),
            _ => Some(om_collate::text_index_key(line.trim())),
        })
        .collect()
}

/// [`Reading`] of a file's locale.
fn reading_of(locale: &om_data::calc::Locale) -> Reading {
    Reading {
        order: locale.date_order,
        numbers: om_collate::NumberFormat {
            decimal: locale.decimal_separator,
            thousands: locale.thousands_separator,
        },
    }
}

/// The error slot of a failed op86 reply, `[00] = 02 <u16>` (query-program.md
/// 11.2).
pub(crate) fn query_error(slots: &[u8]) -> Option<u16> {
    let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(slots)).ok()?;
    let value = tree.node(&[&[0]])?.value?;
    match value {
        [2, hi, lo] => Some(u16::from_be_bytes([*hi, *lo])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repetition n of field F is the cell `F * 256 + n`, F from the file
    /// (#388): the largest field id whose repetitions have keys reads its
    /// 255th from the cell `u64::MAX`; past it only the first repetition
    /// has a cell, and the others read as empty, not as the cells the key
    /// would wrap around to.
    #[test]
    fn repetitions_of_the_largest_field_ids() {
        let field = |id: u64, repetitions: u16| {
            let mut options = vec![0; 26];
            options[0] = 1;
            options[1] = 1;
            options[24..26].copy_from_slice(&repetitions.to_be_bytes());
            om_hbam::Field {
                id,
                name: "Field".into(),
                kind: om_hbam::FieldKind::Normal,
                data_type: om_hbam::DataType::Text,
                options,
            }
        };
        let text = |s: &str| om_hbam::record::Cell::Value(om_scsu::encode_scsu(s));
        let largest = u64::MAX / 256;
        let row = om_hbam::record::Record {
            id: 1,
            cells: [
                (largest, text("first")),
                (u64::MAX, text("last")),
                (largest + 1, text("only")),
                (2, text("another field")),
            ]
            .into(),
        };
        let texts = field_texts(&row, &field(largest, 255));
        assert_eq!((texts[0].as_str(), texts[254].as_str()), ("first", "last"));
        assert_eq!(field_texts(&row, &field(largest + 1, 3)), ["only", "", ""]);
    }
}
