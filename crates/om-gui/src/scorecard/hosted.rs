//! The scorecard over the wire, against one of two hosts:
//!
//! - **OpenMaker Server** (the default): the files `OM_SCORECARD_FILES`,
//!   copied and hosted by `om-server` run as a process of its own on
//!   loopback ([`super::server`]), signed in to as each file's own Admin
//!   (the starter solutions' empty password).
//! - **An external server** (`OM_SCORECARD_HOST` set): the one file
//!   `OM_SCORECARD_FILE` there, such as a file hosted by FileMaker Server
//!   ([`External`]). The run signs in once (in the full mode, once more
//!   for the connection that reads back) and ends at the first failed
//!   sign-in, without trying again. No variable's value is printed or
//!   written to the report.
//!
//! Each file is measured by reading calls only ([`ReadOnly`]; in the
//! read-only mode a find only on a field that keeps its indexes): the open,
//! the layout list, Browse, Edit, Script, Output and Calculations as the
//! local scorecard measures them (table definitions read from the file on
//! disk where there is one), finds and sorts ([`find_and_sort`]), and each
//! layout's Preview and PDF page count ([`page_counts`]). Then, unless
//! `OM_SCORECARD_READ_ONLY` is set, the two things a hosted user meets
//! first:
//!
//! - a record created and committed on one connection that a second
//!   connection reads back;
//! - a script that changes a record: saved over the wire (New Script, its
//!   steps: Go to Layout, New Record/Request, Set Field, Commit
//!   Records/Requests) and performed on the host (Perform Script on
//!   Server), the record then read on a second connection.
//!
//! Those records and that script are deleted afterwards; what could not
//! be is reported. Trust and Build are measured on local files only.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use om_data::found::{FindKind, FindRequest, FoundSet, FoundSetChange, SortField, SortOrder};
use om_data::runtime::ServerScriptCall;
use om_data::scripts::display::{parse_steps, Origin, Pasted};
use om_data::{Connection, DataError, DataProvider, FieldChanges, Script, ScriptKind};
use om_layout::pagination::PageSize;
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use serde::Serialize;

use super::server::{HostedServer, ACCOUNT, PASSWORD};
use super::{label, measure_opened, Desktop, FileCard, Open};
use crate::report;

/// The name of the script the full mode makes, and deletes.
const SCRIPT: &str = "Scorecard Hosted Script";

/// Preview's plans are drawn as a PDF, and its pages counted, up to this
/// many pages.
const PDF_PAGES: usize = 200;

#[derive(Default, Serialize)]
struct Extra {
    file: String,
    /// `Get ( HostApplicationVersion )` as the host gives it.
    host: Option<String>,
    /// Layouts listed, separators and folders left out.
    layouts: usize,
    finds: Finds,
    pages: Vec<Pages>,
    /// `None` in the read-only mode.
    commit_read_back: Option<(bool, String)>,
    script_changes_record: Option<(bool, String)>,
    /// Records and scripts the full mode made and could not delete.
    left: Vec<String>,
}

#[derive(Default, Serialize)]
struct Finds {
    passed: usize,
    checks: usize,
    detail: String,
}

#[derive(Serialize)]
struct Pages {
    layout: String,
    records: u64,
    /// Pages in Preview's plan, which Save Records as PDF and Print share.
    preview: Result<usize, String>,
    /// Pages of the PDF drawn from that plan; `None` past [`PDF_PAGES`].
    pdf: Option<Result<usize, String>>,
    /// Parts that resize to their content, measured with empty fields.
    resizes: bool,
}

/// A signed-in connection through which only reads reach the host: the
/// read-only mode's guarantee. It forwards the reading calls below and no
/// others. Every other `DataProvider` method keeps the trait's default
/// body, which has no connection to reach and refuses or does nothing:
/// creating, editing, deleting and importing records, globals, serial
/// numbers, transactions, schema and layout changes, saving and performing
/// scripts. It cannot sign in again (`open` refuses). Nothing it forwards
/// performs a script, and no window opens, so no script trigger runs. One
/// forwarded read can still make the host write: a find on a field without
/// its index, which the host builds and stores; the read-only mode finds
/// only on a field that keeps its indexes ([`keeps_indexes`]).
struct ReadOnly<'a>(&'a mut NativeProvider);

impl DataProvider for ReadOnly<'_> {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<om_data::HostedFile>, DataError> {
        Err(DataError::new("The read-only measure lists no files."))
    }

    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Err(DataError::new(
            "The read-only measure signs in once, before it starts.",
        ))
    }

    /// The connection's owner closes it.
    fn close(&mut self) {}

    fn layouts(&mut self) -> Result<Vec<om_data::LayoutInfo>, DataError> {
        self.0.layouts()
    }

    fn layout_menu(&mut self) -> Result<Vec<om_data::LayoutMenuEntry>, DataError> {
        self.0.layout_menu()
    }

    fn records(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
    ) -> Result<om_data::RecordPage, DataError> {
        self.0.records(layout_id, offset, limit)
    }

    fn render_layout(
        &mut self,
        layout_id: u32,
    ) -> Result<Option<om_data::RenderLayout>, DataError> {
        self.0.render_layout(layout_id)
    }

    fn record_capabilities(
        &mut self,
        layout_id: u32,
    ) -> Result<om_data::RecordCapabilities, DataError> {
        self.0.record_capabilities(layout_id)
    }

    fn calc_schema(&mut self) -> Result<Option<Arc<om_data::host::CalcSchema>>, DataError> {
        self.0.calc_schema()
    }

    /// The script catalog, read; no script is performed.
    fn scripts(&mut self) -> Result<om_data::ScriptCatalog, DataError> {
        self.0.scripts()
    }

    /// Finds and sorts change this connection's found sets, not the file.
    fn change_found_set(
        &mut self,
        layout_id: u32,
        change: FoundSetChange,
    ) -> Result<FoundSet, DataError> {
        self.0.change_found_set(layout_id, change)
    }

    fn host_application_version(&self) -> Option<String> {
        self.0.host_application_version()
    }
}

/// Where and as whom the measure signs in.
struct Endpoint {
    config: NativeConfig,
    host: String,
    port: u16,
    account: String,
    password: String,
    /// The client license key (install code) sent with the sign-in.
    license: Option<String>,
}

impl Endpoint {
    fn sign_in(&self, file: &str) -> Result<NativeProvider, String> {
        let mut p = NativeProvider::new(self.config.clone());
        if let Some(license) = &self.license {
            p.set_client_license(Some(license)).map_err(|e| e.message)?;
        }
        p.open(&Connection {
            host: self.host.clone(),
            port: self.port,
            file: file.into(),
            account: self.account.clone(),
            password: self.password.clone(),
        })
        .map_err(|e| e.message)?;
        Ok(p)
    }
}

/// The OpenMaker Server child's endpoint: its certificate trusted, a client
/// identity made for the run.
fn child_endpoint(server: &HostedServer) -> Endpoint {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    Endpoint {
        config: NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: server.ca_pem.clone(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        },
        host: "127.0.0.1".into(),
        port: server.port,
        account: ACCOUNT.into(),
        password: PASSWORD.into(),
        license: None,
    }
}

/// An external server, from the environment only: `OM_SCORECARD_HOST`,
/// `OM_SCORECARD_PORT` (5003 when unset), `OM_SCORECARD_FILE`,
/// `OM_SCORECARD_ACCOUNT`, `OM_SCORECARD_PASSWORD` (it may be empty),
/// `OM_SCORECARD_LICENSE` (the client license key, when the server wants
/// one) and `OM_SCORECARD_LOCAL_COPY` (a copy of the same file on disk, for
/// its table definitions). TLS is the desktop's ([`NativeConfig::from_env`]):
/// public roots, `OPENMAKER_CA_CERT`, and `OPENMAKER_CLIENT_CERT` with
/// `OPENMAKER_CLIENT_KEY` or an identity made for the run. The values are
/// never printed or written: [`External::redact`] masks them in any
/// message that carries one.
struct External {
    endpoint: Endpoint,
    file: String,
    local_copy: Option<PathBuf>,
}

impl External {
    /// `None` when `OM_SCORECARD_HOST` is not set.
    fn from_env() -> Option<Result<External, String>> {
        let host = std::env::var("OM_SCORECARD_HOST")
            .ok()
            .filter(|h| !h.trim().is_empty())?;
        let var = |name: &str| std::env::var(name).map_err(|_| format!("set {name}"));
        Some((|| {
            let port = match std::env::var("OM_SCORECARD_PORT") {
                Ok(port) => port
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "OM_SCORECARD_PORT is not a port number".to_string())?,
                Err(_) => crate::connect::DEFAULT_PORT,
            };
            Ok(External {
                endpoint: Endpoint {
                    config: NativeConfig::from_env().map_err(|e| e.message)?,
                    host: host.trim().to_owned(),
                    port,
                    account: var("OM_SCORECARD_ACCOUNT")?,
                    password: var("OM_SCORECARD_PASSWORD")?,
                    license: std::env::var("OM_SCORECARD_LICENSE")
                        .ok()
                        .filter(|l| !l.is_empty()),
                },
                file: var("OM_SCORECARD_FILE")?,
                local_copy: std::env::var_os("OM_SCORECARD_LOCAL_COPY").map(PathBuf::from),
            })
        })())
    }

    /// `text` with the host, file, account, password and license masked;
    /// a value under three characters is left, as masking it would mask
    /// the text around it too (the measure writes none of them itself).
    fn redact(&self, text: &str) -> String {
        let e = &self.endpoint;
        let stem = self.file.strip_suffix(".fmp12").unwrap_or(&self.file);
        let mut values = vec![
            (e.password.as_str(), "<password>"),
            (e.license.as_deref().unwrap_or_default(), "<license>"),
            (self.file.as_str(), "<file>"),
            (stem, "<file>"),
            (e.account.as_str(), "<account>"),
            (e.host.as_str(), "<host>"),
        ];
        values.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        let mut text = text.to_owned();
        for (value, mask) in values {
            if value.chars().count() >= 3 {
                text = text.replace(value, mask);
            }
        }
        text
    }
}

/// Sign in to `file`, measure it, and in the full mode make and remove its
/// records and script. `Err` is a failed sign-in, before anything was
/// written.
fn measure_file(
    endpoint: &Endpoint,
    file: &str,
    card: &mut FileCard,
    local: Option<&Path>,
    desktop: &Desktop,
    read_only: bool,
) -> Result<Extra, String> {
    let mut a = endpoint.sign_in(file)?;
    card.open = Open {
        opened: true,
        account: None,
        read_only: None,
        error: None,
    };
    let mut extra = Extra {
        file: card.file.clone(),
        ..Extra::default()
    };
    {
        let mut reads = ReadOnly(&mut a);
        extra.host = reads.host_application_version();
        measure_opened(card, &mut reads, local, desktop);
        extra.layouts = card.layouts.len();
        extra.finds = find_and_sort(&mut reads, read_only, &mut extra.left);
        extra.pages = page_counts(&mut reads);
    }
    if !read_only {
        // The connection that reads back, signed in before anything is
        // written.
        let mut b = match endpoint.sign_in(file) {
            Ok(b) => b,
            Err(e) => {
                a.close();
                return Err(format!("the second connection: {e}"));
            }
        };
        extra.commit_read_back = Some(commit_read_back(&mut a, &mut b, &mut extra.left));
        extra.script_changes_record = Some(script_changes_record(&mut a, &mut b, &mut extra.left));
        b.close();
    }
    a.close();
    Ok(extra)
}

/// Separators and folders of the layout menu: no layouts to use.
fn structural(p: &mut dyn DataProvider) -> BTreeSet<u32> {
    p.layout_menu()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.flags & (0x01 | 0x02 | 0x10) != 0)
        .map(|entry| entry.id)
        .collect()
}

/// A find on a field whose index does not exist makes the host build it
/// and store it in the file (schema-path.md 3.7, query-program.md 7.1;
/// OpenMaker Server does too, measured). A text field whose definition
/// keeps both its value and word indexes (indexing All: options
/// `0x80000000` and `0x20000000`, not "prevent index" `0x40000000`) has
/// them already, so a find on it builds none.
fn keeps_indexes(schema: &om_data::host::CalcSchema, layout: u32, field: &str) -> bool {
    const BOTH: u32 = 0x8000_0000 | 0x2000_0000;
    schema
        .layouts()
        .iter()
        .find(|l| l.id == layout)
        .and_then(|l| schema.table_of(l.occurrence?))
        .is_some_and(|table| {
            table.fields.values().any(|d| {
                d.name == field && d.options & BOTH == BOTH && d.options & 0x4000_0000 == 0
            })
        })
}

/// Finds and sorts on the first layout whose first record shows a word (of
/// three letters or digits or more) in a text field of its own table,
/// preferring a field that keeps its indexes ([`keeps_indexes`]): Show All;
/// Sort by that field ascending and then descending, each ending where the
/// other begins; a find for the word, which finds that record among
/// others; Show All again. Only this connection's found sets change, and
/// it ends with every record found, unsorted. `read_only` runs the find
/// only on a field that keeps its indexes; otherwise a find on one that
/// does not is noted in `left`, as the index the host may have built.
fn find_and_sort(p: &mut dyn DataProvider, read_only: bool, left: &mut Vec<String>) -> Finds {
    let skip = structural(p);
    let schema = p.calc_schema().ok().flatten();
    let indexed = |layout: u32, field: &str| {
        schema
            .as_deref()
            .is_some_and(|schema| keeps_indexes(schema, layout, field))
    };
    // Each candidate: layout, its name, field, word, record, indexes kept.
    let mut candidates = Vec::new();
    match p.layouts() {
        Ok(layouts) => {
            for info in layouts.into_iter().filter(|l| !skip.contains(&l.id)) {
                let Ok(page) = p.records(info.id, 0, 1) else {
                    continue;
                };
                let Some(record) = page.records.first() else {
                    continue;
                };
                for (i, field) in page.fields.iter().enumerate() {
                    if field.kind != om_data::FieldKind::Text || field.name.contains("::") {
                        continue;
                    }
                    let value = record.values.get(i).cloned().flatten().unwrap_or_default();
                    if let Some(word) = value
                        .split(|c: char| !c.is_alphanumeric())
                        .find(|w| w.chars().count() >= 3)
                    {
                        let kept = indexed(info.id, &field.name);
                        candidates.push((
                            info.id,
                            info.name.clone(),
                            field.name.clone(),
                            word.to_owned(),
                            record.id,
                            kept,
                        ));
                    }
                }
            }
        }
        Err(e) => {
            return Finds {
                detail: e.message,
                ..Finds::default()
            }
        }
    }
    let Some(position) = candidates
        .iter()
        .position(|c| c.5)
        .or((!candidates.is_empty()).then_some(0))
    else {
        return Finds {
            detail: "no layout whose first record shows a word in a text field".into(),
            ..Finds::default()
        };
    };
    let (layout, layout_name, field, word, record, kept) = candidates.swap_remove(position);
    let mut results: Vec<(bool, String)> = Vec::new();
    let total = match p.change_found_set(layout, FoundSetChange::ShowAll) {
        Ok(all) => {
            results.push((
                all.found == all.total && all.total > 0,
                format!("Show All: {} of {}", all.found, all.total),
            ));
            all.total
        }
        Err(e) => {
            results.push((false, format!("Show All: {}", e.message)));
            0
        }
    };
    let mut ends = |order: SortOrder| -> Result<(bool, Option<String>, Option<String>), String> {
        let sorted = p
            .change_found_set(
                layout,
                FoundSetChange::Sort(vec![SortField::new(field.clone(), order)]),
            )
            .map_err(|e| e.message)?;
        let first = value_at(p, layout, &field, 0)?;
        let last = value_at(p, layout, &field, sorted.found.saturating_sub(1))?;
        Ok((sorted.sorted, first, last))
    };
    let ascending = ends(SortOrder::Ascending);
    let descending = ends(SortOrder::Descending);
    results.push(match (ascending, descending) {
        (Ok((a, a_first, a_last)), Ok((d, d_first, d_last))) => {
            let mirrored = a && d && a_first == d_last && a_last == d_first;
            (
                mirrored,
                format!(
                    "Sort by {field} ascending and descending: {}",
                    if mirrored {
                        "each ends where the other begins"
                    } else {
                        "the two orders do not mirror each other"
                    }
                ),
            )
        }
        (Err(e), _) | (_, Err(e)) => (false, format!("Sort by {field}: {e}")),
    });
    let mut request = FindRequest::default();
    request.set(&field, word);
    let find = if read_only && !kept {
        None
    } else {
        if !kept {
            left.push(format!(
                "the indexes of {field}, if the host built them for the find (its definition keeps none)"
            ));
        }
        Some(request)
    };
    if let Some(request) = find {
        results.push(
            match p.change_found_set(
                layout,
                FoundSetChange::Find {
                    requests: vec![request],
                    kind: FindKind::Replace,
                },
            ) {
                Ok(found) => {
                    let among = found.found > 10_000
                        || p.records(layout, 0, 10_000)
                            .is_ok_and(|page| page.records.iter().any(|r| r.id == record));
                    (
                        found.found >= 1 && found.found <= total && among,
                        format!(
                            "Find a word of the first record's {field}: {} of {total}, {}",
                            found.found,
                            if among {
                                "that record among them"
                            } else {
                                "without that record"
                            }
                        ),
                    )
                }
                Err(e) => (false, format!("Find: {}", e.message)),
            },
        );
    }
    results.push(match p.change_found_set(layout, FoundSetChange::ShowAll) {
        Ok(all) => (
            all.found == all.total && all.total == total,
            format!("Show All again: {} of {}", all.found, all.total),
        ),
        Err(e) => (false, format!("Show All again: {}", e.message)),
    });
    Finds {
        passed: results.iter().filter(|(ok, _)| *ok).count(),
        checks: results.len(),
        detail: format!(
            "on “{layout_name}”{}: {}",
            if read_only && !kept {
                format!(", no find: no text field showing a word keeps its indexes, and a find on {field} could make the host build them into the file")
            } else {
                String::new()
            },
            results
                .iter()
                .map(|(ok, note)| if *ok {
                    note.clone()
                } else {
                    format!("**{note}**")
                })
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

/// `field` of the found set's record at `offset` on `layout`.
fn value_at(
    p: &mut dyn DataProvider,
    layout: u32,
    field: &str,
    offset: u64,
) -> Result<Option<String>, String> {
    let page = p.records(layout, offset, 1).map_err(|e| e.message)?;
    let column = page
        .fields
        .iter()
        .position(|f| f.name == field)
        .ok_or("the layout no longer shows the field")?;
    Ok(page
        .records
        .first()
        .and_then(|r| r.values.get(column).cloned().flatten()))
}

/// A report's content without values: every field empty, no conditional
/// format holding, no related rows in a portal.
struct Empty;

impl report::ReportContent for Empty {
    fn text(
        &mut self,
        _: &om_layout::LayoutObject,
        _: report::ContentContext,
    ) -> Result<String, report::ReportError> {
        Ok(String::new())
    }

    fn conditions(
        &mut self,
        _: &om_layout::LayoutObject,
        _: report::ContentContext,
    ) -> Result<Option<Vec<usize>>, report::ReportError> {
        Ok(Some(Vec::new()))
    }

    fn portal_records(
        &mut self,
        _: &om_layout::LayoutObject,
        _: report::ContentContext,
    ) -> Result<Option<Vec<u64>>, report::ReportError> {
        Ok(Some(Vec::new()))
    }
}

/// Each layout's Preview and PDF page count for every record of its table,
/// as Preview and Save Records as PDF plan them (`report::plan_report_with`):
/// unsorted, so no sub-summary part prints, and without values ([`Empty`]),
/// so a layout whose parts resize to their content is measured with its
/// fields empty. Up to [`PDF_PAGES`] pages the PDF is drawn and its pages
/// counted.
fn page_counts(p: &mut dyn DataProvider) -> Vec<Pages> {
    let skip = structural(p);
    let Ok(layouts) = p.layouts() else {
        return Vec::new();
    };
    let mut counts = Vec::new();
    for info in layouts.into_iter().filter(|l| !skip.contains(&l.id)) {
        let mut pages = Pages {
            layout: info.name,
            records: 0,
            preview: Err(String::new()),
            pdf: None,
            resizes: false,
        };
        let render = match p.render_layout(info.id) {
            Ok(Some(render)) => render,
            Ok(None) => {
                pages.preview = Err("no layout to render".into());
                counts.push(pages);
                continue;
            }
            Err(e) => {
                pages.preview = Err(e.message);
                counts.push(pages);
                continue;
            }
        };
        match p.records(info.id, 0, 1) {
            Ok(page) => pages.records = page.total,
            Err(e) => {
                pages.preview = Err(format!("Records: {}", e.message));
                counts.push(pages);
                continue;
            }
        }
        // Nothing to print: Preview and PDF refuse, which is not measured.
        if pages.records == 0 {
            pages.preview = Err("no records".into());
            counts.push(pages);
            continue;
        }
        let layout = &render.layout;
        // A page the layout cannot have (its fixed margins leave no room,
        // #609) refuses Preview and PDF, as on the desktop.
        let paper = match PageSize::for_layout(layout) {
            Ok(paper) => paper,
            Err(error) => {
                pages.preview = Err(error);
                counts.push(pages);
                continue;
            }
        };
        let records = usize::try_from(pages.records).unwrap_or(usize::MAX);
        pages.resizes = report::resizes_parts(layout);
        pages.preview = report::plan_report_with(layout, records, paper.clone(), &[], &mut Empty)
            .map(|plan| plan.pages.len())
            .map_err(|e| e.message);
        if pages.preview.as_ref().is_ok_and(|n| *n <= PDF_PAGES) {
            pages.pdf = Some(
                report::render_pdf_with(layout, records, paper, &[], &mut Empty)
                    .map_err(|e| e.message)
                    .and_then(|pdf| {
                        pdf_pages(&pdf.bytes).ok_or_else(|| "no page count in the PDF".to_string())
                    }),
            );
        }
        counts.push(pages);
    }
    counts
}

/// The page count of a PDF's page tree (`/Count n`).
fn pdf_pages(bytes: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(bytes);
    let at = text.find("/Count ")? + "/Count ".len();
    text[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// The first layout of a table of the file, its occurrence's name, and a
/// stored text field it shows that Browse may change.
fn target(p: &mut dyn DataProvider) -> Result<(u32, String, String, String), String> {
    let schema = p
        .calc_schema()
        .map_err(|e| e.message)?
        .ok_or("no calculation schema")?;
    let (layout, name, occurrence) = schema
        .layouts()
        .iter()
        .find_map(|l| {
            let o = l.occurrence?;
            schema.table_of(o)?;
            Some((l.id, l.name.clone(), o))
        })
        .ok_or("no layout of a table of this file")?;
    let occurrence_name = schema
        .occurrence(occurrence)
        .map(|o| o.name.clone())
        .ok_or("no occurrence")?;
    let table = schema.table_of(occurrence).ok_or("no table")?;
    let capabilities = p.record_capabilities(layout).map_err(|e| e.message)?;
    let page = p.records(layout, 0, 1).map_err(|e| e.message)?;
    let field = page
        .fields
        .iter()
        .filter(|f| f.kind == om_data::FieldKind::Text && capabilities.can_edit(f))
        .find(|f| {
            table.fields.values().any(|d| {
                d.name == f.name
                    && d.class == om_data::host::FieldClass::Data
                    && !d.global
                    && d.auto_enter.is_none()
            })
        })
        .map(|f| f.name.clone())
        .ok_or("no text field Browse may change on the layout")?;
    Ok((layout, name, occurrence_name, field))
}

/// The records on `layout` with their `field`, by id.
fn rows(p: &mut dyn DataProvider, layout: u32, field: &str) -> Result<Vec<(i64, String)>, String> {
    let page = p.records(layout, 0, 100_000).map_err(|e| e.message)?;
    let column = page
        .fields
        .iter()
        .position(|f| f.name == field)
        .ok_or("the layout no longer shows the field")?;
    Ok(page
        .records
        .iter()
        .map(|r| {
            let value = r.values.get(column).cloned().flatten().unwrap_or_default();
            (r.id, value)
        })
        .collect())
}

/// Delete records `ids` of `layout`; those that could not be go to `left`.
/// Says what happened.
fn remove_records(
    p: &mut dyn DataProvider,
    layout: u32,
    layout_name: &str,
    ids: &[i64],
    left: &mut Vec<String>,
) -> String {
    let mut kept = Vec::new();
    for id in ids {
        if let Err(e) = p.delete_record(layout, *id) {
            kept.push(format!("record {id} on “{layout_name}” ({})", e.message));
        }
    }
    let said = if kept.is_empty() {
        "deleted afterwards".to_string()
    } else {
        format!("left: {}", kept.join(", "))
    };
    left.extend(kept);
    said
}

fn commit_read_back(
    a: &mut NativeProvider,
    b: &mut NativeProvider,
    left: &mut Vec<String>,
) -> (bool, String) {
    let result = (|| -> Result<(bool, String), String> {
        let (layout, layout_name, _, field) = target(a)?;
        let value = "Committed over the wire";
        let record = a
            .create_record(
                layout,
                FieldChanges::from([(field.clone(), Some(value.into()))]),
            )
            .map_err(|e| format!("New Record with {field}: {}", e.message))?;
        // Read back, then remove the record, whatever the read found.
        let seen = rows(b, layout, &field).map(|rows| rows.iter().any(|(_, v)| v == value));
        let removed = remove_records(a, layout, &layout_name, &[record.id], left);
        let seen = seen.map_err(|e| format!("a second connection: {e}; the record {removed}"))?;
        Ok((
            seen,
            format!(
                "record {} on “{layout_name}”, {field} = “{value}”: {}; {removed}",
                record.id,
                if seen {
                    "read back on a second connection"
                } else {
                    "not on a second connection"
                }
            ),
        ))
    })();
    result.unwrap_or_else(|e| (false, e))
}

fn script_changes_record(
    a: &mut NativeProvider,
    b: &mut NativeProvider,
    left: &mut Vec<String>,
) -> (bool, String) {
    let value = "Set by a script on the host";
    // What the cleanup needs: the layout and field read, the script saved.
    let mut shown: Option<(u32, String, String)> = None;
    let mut made = None;
    let result = (|| -> Result<(bool, String), String> {
        let (layout, layout_name, occurrence, field) = target(a)?;
        shown = Some((layout, layout_name.clone(), field.clone()));
        let schema = a
            .calc_schema()
            .map_err(|e| e.message)?
            .ok_or("no calculation schema")?;
        let text = format!(
            "Go to Layout [ “{layout_name}” ({occurrence}) ]\nNew Record/Request\nSet Field [ {occurrence}::{field} ; \"{value}\" ]\nCommit Records/Requests [ With dialog: Off ]"
        );
        let before = rows(b, layout, &field)?;
        let steps = parse_steps(&text, &*schema, Origin::Unknown)?
            .into_iter()
            .map(|pasted| match pasted {
                Pasted::Step(step) => Ok(step),
                Pasted::Failed { text, reason } => Err(format!("“{text}”: {reason}")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut catalog = a.scripts_for_editing().map_err(|e| e.message)?;
        let id = catalog.scripts.iter().map(|s| s.id).max().unwrap_or(0) + 1;
        catalog.scripts.push(Script {
            id,
            name: SCRIPT.into(),
            kind: ScriptKind::Script,
            ..Script::default()
        });
        let stored = a
            .save_script_catalog(catalog)
            .map_err(|e| format!("New Script: {}", e.message))?;
        let mut script = stored
            .scripts
            .into_iter()
            .find(|s| s.name == SCRIPT)
            .ok_or("the stored catalog has no new script")?;
        made = Some(script.id);
        // As the Script Workspace adds pasted steps: each gets its data key.
        script.steps.clear();
        script
            .insert_steps(0, steps)
            .map_err(|e| format!("adding the steps: {e}"))?;
        let saved = a
            .save_script(script)
            .map_err(|e| format!("saving its steps: {}", e.message))?;
        let ran = a
            .perform_script_on_server(&ServerScriptCall {
                file: 0,
                script: saved.id,
                parameter: None,
                wait: true,
            })
            .map_err(|e| format!("Perform Script on Server: {}", e.message))?;
        // The steps as stored, read back as text with the file's names.
        let names: Vec<String> = saved
            .steps
            .iter()
            .map(|s| om_data::scripts::display::step_text(s, &*schema))
            .collect();
        let after = rows(b, layout, &field)?;
        let seen = after.iter().any(|(_, v)| v == value);
        Ok((
            ran.error.0 == 0 && seen,
            format!(
                "stored {names:?}, performed on the host: last error {}; {} records before, {} after; {field} = “{value}” {}",
                ran.error.0,
                before.len(),
                after.len(),
                if seen {
                    "read back on a second connection"
                } else {
                    "not on a second connection"
                }
            ),
        ))
    })();
    // Remove what was made, whatever happened: the records holding the
    // script's value (none another user made), then the script.
    let mut cleanup = Vec::new();
    if let Some((layout, layout_name, field)) = &shown {
        match rows(b, *layout, field) {
            Ok(rows) => {
                let ids: Vec<i64> = rows
                    .iter()
                    .filter(|(_, v)| v == value)
                    .map(|(id, _)| *id)
                    .collect();
                if !ids.is_empty() {
                    let said = remove_records(a, *layout, layout_name, &ids, left);
                    cleanup.push(format!("its record {said}"));
                }
            }
            Err(e) => {
                let note = format!(
                    "records with {field} = “{value}” on “{layout_name}”, if the script made one (not read: {e})"
                );
                cleanup.push(format!("left: {note}"));
                left.push(note);
            }
        }
    }
    if let Some(id) = made {
        let deleted = a.scripts_for_editing().and_then(|mut catalog| {
            catalog.scripts.retain(|s| s.id != id);
            a.save_script_catalog(catalog)
        });
        match deleted {
            Ok(_) => cleanup.push("the script deleted afterwards".into()),
            Err(e) => {
                let note = format!("script “{SCRIPT}” ({})", e.message);
                cleanup.push(format!("left: {note}"));
                left.push(note);
            }
        }
    }
    let (ok, mut said) = result.unwrap_or_else(|e| (false, e));
    if !cleanup.is_empty() {
        said = format!("{said}; {}", cleanup.join(", "));
    }
    (ok, said)
}

#[test]
#[ignore = "requires OM_SCORECARD_OUT, and OM_SCORECARD_FILES with the om-server binary (cargo build -p om-server, or OM_SERVER_BIN), or OM_SCORECARD_HOST, OM_SCORECARD_FILE, OM_SCORECARD_ACCOUNT and OM_SCORECARD_PASSWORD for an external server"]
fn alpha_scorecard_hosted() {
    let out = PathBuf::from(std::env::var_os("OM_SCORECARD_OUT").expect("set OM_SCORECARD_OUT"));
    let read_only =
        std::env::var("OM_SCORECARD_READ_ONLY").is_ok_and(|v| !v.is_empty() && v != "0");
    let desktop = Desktop::read();
    let external = External::from_env().map(|e| e.unwrap_or_else(|e| panic!("{e}")));
    let mut cards = Vec::new();
    let mut extras = Vec::new();
    match &external {
        Some(external) => {
            let mut card = FileCard {
                file: "the hosted file".into(),
                ..FileCard::default()
            };
            let extra = measure_file(
                &external.endpoint,
                &external.file,
                &mut card,
                external.local_copy.as_deref(),
                &desktop,
                read_only,
            )
            .unwrap_or_else(|e| {
                panic!(
                    "{}",
                    external.redact(&format!(
                        "A sign-in to the external server failed, and the run ends here without trying again: {e}"
                    ))
                )
            });
            cards.push(card);
            extras.push(extra);
        }
        None => {
            let files: Vec<PathBuf> = std::env::split_paths(
                &std::env::var_os("OM_SCORECARD_FILES").expect("set OM_SCORECARD_FILES"),
            )
            .collect();
            let temp = tempfile::tempdir().expect("temporary folder");
            // Each starter is `en.fmp12` in a folder named for it: hosted
            // by that name.
            let names: Vec<String> = files
                .iter()
                .map(|p| format!("{}.fmp12", label(p).split('/').next().unwrap_or("file")))
                .collect();
            let copies: Vec<PathBuf> = files
                .iter()
                .zip(&names)
                .map(|(path, name)| {
                    let copy = temp.path().join(name);
                    std::fs::copy(path, &copy).expect("copy the file");
                    copy
                })
                .collect();
            let server = HostedServer::start(&copies, temp.path()).expect("OpenMaker Server");
            let endpoint = child_endpoint(&server);
            for ((path, copy), name) in files.iter().zip(&copies).zip(&names) {
                let mut card = FileCard {
                    file: label(path),
                    ..FileCard::default()
                };
                match measure_file(&endpoint, name, &mut card, Some(copy), &desktop, read_only) {
                    Ok(extra) => extras.push(extra),
                    Err(error) => {
                        card.open.error = Some(error);
                        extras.push(Extra {
                            file: card.file.clone(),
                            ..Extra::default()
                        });
                    }
                }
                cards.push(card);
            }
            drop(server);
        }
    }
    let redact = |text: String| match &external {
        Some(external) => external.redact(&text),
        None => text,
    };
    std::fs::create_dir_all(&out).expect("output folder");
    let json = serde_json::to_string_pretty(&(&cards, &extras)).expect("JSON");
    std::fs::write(out.join("hosted.json"), redact(json)).expect("write hosted.json");
    let md = summary(&cards, &extras, external.as_ref(), read_only);
    std::fs::write(out.join("hosted.md"), redact(md)).expect("write hosted.md");
}

fn summary(
    cards: &[FileCard],
    extras: &[Extra],
    external: Option<&External>,
    read_only: bool,
) -> String {
    // The local summary's sections, without Trust's and Build's, which
    // are measured on local files only.
    let full = super::summary(cards);
    let mut kept = Vec::new();
    let mut skipping = false;
    for line in full.lines() {
        if line.starts_with("## ") {
            skipping = line.starts_with("## Trust") || line.starts_with("## Build");
        }
        if skipping || line.starts_with("| 7 Build") || line.starts_with("| 8 Trust") {
            continue;
        }
        if line.starts_with("The ") && line.contains("written files") {
            continue;
        }
        // Hosted files open as the account signed in with, not as the
        // local measure's (the account the file suggests, an empty
        // password).
        if line.starts_with("| 1 Open |") {
            let opened = line.rsplit_once("| ").map_or("", |(_, r)| r);
            let account = match external {
                Some(_) => "the account `OM_SCORECARD_ACCOUNT` names".to_string(),
                None => format!("`{ACCOUNT}`, with the file's own password"),
            };
            kept.push(format!(
                "| 1 Open | files that open over the wire as {account} | {opened}"
            ));
            continue;
        }
        kept.push(line.to_owned());
    }
    let host = extras
        .iter()
        .find_map(|e| e.host.clone())
        .unwrap_or_else(|| "not given".into());
    let mut md = match external {
        Some(external) => format!(
            "# A file hosted by an external server\n\nThe file `OM_SCORECARD_FILE` names, on the server `OM_SCORECARD_HOST` names (`Get ( HostApplicationVersion )`: {host}), measured over the wire as the account `OM_SCORECARD_ACCOUNT` names; their values are not written here. Table definitions: {}.\n\n",
            match &external.local_copy {
                Some(_) => "read from the copy `OM_SCORECARD_LOCAL_COPY` names",
                None => "not read (no `OM_SCORECARD_LOCAL_COPY`), so Edit has no tables and no auto-entry prohibitions",
            }
        ),
        None => format!(
            "# Hosted by OpenMaker Server\n\nThe files hosted by OpenMaker Server ({host}), a separate `om-server` process on 127.0.0.1, and measured over the wire as `{ACCOUNT}`, each file's own [Full Access] account, which the server checks against the file as FileMaker Server does. The starter solutions' Admin has an empty password, which FileMaker Server does not host by default, so the server is told to host them (`--allow-insecure-files`). Hosting by FileMaker Server is measured by this test's external-server mode, not here.\n\n"
        ),
    };
    md.push_str(if read_only {
        "**Read-only:** only reading calls reached the host. No record, field, layout or script was written, no script was performed, and no window opened, so no script trigger ran. The find ran only on a field that keeps its indexes, so the host built none.\n\n"
    } else {
        "**Full:** after the reads, a record and a script were made on the host, and deleted afterwards.\n\n"
    });
    let sum = |f: fn(&Extra) -> usize| extras.iter().map(f).sum::<usize>();
    // Layouts whose table has records: an empty one prints nothing.
    let planned: Vec<&Pages> = extras
        .iter()
        .flat_map(|e| &e.pages)
        .filter(|p| p.records > 0)
        .collect();
    let drawn: Vec<&Pages> = planned
        .iter()
        .copied()
        .filter(|p| p.pdf.is_some())
        .collect();
    let agree = drawn
        .iter()
        .filter(|p| p.pdf.as_ref().and_then(|r| r.as_ref().ok()) == p.preview.as_ref().ok())
        .count();
    md.push_str(&format!(
        "| Over the wire | Result |\n|---|---|\n| layouts listed (separators and folders left out) | {} |\n| find and sort checks (Show All, Sort both ways, Find, Show All) | {} of {} |\n| layouts with records that Preview plans pages for, every record, unsorted | {} of {} |\n| of those up to {PDF_PAGES} pages, PDFs whose page count is Preview's | {agree} of {} |\n",
        sum(|e| e.layouts),
        sum(|e| e.finds.passed),
        sum(|e| e.finds.checks),
        planned.iter().filter(|p| p.preview.is_ok()).count(),
        planned.len(),
        drawn.len(),
    ));
    if !read_only {
        let count =
            |f: fn(&Extra) -> Option<bool>| extras.iter().filter(|e| f(e) == Some(true)).count();
        md.push_str(&format!(
            "| a record committed on one connection, read back on a second | {} of {} |\n| a script saved over the wire and performed on the host changes a record a second connection reads | {} of {} |\n| records and scripts made and not deleted | {} |\n",
            count(|e| e.commit_read_back.as_ref().map(|r| r.0)),
            extras.len(),
            count(|e| e.script_changes_record.as_ref().map(|r| r.0)),
            extras.len(),
            sum(|e| e.left.len()),
        ));
    }
    md.push('\n');
    for e in extras {
        md.push_str(&format!("- {}: find and sort {}", e.file, e.finds.detail));
        if let Some((_, said)) = &e.commit_read_back {
            md.push_str(&format!("; commit: {said}"));
        }
        if let Some((_, said)) = &e.script_changes_record {
            md.push_str(&format!("; script: {said}"));
        }
        if !e.left.is_empty() {
            md.push_str(&format!("; **left on the host:** {}", e.left.join(", ")));
        }
        md.push('\n');
    }
    md.push_str("\n## Preview and PDF pages\n\nEvery record of each layout's table, unsorted, without values: fields empty, no conditional format holding and no portal rows, so a layout whose parts resize to their content is measured empty.\n\n| File | Layout | Records | Preview pages | PDF pages |\n|---|---|---:|---:|---:|\n");
    for e in extras {
        for p in &e.pages {
            let preview = match &p.preview {
                Ok(n) => format!("{n}{}", if p.resizes { " (fields empty)" } else { "" }),
                Err(_) if p.records == 0 => "no records".into(),
                Err(e) => format!("refused: {e}"),
            };
            let pdf = match &p.pdf {
                Some(Ok(n)) => n.to_string(),
                Some(Err(e)) => format!("refused: {e}"),
                None => "-".into(),
            };
            md.push_str(&format!(
                "| {} | {} | {} | {preview} | {pdf} |\n",
                e.file, p.layout, p.records
            ));
        }
    }
    md.push('\n');
    md.push_str(&kept.join("\n"));
    md.push('\n');
    md
}
