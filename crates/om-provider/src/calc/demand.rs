//! Related files opened when something first reads them (#983), for a
//! window opening a hosted file: the opened file's own calculation schema
//! comes first, with no related file opened, and grows as layouts and
//! calculations read tables of related files. FileMaker Pro 26 opened 19 of
//! a 63-file solution's files over a whole session, on demand (#983's
//! capture); OpenMaker opened all 63 before the first layout.
//!
//! [`ExternalFiles::set_on_demand`](crate::external::ExternalFiles::set_on_demand)
//! turns it on; without it every related file opens when the schema is
//! built, as before. On demand:
//! - The schema the window asks for first is the opened file's own: the
//!   occurrences of related files wait ([`CalcSchema::is_pending`]).
//! - Drawing a layout opens the files of the occurrences it reads
//!   ([`om_data::host::needs::layout_occurrences`]) before it is drawn.
//! - A calculation reading a waiting occurrence asks for it
//!   ([`CalcNeed::Occurrence`]); the related reads of a calculation open
//!   their files before they are planned ([`NativeProvider::grow_for_needs_until`]).
//! - Growing builds the schema again from the opened file's own, kept here,
//!   with no download: the files already open import first, in the order
//!   they opened, so every id handed out stays; the newly wanted ones are
//!   opened and imported after them. What calculations read is kept, since
//!   no id changed. The window takes the grown schema with the answer
//!   ([`om_data::DataProvider::take_grown_schema`]).

use std::collections::BTreeSet;
use std::sync::Arc;

use om_data::host::{CalcNeed, CalcSchema};
use om_data::DataError;

use crate::NativeProvider;

/// The root's extra catalogs and decoded schema before any related file is
/// imported. Related growth clones this schema; a root metadata change
/// drops only the decoded copy, so the next growth builds it again.
pub(crate) struct RootSchema {
    pub extra: Vec<crate::Download>,
    pub decoded: Option<CalcSchema>,
}

impl NativeProvider {
    /// Root metadata changed without dropping the whole calculation schema
    /// (for example, a layout member arrived). Keep its raw catalogs for
    /// hydration, but decode the root again before the next import.
    pub(crate) fn invalidate_root_schema(&mut self) {
        if let Some(root) = self.calc.root.as_mut() {
            root.decoded = None;
        }
    }

    /// Open the related files the needs read, on demand: the occurrences
    /// they name that wait for their files or field catalogs. Once `stop`
    /// holds (the window's read was superseded, #987) no further file opens:
    /// the files opened so far stay in the grown schema, the rest wait.
    pub(crate) fn grow_for_needs_until(
        &mut self,
        needs: &[CalcNeed],
        stop: &dyn Fn() -> bool,
    ) -> Result<bool, DataError> {
        // A layout listed from the catalog's index only: its member (#1019).
        let mut grew = false;
        for need in needs {
            if let CalcNeed::Layout { layout } = *need {
                if stop() {
                    return Ok(grew);
                }
                grew |= self.read_layout(layout)?;
            }
        }
        if !self.external.on_demand() {
            return Ok(grew);
        }
        let schema = self.calc_schema_cached()?;
        let mut occurrences = Vec::new();
        for need in needs {
            let wanted = match *need {
                CalcNeed::Occurrence { occurrence } => vec![occurrence],
                // A related set walks every occurrence of its path: an
                // intermediate one of another file is read as well.
                CalcNeed::Related { from, to, .. } | CalcNeed::RelatedIds { from, to, .. } => {
                    std::iter::once(from)
                        .chain(
                            schema
                                .path(from, to)
                                .unwrap_or_default()
                                .iter()
                                .map(|h| h.to),
                        )
                        .chain(std::iter::once(to))
                        .collect()
                }
                CalcNeed::FoundSet { occurrence, .. } => vec![occurrence],
                // Data requests carry base-table ids, including tables of
                // already-open files whose fields have not been read. Ask
                // through an occurrence so `want()` reads their catalog
                // before the data read plans over it (#772).
                CalcNeed::Record { table, .. }
                | CalcNeed::Records { table }
                | CalcNeed::Globals { table }
                | CalcNeed::Lookup { table, .. }
                | CalcNeed::Member { table, .. }
                | CalcNeed::MemberData { table, .. } => match schema
                    .occurrences()
                    .find(|o| o.table == table && schema.is_pending(o.id))
                {
                    Some(occurrence) => vec![occurrence.id],
                    // There is no occurrence through which `want()` can
                    // hydrate this catalog. Never decode and cache empty
                    // values as a successful record from an unread table.
                    None if schema.are_fields_pending(table) => {
                        return Err(DataError::with_code(
                            3,
                            "The table's fields are unread and no occurrence can request them.",
                        ));
                    }
                    None => Vec::new(),
                },
                _ => Vec::new(),
            };
            occurrences.extend(wanted);
        }
        if occurrences.is_empty() {
            return Ok(grew);
        }
        Ok(self.grow_related_until(occurrences, stop)? || grew)
    }

    /// The members of `layouts`, which a calculation the provider evaluated
    /// itself asked for (a record page's, a find's, a value list's, a
    /// write's; #1019), read. `true` when the schema changed, so that
    /// evaluation runs again over it and reads their objects rather than
    /// none. Each layout settles once, read or found absent, so a caller
    /// evaluating again until this is `false` stops.
    pub(crate) fn read_layouts(
        &mut self,
        layouts: impl IntoIterator<Item = u32>,
    ) -> Result<bool, DataError> {
        let mut grew = false;
        for layout in layouts {
            grew |= self.read_layout(layout)?;
        }
        Ok(grew)
    }

    /// Open the related files the rules of base table `table` read, before
    /// a write to it is planned (#1139), as the planner reads those rules:
    /// from `tree`, its field catalog `[T, 3]` as just downloaded, or, when
    /// `None`, from the catalogs read at open (`schema_downloads`). Nothing
    /// is read while nothing in the schema waits ([`CalcSchema::waits`]: a
    /// related file not opened yet, or a table whose fields are not read
    /// yet, #772).
    pub(crate) fn open_rule_files(
        &mut self,
        table: &om_hbam::Table,
        tree: Option<&om_hbam::KeyTree<'_>>,
    ) -> Result<(), DataError> {
        if !self.external.on_demand() {
            return Ok(());
        }
        let mut asked = BTreeSet::new();
        loop {
            let schema = self.calc_schema_cached()?;
            if !schema.waits() {
                return Ok(());
            }
            let read = match tree {
                Some(tree) => rule_reads(&schema, table, tree),
                None => {
                    // The table's own catalogs only: the planner reads no other.
                    let own = crate::key_bytes(table.id);
                    let mut downloads: Vec<&crate::Download> = self
                        .schema_downloads
                        .iter()
                        .filter(|d| d.path.first() == Some(&own))
                        .collect();
                    downloads.sort_by(|a, b| a.path.cmp(&b.path));
                    let tree = crate::download_tree_refs(&downloads)?;
                    rule_reads(&schema, table, &tree)
                }
            };
            let fresh: Vec<u64> = read
                .into_iter()
                .filter(|occurrence| schema.is_pending(*occurrence) && asked.insert(*occurrence))
                .collect();
            if fresh.is_empty() || !self.grow_related(fresh)? {
                return Ok(());
            }
        }
    }

    /// Whether a rule of base table `table` (from `tree`, its field catalog
    /// as the planner reads it) reads a related file still waiting to open
    /// in `schema`, or a table whose fields are not read yet: then a plan
    /// refused over `schema` may be decided only by what waits, which the
    /// write reads first ([`Self::open_rule_files`]).
    pub(crate) fn rule_files_pending(
        &self,
        schema: &CalcSchema,
        table: &om_hbam::Table,
        tree: &om_hbam::KeyTree<'_>,
    ) -> bool {
        self.external.on_demand()
            && schema.waits()
            && rule_reads(schema, table, tree)
                .into_iter()
                .any(|occurrence| schema.is_pending(occurrence))
    }

    /// Grow the schema for what a calculation evaluated over it asked for
    /// (`needs`): the members of layouts it read as having no objects
    /// (#1019), and the related files not open yet it read, opened as the
    /// window's reads open them (#1125). `true` when the schema changed, so
    /// the caller evaluates again over it. Each occurrence is asked for
    /// once per evaluation (`asked`): a file that does not open (refused,
    /// its host not reached, another host's account not given) ends the
    /// loop rather than repeating it.
    pub(crate) fn grow_for(
        &mut self,
        needs: super::SchemaNeeds,
        asked: &mut BTreeSet<u64>,
    ) -> Result<bool, DataError> {
        let mut grew = self.read_layouts(needs.layouts)?;
        let fresh: Vec<u64> = needs
            .occurrences
            .into_iter()
            .filter(|occurrence| asked.insert(*occurrence))
            .collect();
        if !fresh.is_empty() {
            grew |= self.grow_related(fresh)?;
        }
        Ok(grew)
    }

    /// Layout `layout`'s member, when only the catalog's index was read for
    /// it. `true` when the schema had it waiting and no longer has.
    fn read_layout(&mut self, layout: u32) -> Result<bool, DataError> {
        if !self.calc_schema_cached()?.is_layout_pending(layout) {
            return Ok(false);
        }
        self.ensure_layout_member(u64::from(layout))?;
        Ok(!self.calc_schema_cached()?.is_layout_pending(layout))
    }

    /// The window read the whole script catalog `[17]` (`whole`). A
    /// calculation schema built from the catalog's index alone (on demand,
    /// as FileMaker Pro 26 reads it) keeps it for its later builds in place
    /// of the index, and is built again now when the catalog holds a
    /// separator: the index lists one as a script ([`om_data::meta::indexed_scripts`]),
    /// the only way its list can differ. Nothing otherwise.
    pub(crate) fn adopt_scripts(&mut self, whole: crate::Download) -> Result<(), DataError> {
        let index = super::script_index();
        let Some(root) = self.calc.root.as_mut() else {
            return Ok(());
        };
        let Some(at) = root.extra.iter().position(|d| d.path == index) else {
            return Ok(());
        };
        let separators = {
            let tree = crate::download_tree(std::slice::from_ref(&whole))?;
            om_data::meta::scripts(&tree)
                .iter()
                .any(|s| s.kind == om_data::ScriptKind::Separator)
        };
        root.extra[at] = whole;
        root.decoded = None;
        if separators {
            self.regrow_schema()?;
        }
        Ok(())
    }

    /// The calculation schema with the related files `layout` reads open.
    pub(crate) fn calc_schema_for_layout(
        &mut self,
        layout: &om_layout::Layout,
    ) -> Result<Arc<CalcSchema>, DataError> {
        let schema = self.calc_schema_cached()?;
        if !self.external.on_demand() {
            return Ok(schema);
        }
        let occurrences = om_data::host::needs::layout_occurrences(&schema, layout);
        if self.grow_related(occurrences)? {
            return self.calc_schema_cached();
        }
        Ok(schema)
    }

    /// Ask for the files of `occurrences`, and when that changes what the
    /// schema has, build it again from the opened file's own (module docs).
    /// `true` when it grew.
    pub(crate) fn grow_related(
        &mut self,
        occurrences: impl IntoIterator<Item = u64>,
    ) -> Result<bool, DataError> {
        self.grow_related_until(occurrences, &|| false)
    }

    /// [`Self::grow_related`], opening no further file once `stop` holds.
    pub(crate) fn grow_related_until(
        &mut self,
        occurrences: impl IntoIterator<Item = u64>,
        stop: &dyn Fn() -> bool,
    ) -> Result<bool, DataError> {
        // Every related file opens with the schema otherwise: nothing to do.
        if !self.external.on_demand() {
            return Ok(false);
        }
        let schema = self.calc_schema_cached()?;
        if !self.external.want(&schema, occurrences) {
            return Ok(false);
        }
        if self.calc.root.is_none() {
            return Ok(false);
        }
        self.regrow_schema_until(stop)?;
        Ok(true)
    }

    /// Build the calculation schema again from the opened file's catalogs
    /// as downloaded now, with no download of them: its own schema, then
    /// the related files already open imported again in the order they
    /// opened (no id changes), and those wanted since opened after them.
    /// For a related file a read wants, and for a layout member read on
    /// demand (#1019). What calculations read stays; the window takes the
    /// schema with the answer ([`om_data::DataProvider::take_grown_schema`]).
    /// Nothing before the first schema, or when nothing opens on demand.
    pub(crate) fn regrow_schema(&mut self) -> Result<(), DataError> {
        if self.calc.schema.is_none() || self.calc.root.is_none() {
            return Ok(());
        }
        self.regrow_schema_until(&|| false)
    }

    fn regrow_schema_until(&mut self, stop: &dyn Fn() -> bool) -> Result<(), DataError> {
        let Some(root) = self.calc.root.as_ref() else {
            return Ok(());
        };
        let tree = {
            let mut downloads: Vec<&crate::Download> = self
                .schema_downloads
                .iter()
                .chain(
                    self.layout_downloads
                        .iter()
                        .filter(|d| super::calc_catalog(d)),
                )
                .chain(&root.extra)
                .collect();
            // A key tree is built in key order.
            downloads.sort_by(|a, b| a.path.cmp(&b.path));
            crate::download_tree_refs(&downloads)?
        };
        let mut grown = root
            .decoded
            .clone()
            .unwrap_or_else(|| self.root_schema(&tree));
        // Keep only the root before hydration mutates the working copy.
        // Publish a refreshed copy only if hydration succeeds.
        let decoded = root.decoded.is_none().then(|| grown.clone());
        // The account the file is open with, kept for its scripts' opens,
        // opens the related files now wanted, on its own host only (#978).
        if let Some((account, password)) = self.held_credentials() {
            self.external.lend(&account, &password);
        }
        self.external.hydrate_until(
            &self.config,
            &self.close_requests,
            self.session.as_ref(),
            &mut grown,
            &self.catalog,
            &tree,
            stop,
        )?;
        if let Some(decoded) = decoded {
            self.calc.root.as_mut().expect("root schema").decoded = Some(decoded);
        }
        let grown = Arc::new(grown);
        self.register_desktop_file(Some(&grown));
        self.calc.schema = Some(grown);
        self.calc.grown = true;
        Ok(())
    }
}

/// What the rules of `table` read, as [`RecordPlan`] computes them from
/// its field catalog `tree`: the formulas of its stored calculation fields
/// (an unstored or global one is not computed by a write), auto-entered and
/// validation calculations, its looked-up values and validation value
/// lists ([`om_data::host::needs::rule_occurrences`]).
///
/// [`RecordPlan`]: om_data::rules::RecordPlan
pub(crate) fn rule_reads(
    schema: &CalcSchema,
    table: &om_hbam::Table,
    tree: &om_hbam::KeyTree<'_>,
) -> BTreeSet<u64> {
    use om_data::field_options::{AutoEnterValue, FieldOptions};
    use om_hbam::catalog::FieldKind;
    // Members `05` (a calculation or auto-entered calculation) and `19` (a
    // validation calculation) of `[T, 3, 5, F]` (schema-path.md 5.5).
    const FORMULA: u64 = 0x05;
    const VALIDATION_FORMULA: u64 = 0x19;
    // The option word's global storage (schema-path.md 5.5, 5.10).
    const GLOBAL: u32 = 0x1_0000;
    let formula = |field: u64, member: u64| {
        let node = tree.at(&[table.id, 3, 5, field, member])?;
        let polish = node.get(6)?.assembled()?;
        let context = node.get(4).and_then(|n| n.assembled());
        om_data::formula::Formula::decode(context.as_deref(), None, &polish).ok()
    };
    let mut formulas = Vec::new();
    let mut lookups = Vec::new();
    let mut lists = Vec::new();
    for field in &table.fields {
        match field.kind {
            FieldKind::Calculation => {
                let computed = field.decoded_options().is_none_or(|options| {
                    !options.unstored_calculation() && options.options & GLOBAL == 0
                });
                if computed {
                    formulas.extend(formula(field.id, FORMULA));
                }
            }
            FieldKind::Normal => {
                let Some(mut options) = FieldOptions::decode(&field.options, None, None) else {
                    continue;
                };
                options.read_members(&RuleMembers {
                    tree,
                    path: [table.id, 3, 5, field.id],
                });
                match &options.auto_enter.value {
                    AutoEnterValue::Calculated(_) => formulas.extend(formula(field.id, FORMULA)),
                    AutoEnterValue::LookedUp(Some(lookup)) => {
                        lookups.push((lookup.from, lookup.to));
                    }
                    _ => {}
                }
                if options.validation.calculation.is_some() {
                    formulas.extend(formula(field.id, VALIDATION_FORMULA));
                }
                if let Some(id) = options.validation.value_list.filter(|id| *id != 0) {
                    lists.extend(schema.value_lists().iter().filter(|list| list.id == id));
                }
            }
            _ => {}
        }
    }
    om_data::host::needs::rule_occurrences(schema, &formulas, &lookups, &lists)
}

/// A field's members `[T, 3, 5, F]` as a write's rules read them before
/// it is planned: bytes and text only (a looked-up value's definition, a
/// validation list's reference). Formulas come from the calculation
/// schema.
struct RuleMembers<'a, 't> {
    tree: &'a om_hbam::KeyTree<'t>,
    path: [u64; 4],
}

impl RuleMembers<'_, '_> {
    fn node(&self, key: u8) -> Option<&om_hbam::Node<'_>> {
        let [table, catalog, members, field] = self.path;
        self.tree
            .at(&[table, catalog, members, field, u64::from(key)])
    }
}

impl om_data::field_options::OptionMembers for RuleMembers<'_, '_> {
    fn text(&self, key: u8) -> Option<String> {
        self.node(key)?.text()?.ok()
    }

    fn bytes(&self, key: u8) -> Option<Vec<u8>> {
        Some(self.node(key)?.assembled()?.into_owned())
    }

    fn formula(&self, _key: u8) -> Option<om_data::field_options::OptionFormula> {
        None
    }
}
