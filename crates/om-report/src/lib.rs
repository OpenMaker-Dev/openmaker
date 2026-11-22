//! The interface between a host's data and OpenMaker's report renderer:
//! what a report asks of the records it prints ([`ReportContent`]), in
//! which record, page, group and portal row it asks ([`ContentContext`]),
//! and what rendering gives back ([`PdfReport`], [`ReportError`]).
//!
//! The renderer itself is the desktop client's (`om_gui::report`), for
//! the layout styles it draws with. This crate holds only what does not
//! depend on it, so that a host other than the desktop, OpenMaker Server's
//! Save Records as PDF, can give its own content
//! ([#104](https://github.com/OpenMaker-Dev/openmaker/issues/104)).

use std::sync::Arc;

use om_layout::pagination::ReportPlan;
use om_layout::{BreakField, LayoutObject};

#[derive(Clone, Debug)]
pub struct ReportError {
    pub code: i32,
    pub message: String,
}
impl ReportError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }
}

/// What a viewer shows beside the page when it opens the PDF: Save Records
/// as PDF's restored Initial View option (script-steps.md 11.7, its values
/// as Create PDF's `View` names them in 12.5), written as the catalog's
/// `/PageMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PdfView {
    /// 0 Page Only: no `/PageMode`, the PDF default (`/UseNone`).
    #[default]
    PageOnly,
    /// 1 Bookmarks Panel and Page: `/UseOutlines`.
    Bookmarks,
    /// 2 Pages Panel and Page: `/UseThumbs`.
    Pages,
}

/// The planned document and its complete, validated PDF. No file I/O is done.
#[derive(Debug)]
pub struct PdfReport {
    pub plan: ReportPlan,
    pub bytes: Vec<u8>,
}

/// Independent coordinates for content evaluation. A once-only part or footer
/// may reuse a record on a different physical page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentContext {
    /// Index in the selected records, in found-set order. A sub-summary
    /// part's is its group's first (leading) or last (trailing) record.
    pub record: usize,
    /// Zero-based physical page, independent of the record's first placement.
    pub page: usize,
    /// A sub-summary part's break field: its summary fields read the
    /// record's group (`GetSummary`) rather than the whole found set.
    pub group: Option<BreakField>,
    /// The portal row an object prints in: its fields read the row's
    /// related record.
    pub portal: Option<PortalRowContext>,
}

/// `context` in portal row `row`, as Browse evaluates a portal row's
/// objects: the row's record of the portal's occurrence, and its number.
pub fn in_row(
    context: om_data::calc::eval::Context,
    row: Option<PortalRowContext>,
) -> om_data::calc::eval::Context {
    match row {
        Some(row) => context
            .with_portal(row.occurrence, row.record)
            .with_portal_row_number(row.number as u64),
        None => context,
    }
}

/// A portal row's related record: what Browse's portal rows evaluate in
/// (`Context::with_portal`, row number from 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalRowContext {
    pub occurrence: u64,
    pub record: u64,
    pub number: usize,
}

/// What a report shows of its objects for a record: the text of a text,
/// button or field object, the data of a container field, and the
/// conditional formats that hold for it. A closure gives text only: its
/// container fields print as their text, and an object with conditional
/// formatting is refused rather than printed unformatted.
pub trait ReportContent {
    fn text(
        &mut self,
        object: &LayoutObject,
        context: ContentContext,
    ) -> Result<String, ReportError>;

    /// The data a field object shows when it is a container field holding
    /// data, its streams loaded; `None` for other objects and empty fields.
    /// A missing or pending value is an error, as for text.
    fn container(
        &mut self,
        _object: &LayoutObject,
        _context: ContentContext,
    ) -> Result<Option<Arc<om_data::calc::eval::StoredContainer>>, ReportError> {
        Ok(None)
    }

    /// The conditional formats of `object` whose condition holds for the
    /// record and page, as indexes into `calcs.conditions` in stored order;
    /// `None` when this source does not evaluate them.
    fn conditions(
        &mut self,
        _object: &LayoutObject,
        _context: ContentContext,
    ) -> Result<Option<Vec<usize>>, ReportError> {
        Ok(None)
    }

    /// The related records `portal` shows for the record in `context`, in
    /// the relationship's order; `None` when this source does not read
    /// them. A pending read is an error, as for text.
    fn portal_records(
        &mut self,
        _portal: &LayoutObject,
        _context: ContentContext,
    ) -> Result<Option<Vec<u64>>, ReportError> {
        Ok(None)
    }

    /// The panel tab control `control` shows in the window the report is
    /// for; `None` to print the one it first shows.
    fn front_panel(&mut self, _control: &LayoutObject) -> Option<u64> {
        None
    }
}

impl<F> ReportContent for F
where
    F: FnMut(&LayoutObject, ContentContext) -> Result<String, ReportError>,
{
    fn text(
        &mut self,
        object: &LayoutObject,
        context: ContentContext,
    ) -> Result<String, ReportError> {
        self(object, context)
    }
}

impl From<String> for ReportError {
    fn from(message: String) -> Self {
        Self::unsupported(message)
    }
}
