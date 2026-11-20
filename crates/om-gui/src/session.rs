//! An open file's state: its layouts, the selected layout's geometry, the
//! found set's current record, and the window of records loaded from the
//! provider around it.
//!
//! Browse record pages are provider fetch windows. Preview separately plans
//! physical report pages, without changing the current Browse record.

use std::collections::{BTreeMap, BTreeSet};

use om_data::host::CalcSchema;
use om_data::{DataError, Field, LayoutGeometry, LayoutInfo, Record, RecordPage, RenderLayout};

/// How many records one provider request fetches.
pub(crate) const DEFAULT_PAGE_SIZE: u32 = 25;

/// Where a session is connected, for display.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub host: String,
    pub file: String,
    /// The file's path, for a file opened from this computer (its `host`
    /// is empty).
    pub local: Option<std::path::PathBuf>,
}

/// Identifies one `records` request so stale replies can be dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PageKey {
    pub revision: u64,
    pub layout_id: u32,
    pub offset: u64,
    pub limit: u32,
}

/// A page of records as loaded from the provider.
#[derive(Clone, Debug)]
pub(crate) struct Page {
    pub key: PageKey,
    pub data: RecordPage,
    /// Its records and fields the account may not see, shown as
    /// `<No Access>` (`DataProvider::no_access`).
    pub no_access: om_data::NoAccess,
}

/// What is known of the selected layout's geometry.
#[derive(Debug, PartialEq)]
pub(crate) enum Geometry {
    /// Requested; the reply is not in yet.
    Loading,
    /// The provider has none for this layout (or failed to load it); Form
    /// view falls back to the generic field list.
    Unavailable,
    Loaded(LayoutGeometry),
}

/// The actual styled layout, separate from the geometry used for authoring.
#[derive(Debug, PartialEq)]
pub(crate) enum RenderState {
    Loading,
    Unavailable,
    Loaded(Box<RenderLayout>),
}

type PlanResult = Result<std::sync::Arc<om_layout::pagination::ReportPlan>, String>;
/// The grouped found set's record ids and its sorted sub-summary groups.
pub(crate) type GroupsResult = Result<crate::report::FoundSetGroups, String>;
/// Browse List view's sub-summary groups: none to show (`Ok(None)`), the
/// groups, or why they cannot be shown.
pub(crate) type ListGroupsResult = Result<Option<std::sync::Arc<crate::calc::ListGroups>>, String>;
/// One found set's report: its sub-summary groups (read once through the
/// window's calculations, never per paint) and the plan made from them;
/// and List view's groups, read and invalidated the same way.
#[derive(Debug)]
struct CachedReport {
    revision: u64,
    count: u64,
    groups: Option<GroupsResult>,
    plan: Option<PlanResult>,
    list: Option<ListGroupsResult>,
}

/// The state of an open file.
#[derive(Debug)]
pub(crate) struct Session {
    pub target: Target,
    pub layouts: Vec<LayoutInfo>,
    pub layout_menu: Vec<om_data::LayoutMenuEntry>,
    /// The layouts the account's custom set gives no access
    /// (session-identity.md 2.2): left out of the layout menus, and shown as
    /// a gray page when the window opens or goes there.
    pub hidden_layouts: BTreeSet<u32>,
    /// The window opened on a layout the account's set hides
    /// ([`Self::open_on`]) and has not left it: FileMaker Pro's blank gray
    /// page, without the `<No Access>` text Go to Layout shows (#696).
    pub blank_start: bool,
    /// Go to Related Record took the window to a layout the account's set
    /// hides: the layout menu names it, as FileMaker Pro 26.0.3 did (#696).
    pub hidden_named: bool,
    pub layouts_loading: bool,
    /// The selected layout.
    pub layout: Option<u32>,
    /// The selected layout's geometry.
    pub geometry: Geometry,
    pub render: RenderState,
    pub page_size: u32,
    /// The most recently loaded page of the selected layout.
    pub page: Option<Page>,
    /// The page request in flight, if any. Only its reply is accepted.
    pub pending: Option<PageKey>,
    /// Zero-based index of the current record in the found set.
    pub current: u64,
    /// The current record of each base table the window showed last, so
    /// another table's layout and back returns to it (FileMaker keeps a
    /// current record per table occurrence; the provider keeps a found set
    /// per base table).
    table_current: BTreeMap<u64, u64>,
    /// Physical Preview page; independent of the Browse current record.
    pub preview_page: u64,
    pub paper: Option<om_layout::paper::PaperSettings>,
    paper_revision: u64,
    report: std::cell::RefCell<Option<CachedReport>>,
    /// Preview's decoded static pictures, for the displayed layout snapshot.
    pictures: crate::report::Pictures,
    cache: BTreeMap<u64, Page>,
    ranges: BTreeSet<PageKey>,
    range_errors: BTreeMap<u64, String>,
    revision: u64,
    /// What the window showed before a layout switch, until the new
    /// layout's first page is in: a connection lost meanwhile puts it back
    /// rather than leaving the window blank.
    shown: Option<Box<Shown>>,
}

/// A layout as the window showed it.
#[derive(Debug)]
struct Shown {
    layout: Option<u32>,
    geometry: Geometry,
    render: RenderState,
    page: Option<Page>,
    current: u64,
    cache: BTreeMap<u64, Page>,
}

impl Session {
    pub fn new(target: Target, page_size: u32) -> Self {
        Self {
            target,
            layouts: Vec::new(),
            layout_menu: Vec::new(),
            hidden_layouts: BTreeSet::new(),
            blank_start: false,
            hidden_named: false,
            layouts_loading: true,
            layout: None,
            geometry: Geometry::Loading,
            render: RenderState::Loading,
            page_size: page_size.max(1),
            page: None,
            pending: None,
            current: 0,
            table_current: BTreeMap::new(),
            preview_page: 0,
            paper: None,
            paper_revision: 0,
            report: Default::default(),
            pictures: crate::report::Pictures::drawn_apart(),
            cache: BTreeMap::new(),
            ranges: BTreeSet::new(),
            range_errors: BTreeMap::new(),
            revision: 0,
            shown: None,
        }
    }

    /// The found-set size, once a page has loaded.
    pub fn total(&self) -> Option<u64> {
        self.page.as_ref().map(|page| page.data.total)
    }

    /// The shown layout's definition (its view flags `f1`, layout-model.md
    /// 4.2, among them), once it has loaded.
    pub fn layout_def(&self) -> Option<&om_layout::layout::LayoutDef> {
        match &self.render {
            RenderState::Loaded(document) => document.layout.def.as_ref(),
            _ => None,
        }
    }

    pub fn view_enabled(&self, view: crate::app::ViewMode) -> bool {
        let flags = match &self.render {
            RenderState::Loaded(document) => {
                document.layout.def.as_ref().map_or(0, |def| def.flags)
            }
            _ => 0,
        };
        crate::scripting::View::from(view).enabled(flags)
    }

    pub fn layout_name(&self) -> Option<&str> {
        let id = self.layout?;
        self.layouts
            .iter()
            .find(|layout| layout.id == id)
            .map(|layout| layout.name.as_str())
    }

    pub fn is_loading_records(&self) -> bool {
        self.pending.is_some()
    }

    /// Accept the layouts list. Returns the first page to load if this picked
    /// a layout (the first one the account may see, as FileMaker opens on a
    /// default layout).
    pub fn set_layouts(&mut self, layouts: Vec<LayoutInfo>) -> Option<PageKey> {
        self.layouts_loading = false;
        self.layouts = layouts;
        // The shown layout stays while the file has it, also one the
        // account may not see (the window opened or went there, #696);
        // else the first one it may see.
        let still_present = self
            .layout
            .is_some_and(|id| self.layouts.iter().any(|layout| layout.id == id));
        if still_present {
            return None;
        }
        let first = self
            .layouts
            .iter()
            .find(|layout| self.shows_layout(layout.id))?
            .id;
        Some(self.select_layout(first))
    }

    /// [`Self::set_layouts`] for the account `schema` was read as: the
    /// layouts its custom set gives no access are hidden first.
    pub fn set_layouts_for(
        &mut self,
        layouts: Vec<LayoutInfo>,
        schema: Option<&CalcSchema>,
    ) -> Option<PageKey> {
        let access = schema.and_then(|schema| schema.authenticated_account.set_access.as_ref());
        self.hidden_layouts = layouts
            .iter()
            .map(|layout| layout.id)
            .filter(|id| access.is_some_and(|access| !access.shows_layout(u64::from(*id))))
            .collect();
        self.set_layouts(layouts)
    }

    /// Whether layout `id` may be selected.
    pub fn shows_layout(&self, id: u32) -> bool {
        !self.hidden_layouts.contains(&id)
    }

    /// Select the layout a file's first window opens on: `start` (File
    /// Options' "Switch to layout") when the file has it, else the first
    /// layout, whether or not the account's set hides it. FileMaker Pro
    /// 26.0.3 stays on a hidden opening layout with a blank gray page
    /// rather than move to one the account may see (session-identity.md
    /// 2.5, #696). `None` when the file has no layouts.
    pub fn open_on(&mut self, start: Option<u32>) -> Option<PageKey> {
        let id = start
            .filter(|id| self.layouts.iter().any(|layout| layout.id == *id))
            .or_else(|| self.layouts.first().map(|layout| layout.id))?;
        let key = self.select_layout(id);
        self.blank_start = !self.shows_layout(id);
        Some(key)
    }

    /// Switch layouts without table metadata. A different layout starts at
    /// its first record; selecting the current layout keeps its position.
    pub fn select_layout(&mut self, layout_id: u32) -> PageKey {
        self.select_layout_with_schema(layout_id, None)
    }

    /// Layouts of the same base table share the window's found set. Keep
    /// its current position, but reload the new layout's page and geometry.
    /// Another table's layout returns to the record that table showed last,
    /// the first record the first time; a layout whose table is unknown
    /// starts at the first record.
    pub fn select_layout_with_schema(
        &mut self,
        layout_id: u32,
        schema: Option<&CalcSchema>,
    ) -> PageKey {
        self.blank_start = false;
        self.hidden_named = false;
        let table = |layout_id| {
            let schema = schema?;
            let occurrence = schema
                .layouts()
                .iter()
                .find(|layout| layout.id == layout_id)?
                .occurrence?;
            schema.table_of(occurrence).map(|table| table.id)
        };
        let (old, new) = (self.layout.and_then(table), table(layout_id));
        let same_table = old.zip(new).is_some_and(|(old, new)| old == new);
        let current = if self.layout == Some(layout_id) || same_table {
            self.current
        } else {
            // Another table: back to the record it showed last, if any.
            if let Some(old) = old {
                self.table_current.insert(old, self.current);
            }
            new.and_then(|t| self.table_current.get(&t).copied())
                .unwrap_or(0)
        };
        self.revision = self.revision.wrapping_add(1);
        self.preview_page = 0;
        self.report.get_mut().take();
        self.pictures.clear();
        let shown = Shown {
            layout: self.layout.replace(layout_id),
            geometry: std::mem::replace(&mut self.geometry, Geometry::Loading),
            render: std::mem::replace(&mut self.render, RenderState::Loading),
            page: self.page.take(),
            current: self.current,
            cache: std::mem::take(&mut self.cache),
        };
        // Keep the last layout that was on screen, not a half-loaded one.
        if shown.page.is_some() && self.shown.is_none() {
            self.shown = Some(Box::new(shown));
        }
        self.ranges.clear();
        self.range_errors.clear();
        self.current = current;
        self.request(self.page_offset(current))
    }

    /// The connection to the host is gone: no reply is coming. Put back the
    /// layout that was on screen if a switch was still loading (returning
    /// whether it did), and keep the current record on the loaded page.
    pub fn connection_lost(&mut self) -> bool {
        let restored = self.shown.is_some();
        if let Some(shown) = self.shown.take() {
            self.revision = self.revision.wrapping_add(1);
            self.layout = shown.layout;
            self.geometry = shown.geometry;
            self.render = shown.render;
            self.page = Some(shown.page.expect("only shown layouts are kept"));
            self.current = shown.current;
            self.cache = shown.cache;
        }
        self.pending = None;
        self.ranges.clear();
        self.range_errors.clear();
        if let Some(page) = &self.page {
            if !self.page_holds(self.current) && !page.data.records.is_empty() {
                let last = page.key.offset + page.data.records.len() as u64 - 1;
                self.current = self.current.clamp(page.key.offset, last);
            }
        }
        restored
    }

    /// The pages received for the selected layout: the one on screen, then
    /// the others kept.
    pub fn loaded_pages(&self) -> impl Iterator<Item = &Page> {
        self.page.iter().chain(self.cache.values())
    }

    /// Make record `index` current, loading its page if needed. The index is
    /// clamped to the found set.
    pub fn go_to(&mut self, index: u64) -> Option<PageKey> {
        self.layout?;
        let index = match self.total() {
            Some(total) => index.min(total.saturating_sub(1)),
            None => index,
        };
        self.current = index;
        if let Some(page) = self.cache.get(&self.page_offset(index)).cloned() {
            self.page = Some(page);
        }
        if !self.page_holds(index) {
            return Some(self.request(self.page_offset(index)));
        }
        let on_screen = self.page.as_ref()?.key;
        if on_screen.revision == self.revision {
            // A request for some other page may still be in flight; the
            // record is already here, so let that reply be ignored.
            self.pending = None;
            None
        } else if self
            .pending
            .is_some_and(|key| key.offset == on_screen.offset)
        {
            // The page on screen predates a save or refresh whose reload is
            // still in flight. Dropping that reply would leave the old
            // values up, so keep waiting for it.
            None
        } else {
            Some(self.request(on_screen.offset))
        }
    }

    /// Put `record`'s values of `fields` into the loaded pages of layout
    /// `layout_id` that hold the record: what a save of those fields kept,
    /// shown until the pages are read again (#783). The saved record lists
    /// its values in the page's field order; one of another length is not
    /// taken.
    pub fn show_saved(&mut self, layout_id: u32, record: &Record, fields: &[String]) {
        if self.layout != Some(layout_id) {
            return;
        }
        for page in self.page.iter_mut().chain(self.cache.values_mut()) {
            let Some(row) = page.data.records.iter_mut().find(|r| r.id == record.id) else {
                continue;
            };
            if row.values.len() != record.values.len() {
                continue;
            }
            for name in fields {
                let Some(index) = page.data.fields.iter().position(|f| f.name == *name) else {
                    continue;
                };
                if let (Some(cell), Some(saved)) =
                    (row.values.get_mut(index), record.values.get(index))
                {
                    cell.clone_from(saved);
                }
            }
        }
    }

    /// Reload the page holding the current record, e.g. for Refresh Window.
    pub fn refresh(&mut self) -> Option<PageKey> {
        self.layout?;
        self.revision = self.revision.wrapping_add(1);
        self.cache.clear();
        self.ranges.clear();
        self.range_errors.clear();
        Some(self.request(self.page_offset(self.current)))
    }

    /// Take a `records` reply. Replies for anything but the pending request
    /// are ignored. May return a follow-up request when the found set shrank
    /// past the current record.
    #[cfg(test)]
    pub fn accept_page(
        &mut self,
        key: PageKey,
        result: Result<RecordPage, DataError>,
    ) -> Result<Option<PageKey>, DataError> {
        self.accept_page_with(key, result, om_data::NoAccess::default())
    }

    /// [`Self::accept_page`] of a page whose records and fields
    /// `no_access` names show `<No Access>`.
    pub fn accept_page_with(
        &mut self,
        key: PageKey,
        result: Result<RecordPage, DataError>,
        no_access: om_data::NoAccess,
    ) -> Result<Option<PageKey>, DataError> {
        let primary = self.pending == Some(key);
        if !primary && !self.ranges.remove(&key) {
            return Ok(None);
        }
        if primary {
            self.pending = None;
            self.shown = None;
        }
        let data = result.inspect_err(|error| {
            self.range_errors.insert(key.offset, error.message.clone());
        })?;
        let expected = u64::from(key.limit).min(data.total.saturating_sub(key.offset));
        if data.records.len() as u64 != expected {
            self.range_errors.insert(
                key.offset,
                "The provider returned an incomplete report record range.".into(),
            );
        }
        self.cache.insert(
            key.offset,
            Page {
                key,
                data: data.clone(),
                no_access: no_access.clone(),
            },
        );
        if !primary {
            if let Some(page) = &mut self.page {
                page.data.total = data.total;
            }
            return Ok(None);
        }
        let total = data.total;
        self.page = Some(Page {
            key,
            data,
            no_access,
        });
        if total == 0 {
            self.current = 0;
            return Ok(None);
        }
        self.current = self.current.min(total - 1);
        if self.page_holds(self.current) {
            Ok(None)
        } else {
            Ok(Some(self.request(self.page_offset(self.current))))
        }
    }

    /// Take a `layout_geometry` reply. Replies for a layout other than the
    /// selected one are ignored. An error leaves the layout without geometry.
    pub fn accept_geometry(
        &mut self,
        layout_id: u32,
        result: Result<Option<LayoutGeometry>, DataError>,
    ) -> Result<(), DataError> {
        if self.layout != Some(layout_id) {
            return Ok(());
        }
        let (geometry, result) = match result {
            Ok(Some(geometry)) => (Geometry::Loaded(geometry), Ok(())),
            Ok(None) => (Geometry::Unavailable, Ok(())),
            Err(err) => (Geometry::Unavailable, Err(err)),
        };
        self.geometry = geometry;
        result
    }

    pub fn accept_render_layout(
        &mut self,
        layout_id: u32,
        result: Result<Option<RenderLayout>, DataError>,
    ) -> Result<(), DataError> {
        if self.layout != Some(layout_id) {
            return Ok(());
        }
        self.render = RenderState::Unavailable;
        self.report.get_mut().take();
        self.pictures.clear();
        if let Some(layout) = result? {
            self.render = RenderState::Loaded(Box::new(layout));
        }
        Ok(())
    }

    /// The first record index of the page holding `index`.
    fn page_offset(&self, index: u64) -> u64 {
        let size = u64::from(self.page_size);
        index / size * size
    }

    /// Whether the loaded page is for the selected layout and holds `index`.
    fn page_holds(&self, index: u64) -> bool {
        self.page.as_ref().is_some_and(|page| {
            Some(page.key.layout_id) == self.layout
                && page.key.limit == self.page_size
                && index >= page.key.offset
                && index - page.key.offset < page.data.records.len() as u64
        })
    }

    fn request(&mut self, offset: u64) -> PageKey {
        let key = PageKey {
            revision: self.revision,
            layout_id: self.layout.unwrap_or_default(),
            offset,
            limit: self.page_size,
        };
        self.pending = Some(key);
        key
    }

    /// The fields and the current record, if its page is loaded.
    pub fn current_record(&self) -> Option<(&[Field], &Record)> {
        self.record_at(self.current)
    }

    fn record_page(&self, index: u64) -> Option<&Page> {
        self.cache
            .get(&self.page_offset(index))
            .or(self.page.as_ref())
            .filter(|page| {
                index >= page.key.offset && index - page.key.offset < page.data.records.len() as u64
            })
    }

    /// The accepted page supplying this record, independent of pending requests.
    pub fn record_page_key(&self, index: u64) -> Option<PageKey> {
        self.record_page(index).map(|page| page.key)
    }

    /// The version supplied by the accepted page, before its editor takes input.
    pub(crate) fn record_version_at(&self, index: u64) -> Option<u64> {
        let page = self.record_page(index)?;
        let (_, record) = self.record_at(index)?;
        page.data.versions.get(&record.id).copied()
    }

    /// `Get ( RecordAccess )` for the record at `index`, from its page's
    /// judgment of the account's set (`om_data::NoAccess::record_access`).
    pub fn record_access_at(&self, index: u64) -> Option<i64> {
        let page = self.record_page(index)?;
        let record = page
            .data
            .records
            .get(usize::try_from(index - page.key.offset).ok()?)?;
        page.no_access.record_access(record.id)
    }

    /// Whether field `field` of the record at `index` shows `<No Access>`:
    /// a record the account may not view, or a field without access.
    /// Whether the shown page's layout gives its records no access: its
    /// fields show `<No Access>` and take no editor at all (#707).
    pub fn layout_masked(&self) -> bool {
        self.page.as_ref().is_some_and(|page| page.no_access.layout)
    }

    pub fn no_access(&self, index: u64, field: &str) -> bool {
        let Some(page) = self.record_page(index) else {
            return false;
        };
        usize::try_from(index - page.key.offset)
            .ok()
            .and_then(|i| page.data.records.get(i))
            .is_some_and(|record| page.no_access.hides(record.id, field))
    }

    pub fn record_at(&self, index: u64) -> Option<(&[Field], &Record)> {
        let page = self.record_page(index)?;
        Some((
            &page.data.fields,
            page.data
                .records
                .get(usize::try_from(index - page.key.offset).ok()?)?,
        ))
    }

    /// Visible list rows request missing pages without moving the current record.
    pub fn request_range(&mut self, index: u64) -> Option<PageKey> {
        let layout_id = self.layout?;
        if self.record_at(index).is_some() || self.total().is_some_and(|total| index >= total) {
            return None;
        }
        let key = PageKey {
            revision: self.revision,
            layout_id,
            offset: self.page_offset(index),
            limit: self.page_size,
        };
        if self.pending == Some(key) || !self.ranges.insert(key) {
            return None;
        }
        Some(key)
    }

    #[cfg(test)]
    pub fn page_position(&self) -> Option<(u64, u64)> {
        let total = self.total()?;
        let size = u64::from(self.page_size);
        Some((self.current / size + 1, total.div_ceil(size).max(1)))
    }

    /// The loaded layout and found count a report is planned from; `None`
    /// while loading. Never borrow stale rows while a reload is pending.
    fn report_found(&self) -> Result<Option<(&RenderLayout, u64)>, String> {
        let document = match &self.render {
            RenderState::Loading => return Ok(None),
            RenderState::Unavailable => {
                return Err(
                    "This provider has no printable layout. Exact Preview is unavailable.".into(),
                )
            }
            RenderState::Loaded(document) => &**document,
        };
        if self.pending.is_some() {
            return Ok(None);
        }
        if let Some(error) = self.range_errors.get(&self.page_offset(self.current)) {
            return Err(format!("Report records could not be loaded: {error}"));
        }
        if self
            .page
            .as_ref()
            .is_some_and(|p| p.key.revision != self.revision)
        {
            return Err("Report records must be reloaded before Preview is available.".into());
        }
        let Some(total) = self.total() else {
            return Ok(None);
        };
        if total == 0 {
            return Err("No records in the found set.".into());
        }
        Ok(Some((document, total)))
    }

    /// This found set's report cache, replacing one of another found set.
    fn cached_report(&self, total: u64) -> std::cell::RefMut<'_, CachedReport> {
        let mut cache = self.report.borrow_mut();
        if cache
            .as_ref()
            .is_none_or(|cached| cached.revision != self.revision || cached.count != total)
        {
            *cache = Some(CachedReport {
                revision: self.revision,
                count: total,
                groups: None,
                plan: None,
                list: None,
            });
        }
        std::cell::RefMut::map(cache, |cache| cache.as_mut().unwrap())
    }

    /// The found count when the layout has sub-summary parts whose groups
    /// this found set has not read yet: the caller reads them with the
    /// window's calculations and passes them to
    /// [`Self::accept_report_groups`]. An ungrouped layout needs none.
    pub fn report_groups_wanted(&self) -> Option<u64> {
        let (document, total) = self.report_found().ok()??;
        if om_layout::pagination::report_breaks(&document.layout).is_ok_and(|b| b.is_empty())
            && !crate::report::resizes_parts(&document.layout)
        {
            return None;
        }
        self.cached_report(total).groups.is_none().then_some(total)
    }

    /// A report whose parts resize ([`crate::report::resizes_parts`]) is
    /// measured with the window's calculations once its found set's records
    /// are read: this found set's records and groups, and the paper, until
    /// [`Self::accept_report_plan`] takes the plan.
    pub fn report_measure_wanted(
        &self,
    ) -> Option<(
        u64,
        crate::report::FoundSetGroups,
        om_layout::pagination::PageSize,
    )> {
        let (document, total) = self.report_found().ok()??;
        if !crate::report::resizes_parts(&document.layout) {
            return None;
        }
        let paper = self.report_paper(&document.layout).ok()?;
        let cached = self.cached_report(total);
        match (&cached.groups, &cached.plan) {
            (Some(Ok(found)), None) => Some((total, found.clone(), paper)),
            _ => None,
        }
    }

    /// A measured plan for this found set; a newer found set or data
    /// generation has already replaced the cache it was measured for.
    pub fn accept_report_plan(
        &self,
        total: u64,
        plan: Result<om_layout::pagination::ReportPlan, String>,
    ) {
        let mut cached = self.cached_report(total);
        if cached.groups.is_some() {
            cached.plan = Some(plan.map(std::sync::Arc::new));
        }
    }

    /// The report's paper: Page Setup's for this file session, else the
    /// layout's own orientation and margins on US Letter.
    pub fn report_paper(
        &self,
        layout: &om_layout::Layout,
    ) -> Result<om_layout::pagination::PageSize, String> {
        self.paper.as_ref().map_or_else(
            || om_layout::pagination::PageSize::for_layout(layout),
            |paper| paper.for_layout(layout),
        )
    }

    /// Provider data or calculation context can change break values and
    /// measured heights without changing the found set's ids or count.
    /// Page navigation and paper-only changes retain groups; changed data,
    /// globals and invalidated object calculations do not.
    pub fn report_data_changed(&self) {
        *self.report.borrow_mut() = None;
    }

    /// The found count when List view has not read this found set's
    /// sub-summary groups: the layout has sub-summary parts and the found
    /// set's first page is in. The caller reads them with the window's
    /// calculations and passes them to [`Self::accept_list_groups`].
    pub fn list_groups_wanted(&self) -> Option<u64> {
        let RenderState::Loaded(document) = &self.render else {
            return None;
        };
        if self.pending.is_some()
            || self
                .page
                .as_ref()
                .is_none_or(|page| page.key.revision != self.revision)
        {
            return None;
        }
        let total = self.total().filter(|total| *total > 0)?;
        if !document.layout.parts.iter().any(|part| {
            matches!(
                part.kind(),
                om_layout::PartKind::LeadingSubSummary | om_layout::PartKind::TrailingSubSummary
            )
        }) {
            return None;
        }
        self.cached_report(total).list.is_none().then_some(total)
    }

    pub fn accept_list_groups(&self, total: u64, groups: ListGroupsResult) {
        self.cached_report(total).list = Some(groups);
    }

    /// The sub-summary groups List view shows for this found set; `None`
    /// until read. Groups whose records are not those of a loaded page of
    /// this found set, at the same places, are stale and not shown.
    pub fn list_groups(&self) -> Option<ListGroupsResult> {
        let total = self.total()?;
        let list = {
            let cache = self.report.borrow();
            let cached = cache
                .as_ref()
                .filter(|c| c.revision == self.revision && c.count == total)?;
            cached.list.clone()?
        };
        if let Ok(Some(groups)) = &list {
            let stale = self
                .loaded_pages()
                .filter(|page| page.key.revision == self.revision)
                .any(|page| {
                    page.data.records.iter().enumerate().any(|(i, record)| {
                        usize::try_from(page.key.offset)
                            .ok()
                            .and_then(|offset| groups.records.get(offset + i))
                            .and_then(|id| i64::try_from(*id).ok())
                            != Some(record.id)
                    })
                });
            if stale {
                return Some(Err(
                    "The found set changed while grouping its records".into()
                ));
            }
        }
        Some(list)
    }

    pub fn accept_report_groups(&self, total: u64, groups: GroupsResult) {
        let mut cached = self.cached_report(total);
        cached.groups = Some(groups);
        cached.plan = None;
    }

    /// The record id the report's groups were read for at found-set
    /// `index`; `None` without grouping.
    pub fn report_group_record(&self, index: u64) -> Option<u64> {
        let cache = self.report.borrow();
        let cached = cache.as_ref().filter(|c| c.revision == self.revision)?;
        let (records, _) = cached.groups.as_ref()?.as_ref().ok()?;
        records.get(usize::try_from(index).ok()?).copied()
    }

    /// A fixed-height report can be planned from the found count, before its
    /// record bodies arrive. Sub-summary parts also need the found set's
    /// groups ([`Self::report_groups_wanted`]); until then this is `None`.
    /// So is a report whose parts resize, until it is measured
    /// ([`Self::report_measure_wanted`]).
    pub fn report_plan(
        &self,
    ) -> Result<Option<std::sync::Arc<om_layout::pagination::ReportPlan>>, String> {
        use om_layout::pagination::{paginate, report_breaks};
        let Some((document, total)) = self.report_found()? else {
            return Ok(None);
        };
        let paper = self.report_paper(&document.layout);
        let mut cached = self.cached_report(total);
        let CachedReport { groups, plan, .. } = &mut *cached;
        if plan.is_none() && crate::report::resizes_parts(&document.layout) {
            return match groups {
                Some(Err(error)) => Err(error.clone()),
                _ => paper.map(|_| None),
            };
        }
        if plan.is_none() {
            let breaks = match report_breaks(&document.layout) {
                Ok(fields) if fields.is_empty() => Ok(&[][..]),
                Ok(_) => match groups {
                    None => return Ok(None),
                    Some(groups) => groups
                        .as_ref()
                        .map(|(_, g)| g.as_slice())
                        .map_err(Clone::clone),
                },
                Err(error) => Err(error),
            };
            *plan = Some(
                breaks
                    .and_then(|breaks| {
                        let paper = paper?;
                        let count = usize::try_from(total)
                            .map_err(|_| "The found set is too large to preview")?;
                        paginate(&document.layout, count, paper, breaks)
                    })
                    .map(std::sync::Arc::new),
            );
        }
        plan.clone().unwrap().map(Some)
    }

    pub fn set_paper(&mut self, paper: om_layout::paper::PaperSettings) {
        if self.paper.as_ref() != Some(&paper) {
            self.paper = Some(paper);
            self.paper_revision = self.paper_revision.wrapping_add(1);
            // Paper changes the pages, not the found set's groups.
            if let Some(cached) = self.report.get_mut() {
                cached.plan = None;
            }
            self.preview_page = 0;
        }
    }

    /// Pictures decoded for this session's report pages. Entries are checked
    /// against the layout's encoded images, so a restored or reloaded snapshot
    /// never shows another snapshot's pixels.
    pub fn report_pictures(&self) -> &crate::report::Pictures {
        &self.pictures
    }

    pub fn preview_record_error(&self, index: u64) -> Option<&str> {
        self.range_errors
            .get(&self.page_offset(index))
            .map(String::as_str)
    }

    pub fn preview_position(&self) -> Option<(u64, u64)> {
        let plan = self.report_plan().ok()??;
        let count = plan.pages.len() as u64;
        Some((self.preview_page.min(count.saturating_sub(1)), count))
    }

    pub fn go_to_preview(&mut self, page: u64) {
        if let Some((_, count)) = self.preview_position() {
            self.preview_page = page.min(count.saturating_sub(1));
        }
    }

    pub fn preview_identity(&self) -> (Option<u32>, u64, u64, u64) {
        (
            self.layout,
            self.revision,
            self.preview_position().map_or(0, |p| p.0),
            self.paper_revision,
        )
    }
}

#[cfg(test)]
mod tests {
    use om_data::FieldKind;

    use super::*;

    fn target() -> Target {
        Target {
            host: "fms.local".into(),
            file: "Contacts.fmp12".into(),
            local: None,
        }
    }

    #[test]
    fn styled_layout_replies_follow_selection_and_fail_explicitly() {
        let mut session = Session::new(target(), 25);
        session.select_layout(7);
        session
            .accept_render_layout(8, Ok(Some(RenderLayout::default())))
            .unwrap();
        assert_eq!(session.render, RenderState::Loading);
        session
            .accept_render_layout(7, Ok(Some(RenderLayout::default())))
            .unwrap();
        assert!(matches!(session.render, RenderState::Loaded(_)));
        session.select_layout(8);
        assert_eq!(session.render, RenderState::Loading);
        assert!(session
            .accept_render_layout(8, Err(DataError::new("decode failed")))
            .is_err());
        assert_eq!(session.render, RenderState::Unavailable);
    }

    fn page(offset: u64, len: u64, total: u64) -> RecordPage {
        RecordPage {
            versions: Default::default(),
            fields: vec![Field {
                name: "Name".into(),
                kind: FieldKind::Text,
            }],
            records: (offset..offset + len)
                .map(|i| Record {
                    id: i as i64 + 1,
                    values: vec![Some(format!("r{i}"))],
                })
                .collect(),
            total,
        }
    }

    fn layouts() -> Vec<LayoutInfo> {
        vec![
            LayoutInfo {
                id: 1,
                name: "A".into(),
            },
            LayoutInfo {
                id: 2,
                name: "B".into(),
            },
        ]
    }

    /// A session on layout 1 with the first page (10 of 42) loaded.
    fn loaded() -> Session {
        let mut session = Session::new(target(), 10);
        let key = session.set_layouts(layouts()).unwrap();
        assert_eq!(key.offset, 0);
        session.accept_page(key, Ok(page(0, 10, 42))).unwrap();
        session
    }

    #[test]
    fn measured_report_plans_wait_for_records_and_paper_and_surface_paper_errors() {
        use om_layout::{sliding, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect};
        let mut session = loaded();
        let field = LayoutObject {
            id: 1,
            object_type: 1,
            flags: sliding::UP | sliding::RESIZE_PART,
            bounds: Some(Rect {
                top: 10.0,
                bottom: 28.0,
                right: 100.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: vec![Part {
                part_type: 4,
                height: 40.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: vec![1],
                objects: vec![field],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(crate::report::resizes_parts(&layout));
        let render = |layout: &Layout| {
            Ok(Some(RenderLayout {
                layout: layout.clone(),
                ..Default::default()
            }))
        };
        let records = || Ok(((1..=42).collect::<Vec<u64>>().into(), Vec::new()));
        session.accept_render_layout(1, render(&layout)).unwrap();
        // The found set's records come first; the plan waits for its measure.
        assert_eq!(session.report_groups_wanted(), Some(42));
        assert!(session.report_measure_wanted().is_none());
        assert!(matches!(session.report_plan(), Ok(None)));
        session.accept_report_groups(42, records());
        let (total, found, paper) = session.report_measure_wanted().unwrap();
        assert_eq!((total, found.0.len()), (42, 42));
        let plan = om_layout::pagination::paginate(&layout, 42, paper, &[]).unwrap();
        session.accept_report_plan(42, Ok(plan.clone()));
        assert_eq!(*session.report_plan().unwrap().unwrap(), plan);
        assert!(session.report_measure_wanted().is_none());
        // Other paper keeps the records and measures again.
        let mut landscape = om_layout::paper::PaperSettings::initial(&layout).unwrap();
        landscape.orientation = om_layout::paper::Orientation::Landscape;
        session.set_paper(landscape);
        assert!(matches!(session.report_plan(), Ok(None)));
        assert_eq!(session.report_measure_wanted().unwrap().2.width, 792.0);
        // Paper the layout cannot use is an error, never endless loading.
        layout.def.as_mut().unwrap().orientation = 99;
        session.paper = None;
        session.accept_render_layout(1, render(&layout)).unwrap();
        session.accept_report_groups(42, records());
        assert!(session.report_measure_wanted().is_none());
        assert_eq!(
            session.report_plan().unwrap_err(),
            "Unsupported layout page orientation"
        );
    }

    #[test]
    fn a_lost_connection_puts_back_the_layout_a_switch_was_loading() {
        let mut session = loaded();
        session.current = 3;
        let switch = session.select_layout(2);
        assert!(session.page.is_none());
        assert!(session.connection_lost());
        assert_eq!(session.layout, Some(1));
        assert_eq!(session.current, 3);
        assert_eq!(session.current_record().unwrap().1.id, 4);
        assert!(session.pending.is_none());
        // The switch's reply, if one still came, is ignored.
        assert_eq!(
            session.accept_page(switch, Ok(page(0, 10, 42))).unwrap(),
            None
        );
        assert_eq!(session.layout, Some(1));
        assert!(!session.connection_lost(), "nothing more to put back");
    }

    #[test]
    fn a_loaded_switch_is_kept_when_the_connection_is_lost_later() {
        let mut session = loaded();
        let switch = session.select_layout(2);
        session.accept_page(switch, Ok(page(0, 10, 42))).unwrap();
        assert!(!session.connection_lost());
        assert_eq!(session.layout, Some(2));
    }

    #[test]
    fn a_lost_connection_keeps_the_current_record_on_the_loaded_page() {
        let mut session = loaded();
        // Next past the page: its request never comes back.
        assert!(session.go_to(10).is_some());
        assert!(session.current_record().is_none());
        session.connection_lost();
        assert_eq!(session.current, 9);
        assert_eq!(session.current_record().unwrap().1.id, 10);
    }

    #[test]
    fn layouts_select_the_first_and_request_its_first_page() {
        let mut session = Session::new(target(), 25);
        let key = session.set_layouts(layouts()).unwrap();
        assert_eq!(
            key,
            PageKey {
                revision: 1,
                layout_id: 1,
                offset: 0,
                limit: 25
            }
        );
        assert_eq!(session.layout_name(), Some("A"));
        assert!(session.is_loading_records());
    }

    /// FileMaker Pro 26.0.3 stays on a hidden opening layout, named by
    /// File Options or the first, with a blank page (#696).
    #[test]
    fn a_window_opens_on_its_start_layout_even_when_the_set_hides_it() {
        let mut session = Session::new(target(), 25);
        session.hidden_layouts = [1].into();
        assert_eq!(session.set_layouts(layouts()).unwrap().layout_id, 2);
        // File Options name it, or name none or a layout that is gone.
        for start in [Some(1), None, Some(9)] {
            assert_eq!(session.open_on(start).unwrap().layout_id, 1);
            assert!(session.blank_start);
            assert_eq!(session.layout_name(), Some("A"));
        }
        assert_eq!(session.open_on(Some(2)).unwrap().layout_id, 2);
        assert!(!session.blank_start);
        // A new layout list (a reconnect, a layout added) keeps it.
        session.open_on(None);
        assert_eq!(session.set_layouts(layouts()), None);
        assert_eq!(session.layout, Some(1));
        assert!(session.blank_start);
        // Leaving the hidden opening layout ends the blank page, and the
        // name Go to Related Record gave a hidden layout (#696).
        session.hidden_named = true;
        session.select_layout(2);
        assert!(!session.blank_start && !session.hidden_named);
        // A list without the current layout goes to the first it may see.
        session.open_on(None);
        let others = layouts().into_iter().filter(|l| l.id != 1).collect();
        assert_eq!(session.set_layouts(others).unwrap().layout_id, 2);
        assert!(!session.blank_start);
    }

    #[test]
    fn navigation_within_a_page_needs_no_request() {
        let mut session = loaded();
        assert_eq!(session.go_to(9), None);
        assert_eq!(session.current_record().unwrap().1.id, 10);
    }

    #[test]
    fn scrolling_loads_other_pages_without_moving_the_current_record() {
        let mut session = loaded();
        let key = session.request_range(28).unwrap();
        assert_eq!(key.offset, 20);
        assert!(
            session.request_range(29).is_none(),
            "one request per fetch window"
        );
        session.accept_page(key, Ok(page(20, 10, 42))).unwrap();
        assert_eq!(session.current, 0);
        assert_eq!(session.current_record().unwrap().1.id, 1);
        assert_eq!(session.record_at(28).unwrap().1.id, 29);
        assert!(
            session.go_to(28).is_none(),
            "navigation reuses the fetched page"
        );
        assert_eq!(session.current_record().unwrap().1.id, 29);
        assert!(
            session.go_to(0).is_none(),
            "the earlier page remains cached"
        );
    }

    #[test]
    fn refresh_cannot_accept_an_older_reply_for_the_same_fetch_window() {
        let mut session = loaded();
        let older = session.refresh().unwrap();
        let latest = session.refresh().unwrap();
        assert_ne!(older, latest);
        session.accept_page(older, Ok(page(0, 10, 100))).unwrap();
        assert_eq!(session.pending, Some(latest));
        assert_eq!(session.total(), Some(42));
        session.accept_page(latest, Ok(page(0, 10, 43))).unwrap();
        assert_eq!(session.total(), Some(43));
    }

    #[test]
    fn layout_changes_and_refreshes_invalidate_scrolled_pages() {
        let mut session = loaded();
        let old = session.request_range(28).unwrap();
        session.select_layout(2);
        session.accept_page(old, Ok(page(20, 10, 42))).unwrap();
        assert!(session.record_at(28).is_none());
        let first = session.pending.unwrap();
        session.accept_page(first, Ok(page(0, 10, 42))).unwrap();
        let range = session.request_range(28).unwrap();
        session.accept_page(range, Ok(page(20, 10, 42))).unwrap();
        session.refresh();
        assert!(session.record_at(28).is_none());
    }

    #[test]
    fn navigation_past_the_page_requests_the_next_page() {
        let mut session = loaded();
        let key = session.go_to(10).unwrap();
        assert_eq!(key.offset, 10);
        assert!(session.current_record().is_none());
        session.accept_page(key, Ok(page(10, 10, 42))).unwrap();
        assert_eq!(session.current_record().unwrap().1.id, 11);
        assert_eq!(session.page_position(), Some((2, 5)));
    }

    #[test]
    fn navigation_is_clamped_to_the_found_set() {
        let mut session = loaded();
        let key = session.go_to(1_000).unwrap();
        assert_eq!(session.current, 41);
        assert_eq!(key.offset, 40);
    }

    #[test]
    fn stale_replies_are_ignored() {
        let mut session = loaded();
        let first = session.go_to(15).unwrap();
        let second = session.go_to(25).unwrap();
        assert_eq!(
            session.accept_page(first, Ok(page(10, 10, 42))).unwrap(),
            None
        );
        assert!(session.current_record().is_none());
        session.accept_page(second, Ok(page(20, 10, 42))).unwrap();
        assert_eq!(session.current_record().unwrap().1.id, 26);
    }

    #[test]
    fn refresh_reloads_the_current_page_and_keeps_the_record() {
        let mut session = loaded();
        let key = session.go_to(17).unwrap();
        session.accept_page(key, Ok(page(10, 10, 42))).unwrap();
        let refresh = session.refresh().unwrap();
        assert_eq!(refresh.offset, 10);
        // The old page stays on screen until the reply lands.
        assert_eq!(session.current_record().unwrap().1.id, 18);
        session.accept_page(refresh, Ok(page(10, 10, 42))).unwrap();
        assert_eq!(session.current_record().unwrap().1.id, 18);
        assert!(!session.is_loading_records());
    }

    #[test]
    fn navigating_within_a_reloading_page_keeps_its_reload() {
        let mut session = loaded();
        // A save reloads the page; the user moves before the reply lands.
        let reload = session.refresh().unwrap();
        assert_eq!(session.go_to(5), None);
        assert_eq!(session.pending, Some(reload));
        let mut saved = page(0, 10, 42);
        saved.records[5].values[0] = Some("saved".into());
        session.accept_page(reload, Ok(saved)).unwrap();
        assert_eq!(
            session.current_record().unwrap().1.values[0].as_deref(),
            Some("saved")
        );
        // A current page still lets a request for another page go.
        session.go_to(15).unwrap();
        assert_eq!(session.go_to(3), None);
        assert!(!session.is_loading_records());
    }

    #[test]
    fn returning_to_a_reloading_page_requests_it_again() {
        let mut session = loaded();
        session.refresh().unwrap();
        let other = session.go_to(15).unwrap();
        // Back before either reply: the reload was superseded, so the old
        // page on screen needs a new one.
        let reload = session.go_to(5).unwrap();
        assert_eq!((reload.offset, session.pending), (0, Some(reload)));
        session.accept_page(other, Ok(page(10, 10, 42))).unwrap();
        let mut saved = page(0, 10, 42);
        saved.records[5].values[0] = Some("saved".into());
        session.accept_page(reload, Ok(saved)).unwrap();
        assert_eq!(
            session.current_record().unwrap().1.values[0].as_deref(),
            Some("saved")
        );
    }

    #[test]
    fn a_shrunken_found_set_refetches_the_last_page() {
        let mut session = loaded();
        let key = session.go_to(35).unwrap();
        // The file now has only 12 records; the reply for offset 30 is empty.
        let refetch = session
            .accept_page(key, Ok(page(30, 0, 12)))
            .unwrap()
            .unwrap();
        assert_eq!(session.current, 11);
        assert_eq!(refetch.offset, 10);
    }

    #[test]
    fn errors_clear_the_pending_request() {
        let mut session = loaded();
        let key = session.go_to(30).unwrap();
        assert!(session
            .accept_page(key, Err(DataError::new("boom")))
            .is_err());
        assert!(!session.is_loading_records());
    }

    #[test]
    fn switching_layouts_starts_over() {
        let mut session = loaded();
        session.go_to(5);
        session.accept_geometry(1, Ok(None)).unwrap();
        let key = session.select_layout(2);
        assert_eq!(key.layout_id, 2);
        assert_eq!(key.offset, 0);
        assert_eq!(session.current, 0);
        assert!(session.page.is_none());
        assert_eq!(session.geometry, Geometry::Loading);
    }

    fn shared_layout_schema() -> CalcSchema {
        let mut schema = CalcSchema::new("Layouts", om_data::calc::Locale::en_us());
        schema.add_table(0x8001, "Contacts");
        schema.add_table(0x8002, "Tasks");
        schema.add_occurrence(0xd00001, "Contacts", 0x8001);
        schema.add_occurrence(0xd00002, "Contact alias", 0x8001);
        schema.add_occurrence(0xd00003, "Tasks", 0x8002);
        schema.add_layout(1, "Details", Some(0xd00001));
        schema.add_layout(2, "List", Some(0xd00002));
        schema.add_layout(3, "Tasks", Some(0xd00003));
        schema.add_layout(4, "Unknown", None);
        schema
    }

    #[test]
    fn same_table_layout_switch_keeps_the_record_and_fetches_its_new_page() {
        let mut session = loaded();
        let old = session.go_to(27).unwrap();
        let key = session.select_layout_with_schema(2, Some(&shared_layout_schema()));
        assert_eq!((key.layout_id, key.offset, session.current), (2, 20, 27));
        assert!(session.page.is_none());
        assert!(session
            .accept_page(old, Ok(page(20, 10, 42)))
            .unwrap()
            .is_none());
        assert_eq!(session.pending, Some(key));
        assert!(
            session.page.is_none(),
            "the old layout's columns must not reappear"
        );
        session.accept_page(key, Ok(page(20, 10, 42))).unwrap();
        assert_eq!(session.current_record().unwrap().1.id, 28);
    }

    #[test]
    fn different_or_unknown_layout_tables_start_at_the_first_record() {
        for layout in [3, 4, 99] {
            let mut session = loaded();
            session.go_to(5);
            let key = session.select_layout_with_schema(layout, Some(&shared_layout_schema()));
            assert_eq!((key.offset, session.current), (0, 0));
        }
    }

    #[test]
    fn selecting_the_current_layout_keeps_its_record_without_a_schema() {
        let mut session = loaded();
        session.go_to(5);
        let key = session.select_layout(1);
        assert_eq!((key.offset, session.current), (0, 5));
    }

    #[test]
    fn a_preserved_position_is_clamped_if_the_new_reply_has_a_smaller_found_set() {
        let mut session = loaded();
        session.go_to(27);
        let key = session.select_layout_with_schema(2, Some(&shared_layout_schema()));
        let follow = session
            .accept_page(key, Ok(page(20, 0, 3)))
            .unwrap()
            .unwrap();
        assert_eq!(
            (follow.layout_id, follow.offset, session.current),
            (2, 0, 2)
        );
        session.accept_page(follow, Ok(page(0, 3, 3))).unwrap();
        assert_eq!(session.current_record().unwrap().1.id, 3);
    }

    #[test]
    fn geometry_replies_apply_only_to_the_selected_layout() {
        let mut session = loaded();
        session.select_layout(2);
        // A late reply for the layout just left.
        let stale = LayoutGeometry {
            width: 100.0,
            ..LayoutGeometry::default()
        };
        session.accept_geometry(1, Ok(Some(stale))).unwrap();
        assert_eq!(session.geometry, Geometry::Loading);
        session
            .accept_geometry(2, Ok(Some(LayoutGeometry::default())))
            .unwrap();
        assert_eq!(
            session.geometry,
            Geometry::Loaded(LayoutGeometry::default())
        );
    }

    #[test]
    fn missing_or_failed_geometry_falls_back() {
        let mut session = loaded();
        session.accept_geometry(1, Ok(None)).unwrap();
        assert_eq!(session.geometry, Geometry::Unavailable);
        session.select_layout(1);
        assert!(session
            .accept_geometry(1, Err(DataError::new("bad layout")))
            .is_err());
        assert_eq!(session.geometry, Geometry::Unavailable);
    }
}
