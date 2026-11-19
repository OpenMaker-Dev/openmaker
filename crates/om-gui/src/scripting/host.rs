//! The desktop client's [`ScriptHost`], on the provider worker: steps call
//! the provider directly and block, and act on the worker's
//! [`Window`](super::window::Window), which the UI follows.
//!
//! Calculations read the file through the provider's calculation schema
//! (`om_data::host`): every field of every record, related records, the
//! found set, summaries and the session's `Get` answers, with the window's
//! unsaved values on the current record. A provider without a calculation
//! schema gives calculations the fields the layout shows.
//!
//! Events a step causes fire the layout's and objects' triggers
//! (script-runtime.md 7): an action with pre-event triggers (leaving a
//! layout, committing, exiting an object or a mode) is held in stages, one
//! trigger at a time, and goes on when the trigger's script lets it
//! ([`ScriptHost::trigger_action`]); delayed triggers follow the action.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use om_data::calc::eval::{
    default_call, display, qualified_field_name, CalcHost, Context, CustomFunction, FieldId,
    FieldInfo, HostFunction, LookupDirection, VarScope, Variables,
};
use om_data::calc::{CalcError, Ctx, Date, Locale, Value, ValueType};
use om_data::formula::Names;
use om_data::found::{code, FindRequest, FoundSetChange, SortField, SortOrder};
use om_data::host::session::{auto_enters, modified_fields};
use om_data::host::{
    stored_value, CachedSource, CalcData, CalcNeed, CalcSchema, FieldClass, FileCalc, SessionInfo,
    Vars, WindowMode, NO_RECORD,
};
use om_data::runtime::*;
use om_data::{Calculation, DataError, DataProvider, FieldChanges, Script};
use om_layout::ScriptTrigger;

use super::session::Shared;
use super::window::Window;
use super::{files, Answer, PortalRow, Prompt, ScriptEvent, View};
use crate::script_workspace::BrowseSort;
use crate::worker::{self, Job, Reply, Request, Response};

/// `Get` selectors the window answers.
const CURRENT_DATE: u8 = 2;
const FOUND_COUNT: u8 = 7;
const LAYOUT_NAME: u8 = 10;
const WINDOW_MODE: u8 = 13;
const RECORD_NUMBER: u8 = 20;
const RECORD_ID: u8 = 37;
const DESKTOP_PATH: u8 = 72;
const DOCUMENTS_PATH: u8 = 73;
const FILEMAKER_PATH: u8 = 74;
const PREFERENCES_PATH: u8 = 77;
/// "Record access is denied": what FileMaker Pro 26.0.3's Copy All
/// Records/Requests returns when the set denies exporting (#651's reading).
const ACCESS_DENIED: ErrorCode = ErrorCode(200);
const SYSTEM_DRIVE: u8 = 81;
const TEMPORARY_PATH: u8 = 89;
const DOCUMENTS_PATH_LISTING: u8 = 93;
const OPEN_DATA_FILE_INFO: u8 = 123;
const CACHE_FILE_NAME: u8 = 130;
const CACHE_FILE_PATH: u8 = 131;

/// "Field cannot be modified".
const FIELD_LOCKED: i32 = 201;

/// Script Workspace and Manage Database, the menu steps the app has.
const OPEN_MANAGE_DATABASE: u16 = 38;
const OPEN_SCRIPT_WORKSPACE: u16 = 88;

/// Trigger action tokens the UI hands out carry this bit; the worker's own
/// tokens do not.
pub(crate) const UI_TOKEN: u64 = 1 << 63;

/// The runtime options of the desktop client: FileMaker's own rules, but a
/// step the client lacks stops the script before it changes anything, and
/// a time slice lets the worker look at the UI's requests.
pub(crate) fn options() -> Options {
    let client = if cfg!(windows) {
        Client::Windows
    } else {
        Client::Mac
    };
    Options {
        time_slice: Some(std::time::Duration::from_millis(50)),
        stop_on_unsupported: true,
        ..Options::new(client)
    }
}

/// An action a step started that waits for pre-event triggers.
#[derive(Clone, Debug)]
pub(crate) enum Act {
    Layout(u32),
    Record(u64),
    /// Commit Records, with its "Skip data entry validation".
    Commit(bool),
    Print(print::Request),
    Revert,
    Mode(Mode),
    Focus(Option<String>),
    ClosePopover {
        window: u32,
        layout: u32,
        popover: u64,
        button: Option<u64>,
        field: Option<u64>,
        modified: bool,
    },
    /// The focus leaves the field it is in (a portal row's too) for the
    /// portal row `row` (`None`: no object is active), with Go to Portal
    /// Row's Select; the record stays open (Go to Field [ ] commits; this
    /// does not).
    Portal {
        row: Option<PortalRow>,
        select: bool,
    },
    NewRecord,
    Duplicate,
}

mod containers;
mod databases;
mod page_setup;
mod pdf;
mod pdf_text;
mod portals;
mod print;
mod refresh;
mod related;
pub(crate) mod transaction;
mod transfer;
mod web_viewers;
mod windows;
mod writes;

/// A held action and the stage its next trigger is at.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    act: Act,
    stage: usize,
    /// The event whose trigger holds it: the stages after it in FileMaker's
    /// order run when it lets the action go on ([`stage_rank`]).
    fired: TriggerEvent,
    window: u32,
    /// The portal field's edit its OnObjectSave trigger saves, once the
    /// trigger lets the action go on (#1040).
    saving: Option<SavedCell>,
}

/// A portal row field's cell, in the draft of the record the row shows
/// (`RelatedDraft`): a stored record by its table and record, one to
/// create by its row (`begun`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SavedCell {
    table: u64,
    record: Option<u64>,
    begun: Option<u64>,
    cell: u64,
}

/// Where a pre-event trigger stands in FileMaker's order of an action's
/// stages: the field's OnObjectValidate, OnObjectSave and OnObjectExit,
/// then OnRecordCommit (or OnRecordRevert), then OnLayoutExit or
/// OnModeExit. A held action resumes after the stage that held it by this
/// order, not by its place in the list: the trigger's script can change
/// which stages apply, such as a Set Field that ends the field's typing
/// (#1030), or a write that makes the record need committing.
fn stage_rank(event: TriggerEvent) -> u8 {
    match event {
        TriggerEvent::ObjectValidate => 0,
        TriggerEvent::ObjectSave => 1,
        TriggerEvent::ObjectExit => 2,
        TriggerEvent::RecordCommit | TriggerEvent::RecordRevert => 3,
        _ => 4,
    }
}

pub(crate) struct WorkerHost<'a> {
    provider: std::cell::RefCell<&'a mut Box<dyn DataProvider>>,
    store: &'a mut dyn crate::licensing::LicenseStore,
    jobs: &'a std::sync::mpsc::Receiver<Job>,
    responses: &'a std::sync::mpsc::Sender<Response>,
    s: &'a mut Shared,
    pub(crate) files: &'a mut super::databases::OpenFiles,
    triggers: Vec<Trigger>,
    locale: Locale,
    today: Date,
    elevated: bool,
    /// The file whose script step runs with full access, while `elevated`.
    elevated_file: Option<FileId>,
    /// Evaluation of a report row does not navigate the user's window.
    report_record: Option<(u64, usize, usize)>,
    /// One report session snapshot reaches nested field/summary hosts.
    report_session: Option<SessionInfo>,
    /// The UUID of the layout object whose calculation a report evaluates
    /// (`LayoutObjectUUID`, calc-functions.md 6.3 O5).
    pub(super) report_object: Option<String>,
}

impl<'a> WorkerHost<'a> {
    pub(crate) fn new(io: &'a mut super::session::Io<'_>, s: &'a mut Shared) -> Self {
        if s.file.schema.is_none() {
            s.file.schema = Some(io.provider.calc_schema().ok().flatten());
        }
        let utc_offset = om_provider::local_utc_offset();
        WorkerHost {
            provider: std::cell::RefCell::new(&mut *io.provider),
            files: &mut *io.files,
            store: &mut *io.store,
            jobs: io.jobs,
            responses: io.responses,
            s,
            triggers: Vec::new(),
            locale: Locale::en_us(),
            today: om_data::host::session::today(utc_offset),
            elevated: false,
            elevated_file: None,
            report_record: None,
            report_session: None,
            report_object: None,
        }
    }

    /// The open file window `window` belongs to.
    pub(crate) fn file_of_window(&self, window: u32) -> Option<FileId> {
        self.files.owner(&self.s.file, window)
    }

    pub(crate) fn shared(&mut self) -> &mut Shared {
        self.s
    }

    pub(crate) fn shared_ref(&self) -> &Shared {
        self.s
    }

    pub(crate) fn render_layout(&mut self, id: u32) -> Option<Arc<om_data::RenderLayout>> {
        self.render(id)
    }

    /// Adjust Window [Resize to Fit] (#1115): "the minimum size possible while
    /// keeping all items in the layout visible" (Claris, Adjust Window). The
    /// content area takes the layout's width and the bottom of its lowest
    /// part at the window's zoom, with the bars as the script left them; the
    /// frame takes that plus its chrome, no further than the desktop area
    /// reaches from where it is, and its top left stays (script-steps.md,
    /// Adjust Window: inferred). Before the window's content area, or a bar
    /// it shows, was measured, the window fits once it is (`Fit`); without
    /// a layout nothing changes.
    fn resize_to_fit(&mut self) -> HostResult {
        let state = &self.s.file.window.state;
        let Some(layout) = state.layout else {
            return Ok(());
        };
        let Some(render) = self.render(layout) else {
            return Ok(());
        };
        let size = (render.layout.width(), render.layout.height());
        let state = &self.s.file.window.state;
        let fit = state
            .placement
            .as_ref()
            .and_then(|p| p.fit(state.toolbar, state.menubar, state.zoom, size));
        // Before the window's content area was drawn, where its bars sit is
        // not known: the window fits once it was. What the script changed
        // so far (its bars) reaches the window first, in step order with
        // the window commands of later steps.
        let Some((width, height)) = fit else {
            let fit = crate::scripting::Fit {
                layout: size,
                zoom: state.zoom,
                toolbar: state.toolbar,
                menubar: state.menubar,
            };
            self.flush();
            self.send(ScriptEvent::Fit(fit));
            return Ok(());
        };
        self.move_resize_window(
            &WindowTarget::Current,
            Bounds {
                width: Some(width),
                height: Some(height),
                ..Bounds::default()
            },
            false,
        )
    }

    pub(crate) fn table_of(&mut self, id: u32) -> Option<u64> {
        self.layout_table(id)
    }

    fn schema(&self) -> Option<&Arc<CalcSchema>> {
        self.s.file.schema.as_ref().and_then(Option::as_ref)
    }

    /// The schema this host's reads go through: its own, or, while that one
    /// has occurrences waiting for their related files, the provider's,
    /// which grows as they open (#983) so that a read after an open sees
    /// the file.
    fn current_schema(&self) -> Option<Arc<CalcSchema>> {
        let own = self.schema()?;
        if !own.waits() && own.pending_layouts().next().is_none() {
            return Some(Arc::clone(own));
        }
        let grown = self
            .provider
            .try_borrow_mut()
            .ok()
            .and_then(|mut provider| provider.calc_schema().ok().flatten());
        Some(grown.unwrap_or_else(|| Arc::clone(own)))
    }

    pub(super) fn p(&mut self) -> &mut dyn DataProvider {
        self.provider.get_mut().as_mut()
    }

    /// The window and the provider together.
    /// Whether the step runs with full access in the current file: a script
    /// set to run with full access raises its own file's set only, not
    /// that of another file whose window a step works in (script-runtime.md
    /// 9.1).
    fn elevated_here(&self) -> bool {
        self.elevated && self.elevated_file == Some(self.s.file.id)
    }

    /// The current privilege set's name a script step's write carries
    /// (`ScriptWrite::privilege_set`), as this host answers
    /// `Get ( CurrentPrivilegeSetName )` (`Self::session_info`): [Full
    /// Access]'s while the step runs with full access, else the account's;
    /// `None` when the provider does not know the account's.
    /// [`Self::current_set_name`] from a read (`&self`).
    fn privilege_set_name(&self) -> Option<String> {
        let provider = self.provider.borrow();
        let name = if self.elevated_here() {
            provider
                .full_access_privileges()
                .map_or_else(|| "[Full Access]".to_owned(), |set| set.account_set_name)
        } else {
            provider.account_privileges()?.account_set_name
        };
        Some(name).filter(|name| !name.is_empty())
    }

    pub(super) fn current_set_name(&mut self) -> Option<String> {
        let name = if self.elevated_here() {
            self.p()
                .full_access_privileges()
                .map_or_else(|| "[Full Access]".to_owned(), |set| set.account_set_name)
        } else {
            self.p().account_privileges()?.account_set_name
        };
        Some(name).filter(|name| !name.is_empty())
    }

    fn wp(&mut self) -> (&mut Window, &mut dyn DataProvider) {
        (&mut self.s.file.window, self.provider.get_mut().as_mut())
    }

    // ----- talking to the UI --------------------------------------------------

    pub(crate) fn send(&self, event: ScriptEvent) {
        self.send_for(self.s.file.window.id(), event);
    }

    /// Tell window `window` of the UI about `event`.
    pub(crate) fn send_for(&self, window: u32, event: ScriptEvent) {
        let _ = self.responses.send(Response {
            epoch: self.s.epoch,
            window,
            reply: Reply::Script(event),
        });
        self.s.ctx.request_repaint();
    }

    /// Tell the UI what changed in the window. A lost connection is told
    /// first: the UI then keeps the window as it is rather than take what
    /// a step that failed on the lost connection left (drafts dropped,
    /// records to read again).
    pub(crate) fn invalidate_peer_calculations(&mut self) {
        for file in self.files.peers.values_mut() {
            *file.state.cache.get_mut() = CalcData::default();
        }
    }

    pub(crate) fn flush(&mut self) {
        self.connection_lost();
        if let Some(mut update) = self.s.file.window.take_update(false) {
            update.file = self.s.file.id;
            if update.data_changed {
                self.invalidate_peer_calculations();
            }
            self.send(ScriptEvent::Window(Box::new(update)));
        }
    }

    /// Whether the open file's connection to the host was lost; the first
    /// time the UI is told, and the session's scripts stop (see
    /// [`super::session::Shared::connection_lost`]).
    pub(crate) fn connection_lost(&mut self) -> bool {
        let provider = &***self.provider.get_mut();
        self.s.connection_lost(provider, self.responses)
    }

    /// Serve the UI's waiting requests. With `wait_for`, block until the
    /// answer to that prompt arrives; `None` when the session was
    /// interrupted (a file opened or closed, the UI went away, the
    /// connection to the host was lost).
    pub(crate) fn pump(&mut self, wait_for: Option<u64>) -> Option<Answer> {
        loop {
            let job = match wait_for {
                Some(_) => match self.jobs.recv_timeout(worker::CONNECTION_POLL) {
                    Ok(job) => job,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if self.connection_lost() {
                            return None;
                        }
                        continue;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        self.s.interrupted = true;
                        return None;
                    }
                },
                None => match self.jobs.try_recv() {
                    Ok(job) => job,
                    Err(_) => return None,
                },
            };
            match job.request {
                Request::Answer { prompt, answer } => {
                    if Some(prompt) == wait_for {
                        return Some(answer);
                    }
                }
                Request::Script(script) => self.s.queued.push_back((job.epoch, job.ctx, *script)),
                Request::Open { .. } | Request::OpenLocal(_) | Request::Close => {
                    self.s.interrupted = true;
                    self.s.deferred = Some(job);
                    return None;
                }
                request => {
                    let file = self.s.file.id;
                    let Some(target) = self.files.owner(&self.s.file, job.window) else {
                        continue;
                    };
                    self.files.select(target, self.provider.get_mut(), self.s);
                    self.s.observe(&request);
                    let current = self.s.file.window.id();
                    let provider = self.provider.get_mut().as_mut();
                    provider.use_window(job.window);
                    let reply = worker::handle(provider, self.store, request);
                    provider.use_window(current);
                    self.files.select(file, self.provider.get_mut(), self.s);
                    if self
                        .responses
                        .send(Response {
                            epoch: job.epoch,
                            window: job.window,
                            reply,
                        })
                        .is_err()
                    {
                        self.s.interrupted = true;
                        return None;
                    }
                    job.ctx.request_repaint();
                }
            }
        }
    }

    /// Ask the user and wait for the answer; `None` without asking once
    /// the connection to the host is lost.
    fn ask(&mut self, prompt: Prompt) -> Option<Answer> {
        if self.connection_lost() {
            return None;
        }
        self.reveal_window();
        self.flush();
        self.s.next_prompt += 1;
        let id = self.s.next_prompt;
        self.send(ScriptEvent::Prompt { id, prompt });
        self.pump(Some(id))
    }

    /// A "With dialog" confirmation whose `ok` is the default button;
    /// `Err` with error 1 when declined.
    fn confirm(&mut self, title: &str, message: String, ok: &str) -> HostResult {
        self.confirm_with(title, message, ok, true)
    }

    /// [`Self::confirm`], with `ok` the default button or not.
    fn confirm_with(
        &mut self,
        title: &str,
        message: String,
        ok: &str,
        default: bool,
    ) -> HostResult {
        match self.ask(Prompt::Confirm {
            title: title.into(),
            message,
            ok: ok.into(),
            default,
        }) {
            Some(Answer::Confirm(true)) => Ok(()),
            _ => Err(ErrorCode::CANCELED.into()),
        }
    }

    // ----- the layout ---------------------------------------------------------

    /// The window's layout as rendered (triggers, field objects).
    fn render(&mut self, id: u32) -> Option<Arc<om_data::RenderLayout>> {
        if !self.s.file.rendered.contains_key(&id) {
            let render = self.p().render_layout(id).ok().flatten().map(Arc::new);
            self.s.file.rendered.insert(id, render);
        }
        // Rendering can bind a pending related table. Adopt that schema
        // before sync_layout resolves the window's table or publishes it;
        // startup writes and their transaction triggers need the same ids.
        if let Some(schema) = self.current_schema() {
            self.s.file.schema = Some(Some(schema));
        }
        self.s.file.rendered.get(&id).cloned().flatten()
    }

    /// The layouts of the file.
    fn layouts(&mut self) -> Vec<om_data::LayoutInfo> {
        if self.s.file.layouts.is_none() {
            self.s.file.layouts = self.p().layouts().ok();
        }
        self.s.file.layouts.clone().unwrap_or_default()
    }

    /// Whether the account's custom set lets it see layout `id`
    /// (session-identity.md 2.2), from the file's schema or, without one,
    /// its provider. New Window and Go to Related Record still go to one it
    /// may not (#696).
    pub(super) fn layout_shown(&mut self, id: u32) -> bool {
        match self.schema() {
            Some(schema) => schema
                .authenticated_account
                .set_access
                .as_ref()
                .is_none_or(|access| access.shows_layout(u64::from(id))),
            None => self
                .p()
                .account_info()
                .set_access
                .is_none_or(|access| access.shows_layout(u64::from(id))),
        }
    }

    /// Whether the account's privilege set allows the printing (`print`)
    /// and exporting (`export`) a step needs (session-identity.md 2.3).
    /// A step of a script set to run with full access in the file being
    /// printed or exported is allowed them: elevation raises that script's
    /// own file user only (script-runtime.md 9.1).
    fn options_allowed(&mut self, print: bool, export: bool) -> bool {
        if self.elevated_here() {
            return true;
        }
        let allows = |access: &om_data::host::security::SetAccess| {
            (!print || access.prints()) && (!export || access.exports())
        };
        match self.schema() {
            Some(schema) => schema
                .authenticated_account
                .set_access
                .as_ref()
                .is_none_or(allows),
            None => self
                .p()
                .account_info()
                .set_access
                .as_ref()
                .is_none_or(allows),
        }
    }

    /// Error 3 when the set denies what the step needs: FileMaker Pro
    /// 26.0.3 returns 3 for Print, Save Records as PDF, Export Records and
    /// Save Records as Excel, before any dialog and with no alert
    /// ([`Self::denied_quietly`], #651's reading).
    fn require_options(&mut self, print: bool, export: bool) -> HostResult {
        let allowed = self.options_allowed(print, export);
        self.s.options_refused = !allowed;
        if allowed {
            Ok(())
        } else {
            Err(ErrorCode::UNAVAILABLE.into())
        }
    }

    /// Whether error `code` of step `step` is the refusal of
    /// [`Self::require_options`], which FileMaker Pro 26.0.3 stores with no
    /// alert even when errors are not captured (#651's reading). The step
    /// recorded it as it ran: elevation may have ended by the alert.
    fn denied_quietly(&self, step: u16, code: ErrorCode) -> bool {
        code == ErrorCode::UNAVAILABLE
            && matches!(step, 43 | 144 | 36 | 143)
            && self.s.options_refused
    }

    /// Whether the account the window's file was opened with may run
    /// script `id`: not when its custom set gives the script no access
    /// (session-identity.md 2.2).
    pub(super) fn runs_script(&mut self, id: u64) -> bool {
        if let Some(schema) = self.schema() {
            return schema
                .authenticated_account
                .set_access
                .as_ref()
                .is_none_or(|access| access.runs_script(id));
        }
        self.p()
            .account_info()
            .set_access
            .is_none_or(|access| access.runs_script(id))
    }

    /// A layout's table occurrence.
    fn layout_occurrence(&mut self, id: u32) -> Option<u64> {
        match self.render(id) {
            Some(render) => om_layout::occurrence_id(render.layout.def.as_ref()?.table_alias_key),
            // A layout the account's set hides draws nothing; the
            // calculation schema keeps its occurrence (#696).
            None => {
                self.schema()?
                    .layouts()
                    .iter()
                    .find(|l| l.id == id)?
                    .occurrence
            }
        }
    }

    /// The window's layout occurrence.
    fn occurrence(&mut self) -> Option<u64> {
        let layout = self.s.file.window.state.layout?;
        self.layout_occurrence(layout)
    }

    /// The base table of a layout.
    fn layout_table(&mut self, id: u32) -> Option<u64> {
        let occurrence = self.layout_occurrence(id)?;
        match self.schema() {
            Some(schema) => schema.table_of(occurrence).map(|t| t.id),
            None => Some(occurrence),
        }
    }

    /// Fields the layout shows: (occurrence, field) -> name.
    fn layout_fields(&mut self) -> HashMap<(u64, u64), String> {
        let Some(layout) = self.s.file.window.state.layout else {
            return HashMap::new();
        };
        let Some(render) = self.render(layout) else {
            return HashMap::new();
        };
        let alias = render.layout.def.as_ref().map_or(0, |d| d.table_alias_key);
        let Some(occurrence) = om_layout::occurrence_id(alias) else {
            return HashMap::new();
        };
        render
            .layout
            .objects
            .objects
            .iter()
            .filter_map(|object| {
                let spec = object.field_spec()?;
                let name = render.field_bindings.get(&object.id)?;
                (spec.table_key == alias && spec.repetition <= 1)
                    .then(|| Some(((occurrence, spec.field_id()?), name.clone())))
                    .flatten()
            })
            .collect()
    }

    /// A field of the window's table, or its qualified Find criterion.
    fn field_name(&mut self, field: FieldRef) -> Result<String, HostError> {
        let occurrence = self.occurrence();
        if self.s.file.window.state.mode == Mode::Find && occurrence != Some(field.occurrence) {
            return self.qualified_name(field);
        }
        if let Some(schema) = self.schema().cloned() {
            let def = schema
                .field(field.occurrence, field.field)
                .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
            let same_table = occurrence
                .and_then(|o| schema.table_of(o))
                .zip(schema.table_of(field.occurrence))
                .is_some_and(|(a, b)| a.id == b.id);
            if !same_table {
                // A related record's field.
                return Err(HostError::Unsupported);
            }
            if field.repetition > 1 {
                return Err(HostError::Unsupported);
            }
            return Ok(def.name.clone());
        }
        self.layout_fields()
            .get(&(field.occurrence, field.field))
            .cloned()
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))
    }

    /// Find criteria distinguish occurrences even when they reference the
    /// same base table. Keep the qualifier for the provider's relationship
    /// planning or preflight instead of turning it into a base-field find.
    fn find_field_name(&mut self, field: FieldRef) -> Result<String, HostError> {
        let field = FieldRef {
            repetition: 1,
            ..field
        };
        if self.occurrence() == Some(field.occurrence) {
            self.field_name(field)
        } else {
            self.qualified_name(field)
        }
    }

    /// The layout object that shows `field`.
    fn field_object(&mut self, field: &str) -> Option<u64> {
        let layout = self.s.file.window.state.layout?;
        let render = self.render(layout)?;
        super::field_object(&render, field)
    }

    /// New Record and Enter Find Mode put the focus in the first field of
    /// the layout's stored tab order (FileMaker Server's script engine,
    /// s33; calc-functions.md 6.1), a field of the window's table.
    fn focus_first_field(&mut self) {
        let Some(layout) = self.s.file.window.state.layout else {
            return;
        };
        let Some(render) = self.render(layout) else {
            return;
        };
        let alias = render.layout.def.as_ref().map_or(0, |d| d.table_alias_key);
        let first = om_data::host::layout_objects::first_in_tab_order(&render.layout)
            .filter(|o| o.field_spec().is_some_and(|s| s.table_key == alias))
            .and_then(|o| render.field_bindings.get(&o.id).cloned());
        if let Some(field) = first {
            let state = &mut self.s.file.window.state;
            state.focus = Some(field);
            state.object = None;
            state.field_focus = true;
            state.select = false;
            state.selection = None;
            state.typing = None;
            self.s.file.window.touch();
        }
    }

    /// The fields the layout shows, in tab order (top to bottom, left to
    /// right).
    /// Omit Multiple Records' dialog (Claris "Omit Multiple Records"): how
    /// many records to omit, starting with the current one, `count` filled
    /// in. Cancel is 1; a count that is not a whole number above 0 is 5.
    fn ask_omit_count(&mut self, count: i64) -> Result<i64, HostError> {
        let dialog = CustomDialog {
            title: "Omit Multiple".into(),
            message: "Omit how many records, starting with the current one?".into(),
            buttons: vec![
                DialogButton {
                    label: "Omit".into(),
                    commit: true,
                },
                DialogButton {
                    label: "Cancel".into(),
                    commit: false,
                },
            ],
            inputs: vec![DialogInput {
                label: "Records to omit".into(),
                password: false,
                value: Value::text(count.max(1).to_string()),
            }],
        };
        let Some(Answer::Dialog(DialogResponse { button: 1, inputs })) =
            self.ask(Prompt::Dialog(dialog))
        else {
            return Err(ErrorCode::CANCELED.into());
        };
        inputs
            .first()
            .and_then(|value| match value {
                Value::Text(text) => text.as_str().trim().parse::<i64>().ok(),
                _ => None,
            })
            .filter(|count| *count > 0)
            .ok_or(ErrorCode::INVALID.into())
    }

    fn tab_order(&mut self) -> Vec<String> {
        let Some(layout) = self.s.file.window.state.layout else {
            return Vec::new();
        };
        let Some(render) = self.render(layout) else {
            return Vec::new();
        };
        super::tab_order(&render)
    }

    /// Perform Find, Constrain or Extend: a script's, or one run from the
    /// window (`manual`). A script's leaves Find mode first, and an empty
    /// found set when nothing matches. One from the window that matches
    /// nothing keeps the found set and its mode and requests, for the
    /// dialog's Modify Find and Cancel (`error_alert`; client-ui.md,
    /// "Dialogs").
    fn find_with(
        &mut self,
        kind: FindKind,
        requests: Option<&OwnedNode>,
        manual: bool,
    ) -> HostResult {
        let requests = match requests {
            Some(node) => {
                self.commit_window()?;
                self.stored_requests(node)?
            }
            None => self.current_requests()?,
        };
        let imported = self.s.file.window.table.is_some_and(|table| {
            self.schema()
                .is_some_and(|schema| schema.table_context(table).is_some())
        });
        if !imported && !manual {
            self.s.file.window.last_find = requests.clone();
            self.s.file.window.set_mode(Mode::Browse);
        }
        let change = if manual {
            FoundSetChange::ManualFind {
                requests: requests.clone(),
                kind,
            }
        } else {
            FoundSetChange::Find {
                requests: requests.clone(),
                kind,
            }
        };
        let result = self.change(change);
        // Unsupported imported-table forms did not execute a find. Keep
        // the caller's mode and requests available for correction/cancel.
        if imported && matches!(result, Err(HostError::Error(ErrorCode(code::UNSUPPORTED)))) {
            return result;
        }
        self.s.file.window.last_find = requests;
        if manual && matches!(result, Err(HostError::Error(ErrorCode(code::NO_MATCH)))) {
            return result;
        }
        self.s.file.window.set_mode(Mode::Browse);
        self.after(TriggerEvent::ModeEnter, None);
        self.after(TriggerEvent::RecordLoad, None);
        result
    }

    /// The fields Quick Find searches on the window's layout; `None` when
    /// the layout has Quick Find turned off.
    fn quick_find_fields(&mut self) -> Option<Vec<om_data::quick_find::QuickFindField>> {
        let layout = self.s.file.window.state.layout?;
        let render = self.render(layout)?;
        let schema = self.schema()?.clone();
        om_data::quick_find::fields(&render.layout, &schema)
    }

    // ----- triggers -----------------------------------------------------------

    /// The trigger record for `event` on the layout (or on `object`).
    fn trigger_record(
        &mut self,
        event: TriggerEvent,
        object: Option<u64>,
    ) -> Option<ScriptTrigger> {
        let layout = self.s.file.window.state.layout?;
        let render = self.render(layout)?;
        match object {
            Some(object) => super::object_trigger(&render.layout, object, event).cloned(),
            None => super::layout_trigger(&render.layout, event).cloned(),
        }
    }

    fn trigger(
        &mut self,
        event: TriggerEvent,
        object: Option<u64>,
        action: Option<u64>,
    ) -> Option<Trigger> {
        let record = self.trigger_record(event, object)?;
        let mode = self.s.file.window.state.mode;
        let window = self.s.file.window.id();
        super::make_trigger(event, &record, (window, mode), action, true).map(|mut t| {
            t.file = self.s.file.id;
            t
        })
    }

    /// Queue a delayed trigger of the window's layout (or object).
    fn after(&mut self, event: TriggerEvent, object: Option<u64>) {
        if let Some(trigger) = self.trigger(event, object, None) {
            self.triggers.push(trigger);
        }
    }

    /// The delayed triggers of entering a layout, in FileMaker's call
    /// order: OnRecordLoad, then OnLayoutEnter. Each trigger's frame goes on
    /// top of the last, so OnLayoutEnter's script runs first
    /// (script-runtime.md 13.17).
    fn after_layout_entered(&mut self) {
        self.after(TriggerEvent::RecordLoad, None);
        self.after(TriggerEvent::LayoutEnter, None);
    }

    /// The layout object the focus is in: the field object of a field of
    /// the layout's table, or the field the active portal row has the
    /// focus in.
    fn focused_object(&mut self) -> Option<u64> {
        match self.s.file.window.state.focus.clone() {
            Some(field) => self.field_object(&field),
            None => match self.s.file.window.state.portal.and_then(|r| r.field) {
                Some(object) => Some(object),
                // A later repetition of the record's own field (#185).
                None => self
                    .own_repetition_focus()
                    .and(self.s.file.window.state.object),
            },
        }
    }

    /// Whether the user changed the field the focus is in: a field of the
    /// layout's table being typed into (`typing`), or a portal row's field
    /// whose related record the user typed it into (the row's draft's typed
    /// cells), unless a step already saved that edit (`saved`, #1040). A
    /// script's Set Field does not count; nor does another field's change
    /// (#1030).
    fn focused_field_modified(&mut self) -> bool {
        let state = &self.s.file.window.state;
        if state.mode != Mode::Browse {
            return false;
        }
        if let Some(field) = &state.focus {
            return state.typing.as_ref() == Some(field);
        }
        let Some((index, cell)) = self.focused_related_cell() else {
            return false;
        };
        self.s
            .file
            .window
            .state
            .uncommitted
            .related
            .get(index)
            .is_some_and(|draft| {
                !draft.deleted
                    && draft.typed.contains(&cell)
                    && draft.saved.get(&cell) != Some(&draft.cells.get(&cell).cloned().flatten())
            })
    }

    /// A later repetition of the layout's own field with the keyboard.
    /// Its numeric editor has no first-repetition name draft or portal row.
    fn own_repetition_focus(&mut self) -> Option<FieldRef> {
        let state = &self.s.file.window.state;
        if state.focus.is_some() || state.portal.is_some() {
            return None;
        }
        let numeric_focus = state.numeric_focus;
        let (layout, object) = (state.layout?, state.object?);
        let render = self.render(layout)?;
        let spec = render.layout.objects.get(object)?.field_spec()?;
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        let repetition = numeric_focus.filter(|(id, _)| *id == object).map_or_else(
            || u32::try_from(spec.repetition.max(1)).ok(),
            |(_, repetition)| Some(repetition),
        )?;
        (Some(occurrence) == self.s.file.occurrence && repetition > 1).then_some(FieldRef {
            occurrence,
            field: spec.field_id()?,
            repetition,
        })
    }

    /// The draft (its index in `Uncommitted::related`) and the cell of the
    /// portal row's field the focus is in: a stored record's draft by its
    /// table and record, whichever occurrence it was edited through
    /// (`sync_related_edits`), or the row's begun record's.
    fn focused_related_cell(&mut self) -> Option<(usize, u64)> {
        if self.s.file.window.state.portal.is_none() {
            // A later repetition of the record's own field: its cell in the
            // record's own draft.
            let field = self.own_repetition_focus()?;
            let cell = om_data::rules::cell_of(field.field, field.repetition)?;
            let table = self.schema()?.table_of(field.occurrence)?.id;
            let state = &self.s.file.window.state;
            let record = u64::try_from(state.record?).ok()?;
            let index = state
                .uncommitted
                .related
                .iter()
                .position(|draft| draft.table == table && draft.record == Some(record))?;
            return Some((index, cell));
        }
        let state = &self.s.file.window.state;
        let row = state.portal.filter(|row| row.row > 0)?;
        let (object, layout) = (row.field?, state.layout?);
        let field = self
            .render(layout)
            .and_then(|render| render.layout.objects.get(object)?.field_spec()?.field_id())?;
        let cell = writes::cell_of(field, row.repetition.max(1));
        let table = self
            .schema()
            .and_then(|s| s.table_of(row.occurrence))
            .map(|t| t.id);
        let index = match self.active_row_record(row.occurrence) {
            Some(record) => self
                .s
                .file
                .window
                .state
                .uncommitted
                .related
                .iter()
                .position(|d| Some(d.table) == table && d.record == Some(record)),
            None => self.active_begun_row(row.occurrence),
        }?;
        Some((index, cell))
    }

    /// The portal field the focus is in, when the user typed into it: what
    /// its OnObjectSave stage saves.
    fn saving_cell(&mut self) -> Option<SavedCell> {
        let (index, cell) = self.focused_related_cell()?;
        let draft = self.s.file.window.state.uncommitted.related.get(index)?;
        draft.typed.contains(&cell).then_some(SavedCell {
            table: draft.table,
            record: draft.record,
            begun: draft.begun,
            cell,
        })
    }

    /// A step passed the OnObjectSave stage of portal field `saving`, its
    /// trigger letting it: the value it holds is saved, and leaving the
    /// field again does not save it again while it holds that value
    /// (#1040). A trigger that refused it saved nothing.
    fn related_field_saved(&mut self, saving: SavedCell) {
        let draft = self
            .s
            .file
            .window
            .state
            .uncommitted
            .related
            .iter_mut()
            .find(|d| {
                d.table == saving.table && d.record == saving.record && d.begun == saving.begun
            });
        if let Some(draft) = draft {
            let value = draft.cells.get(&saving.cell).cloned().flatten();
            draft.saved.insert(saving.cell, value);
        }
    }

    /// The pre-event triggers an action passes, in order.
    fn stages(&mut self, act: &Act) -> Vec<(TriggerEvent, Option<u64>)> {
        let focus = self.focused_object();
        let modified =
            self.s.file.window.is_modified() && self.s.file.window.state.mode == Mode::Browse;
        // A step that leaves the field the user changed saves it first, as
        // the user's own exit does (`app/scripts.rs` `exit_stages`; Claris,
        // "OnObjectSave": "after object data has been validated and saved,
        // but before the field is exited"; #1030). Revert Record does not
        // save it.
        let field_modified = focus.is_some() && self.focused_field_modified();
        let leaving_object = |stages: &mut Vec<(TriggerEvent, Option<u64>)>, saves: bool| {
            if let Some(object) = focus {
                if saves && field_modified {
                    stages.push((TriggerEvent::ObjectValidate, Some(object)));
                    stages.push((TriggerEvent::ObjectSave, Some(object)));
                }
                stages.push((TriggerEvent::ObjectExit, Some(object)));
            }
        };
        let mut stages = Vec::new();
        match act {
            Act::Layout(_) => {
                leaving_object(&mut stages, true);
                if modified {
                    stages.push((TriggerEvent::RecordCommit, None));
                }
                stages.push((TriggerEvent::LayoutExit, None));
            }
            Act::Record(_) | Act::NewRecord | Act::Duplicate | Act::Commit(_) | Act::Print(_) => {
                leaving_object(&mut stages, true);
                if modified {
                    stages.push((TriggerEvent::RecordCommit, None));
                }
            }
            Act::Revert => {
                leaving_object(&mut stages, false);
                if modified {
                    stages.push((TriggerEvent::RecordRevert, None));
                }
            }
            Act::Mode(_) => {
                leaving_object(&mut stages, true);
                if modified {
                    stages.push((TriggerEvent::RecordCommit, None));
                }
                stages.push((TriggerEvent::ModeExit, None));
            }
            Act::Portal { .. } => leaving_object(&mut stages, true),
            Act::ClosePopover {
                popover,
                field,
                modified,
                ..
            } => {
                if let Some(object) = *field {
                    if *modified {
                        stages.push((TriggerEvent::ObjectValidate, Some(object)));
                        stages.push((TriggerEvent::ObjectSave, Some(object)));
                    }
                    stages.push((TriggerEvent::ObjectExit, Some(object)));
                }
                stages.push((TriggerEvent::ObjectExit, Some(*popover)));
            }
            Act::Focus(field) => {
                leaving_object(&mut stages, true);
                // Go to Field [ ] leaves the field and commits (6.3 O2).
                if field.is_none() && focus.is_some() && modified {
                    stages.push((TriggerEvent::RecordCommit, None));
                }
            }
        }
        stages
    }

    /// Run `act` from `stage`: the next pre-event trigger holds it, or it
    /// happens now.
    fn run_act(&mut self, act: Act, from: usize) -> HostResult {
        self.run_act_from(act, from, None)
    }

    /// [`Self::run_act`] resuming after `fired`'s stage when it is given
    /// (a held action whose trigger let it go on); a closing popover's
    /// stages are fixed by the action and resume at `from`.
    fn run_act_from(&mut self, act: Act, from: usize, fired: Option<TriggerEvent>) -> HostResult {
        if let Act::ClosePopover {
            window,
            layout,
            popover,
            ..
        } = &act
        {
            // A trigger can change windows/layouts or close the popover
            // itself. Never apply its held close to another context.
            if self.s.file.window.id() != *window
                || self.s.file.window.state.layout != Some(*layout)
                || self.s.file.window.state.popovers.get(&u64::from(*layout)) != Some(popover)
            {
                return Ok(());
            }
        }
        let stages = self.stages(&act);
        let from = match fired {
            Some(fired) if !matches!(act, Act::ClosePopover { .. }) => stages
                .iter()
                .position(|(event, _)| stage_rank(*event) > stage_rank(fired))
                .unwrap_or(stages.len()),
            _ => from,
        };
        for (i, (event, object)) in stages.iter().enumerate().skip(from) {
            let saving = (*event == TriggerEvent::ObjectSave
                && self.s.file.window.state.focus.is_none())
            .then(|| self.saving_cell())
            .flatten();
            // A trigger script repeating its own event does not fire it
            // again.
            let fires = !self
                .s
                .firing
                .values()
                .any(|f| *f == (self.s.file.window.id(), event.id()))
                && self.trigger_record(*event, *object).is_some();
            if fires {
                self.s.next_token += 1;
                let token = self.s.next_token;
                if let Some(trigger) = self.trigger(*event, *object, Some(token)) {
                    self.s.held.insert(
                        token,
                        Held {
                            act,
                            stage: i + 1,
                            fired: *event,
                            window: self.s.file.window.id(),
                            saving,
                        },
                    );
                    self.s
                        .firing
                        .insert(token, (self.s.file.window.id(), event.id()));
                    self.triggers.push(trigger);
                    return Ok(());
                }
            }
            // No trigger held the stage: it passed.
            if let Some(saving) = saving {
                self.related_field_saved(saving);
            }
        }
        self.perform(act)
    }

    /// An action whose pre-event triggers let it happen.
    fn perform(&mut self, act: Act) -> HostResult {
        match act {
            Act::Layout(id) => {
                self.commit_window()?;
                self.show_layout(id);
                self.after_layout_entered();
            }
            Act::Record(index) => {
                self.commit_window()?;
                // The active portal row's number carries over (s35 A7).
                let row = self.s.file.window.state.portal;
                self.s.file.window.go_to(index);
                if self.s.file.window.state.mode == Mode::Browse {
                    self.after(TriggerEvent::RecordLoad, None);
                    self.keep_row(row)?;
                }
            }
            Act::Commit(skip_validation) => {
                self.commit_window_with(skip_validation)?;
                self.s.file.window.state.focus = None;
                self.s.file.window.state.object = None;
                self.s.file.window.state.portal = None;
                self.s.file.window.touch();
            }
            Act::Print(request) => return self.dispatch_print(request),
            Act::Revert => {
                let new = self.s.file.window.is_new();
                self.discard_record().map_err(data_error)?;
                let window = &mut self.s.file.window;
                window.state.focus = None;
                window.state.object = None;
                if !new {
                    // The record as the host has it now, as Refresh Window
                    // reads it: a save refused for another client's change
                    // shows that change once the draft goes.
                    window.refresh_data();
                }
            }
            Act::Mode(mode) => {
                self.commit_window()?;
                self.s.file.window.set_mode(mode);
                if mode == Mode::Find {
                    self.focus_first_field();
                }
                self.after(TriggerEvent::ModeEnter, None);
                if mode == Mode::Browse {
                    self.after(TriggerEvent::RecordLoad, None);
                }
            }
            Act::Portal { row, select } => {
                let row = match row {
                    Some(mut row) => {
                        let portal = self.portal_by_id(row.object);
                        row.identity = match portal {
                            Some(portal) if !portal.ordered && row.row > 0 => {
                                self.row_of(&portal, row.row)?
                            }
                            _ => None,
                        };
                        // In a portal with an order of its own the row keeps
                        // its record (#192, `Portal::own_order`).
                        row.clicked =
                            portal.is_some_and(|portal| portal.own_order) && row.identity.is_some();
                        Some(row)
                    }
                    None => None,
                };
                let state = &mut self.s.file.window.state;
                state.focus = None;
                state.object = None;
                state.typing = None;
                state.selection = None;
                state.select = select;
                // A field of the row has the focus now: the window gives
                // its editor the keyboard.
                state.field_focus = row.is_some_and(|row| row.field.is_some());
                state.portal = row;
                self.s.file.window.touch();
            }
            Act::ClosePopover {
                layout,
                popover,
                button,
                ..
            } => {
                let layout = u64::from(layout);
                let state = &mut self.s.file.window.state;
                state.popovers.remove(&layout);
                state.focus = None;
                state.object = None;
                state.typing = None;
                state.selection = None;
                state.portal = None;
                self.send(ScriptEvent::ClosePopover {
                    layout,
                    popover,
                    button,
                });
                self.s.file.window.touch();
                if let Some(button) = button {
                    self.after(TriggerEvent::ObjectEnter, Some(button));
                }
            }
            Act::Focus(field) => {
                // Leaving the field (a portal row's too) for none commits
                // the record (calc-functions.md 6.3 O2, s33), which leaves
                // the portal (13.24).
                if field.is_none() && self.focused_object().is_some() {
                    self.commit_window()?;
                    self.s.file.window.state.portal = None;
                }
                let object = field.as_deref().and_then(|f| self.field_object(f));
                let state = &mut self.s.file.window.state;
                if state.focus != field {
                    state.typing = None;
                    state.selection = None;
                }
                state.focus = field.clone();
                state.object = object;
                state.field_focus = field.is_some();
                if object.is_some() {
                    // A field of the layout's table is the active object
                    // now, as entering a portal row ends the field's focus:
                    // no portal row is active [I].
                    state.portal = None;
                }
                self.s.file.window.touch();
                if let Some(object) = object {
                    self.after(TriggerEvent::ObjectEnter, Some(object));
                }
            }
            Act::NewRecord | Act::Duplicate => {
                self.commit_window()?;
                if matches!(act, Act::Duplicate) {
                    self.create_duplicate()?;
                } else {
                    let set = self.current_set_name();
                    let (window, provider) = self.wp();
                    window
                        .new_record(provider, FieldChanges::new(), Default::default(), set)
                        .map_err(data_error)?;
                }
                self.s.file.cache.borrow_mut().records.clear();
                self.focus_first_field();
                self.after(TriggerEvent::RecordLoad, None);
            }
        }
        Ok(())
    }

    /// Show layout `id` in the view the window last showed it in while
    /// the layout still offers it, else in the view the file stores for it
    /// (layout-model.md 4.2 view flags).
    fn show_layout(&mut self, id: u32) {
        // Go to Related Record names a hidden layout it went to; any other
        // way there does not (#696).
        self.s.file.window.state.hidden_named = false;
        let table = self.layout_table(id);
        let flags = self
            .render(id)
            .and_then(|r| r.layout.def.as_ref().map(|d| d.flags))
            .unwrap_or(0);
        let view = self
            .s
            .file
            .window
            .state
            .views
            .get(&id)
            .copied()
            .filter(|view| view.enabled(flags))
            .unwrap_or(View::stored(flags));
        // The layout left keeps the entry it has: a layout gets one only
        // when it is shown or View As is chosen on it, never from a view
        // the window still had from another layout while it loaded.
        self.s.file.window.set_layout(id, table);
        let (window, provider) = self.wp();
        window.start_on_saved_record(provider);
        let state = &mut self.s.file.window.state;
        state.view = view;
        state.views.insert(id, view);
    }

    /// Commit the current record and its related records (no triggers;
    /// the caller passed them), as a script's writes.
    fn commit_window(&mut self) -> HostResult {
        self.commit_window_with(false)
    }

    /// [`Self::commit_window`] with Commit Records' "Skip data entry
    /// validation". The records written go to OnWindowTransaction, at
    /// Commit Transaction when a transaction is open.
    fn commit_window_with(&mut self, skip_validation: bool) -> HostResult {
        // What the commit sends its hosts from here on, through any file
        // (#152).
        let mark = self.p().submission_mark();
        let apart = self.record_apart();
        let mut unused = self.unchanged_container_drafts()?;
        self.take_typed_globals()?;
        // The related changes as they were before the commit filled the
        // match fields of the rows begun in portals.
        let unmatched = self.s.file.window.state.uncommitted.related.clone();
        self.match_begun_records()?;
        let numbers = self.begun_record_numbers();
        let ids = self.layout_field_ids();
        let table = self.s.file.window.table;
        // Deleted related records' trigger field is read before they go.
        let deleted: Vec<(u64, u64, String)> = self
            .s
            .file
            .window
            .state
            .uncommitted
            .related
            .clone()
            .into_iter()
            .filter(|d| d.deleted)
            .filter_map(|d| {
                let record = d.record?;
                Some((d.table, record, self.transaction_content(d.table, record)))
            })
            .collect();
        // A normal save across files uses a window record session. Its
        // preflight refusal stays open for correction; a partly applied
        // save is retained by the provider for explicit recovery (#1006).
        let lost_before = self.connection_lost();
        let had_related = !self.s.file.window.state.uncommitted.related.is_empty();
        if had_related
            && self.s.file.transaction.is_none()
            && !self.s.file.window.state.uncommitted.session
            && self.p().begin_record_commit().is_ok()
        {
            // A normal save owns a record session too: a preflight refusal
            // must keep its newly reserved related records for correction.
            self.s.file.window.state.uncommitted.session = true;
        }
        self.leave_provider_owned(&mut unused, &mut Vec::new());
        let pending = self.s.file.window.pending();
        let committed = {
            let set = self.current_set_name();
            let (window, provider) = self.wp();
            window.commit(
                provider,
                &|name| ids.get(name).copied(),
                skip_validation,
                set,
                &numbers,
            )
        };
        *self.s.file.cache.borrow_mut() = CalcData::default();
        let committed = match committed {
            Ok(committed) => {
                self.adjust_containers(&unused, -1);
                // Committed, but the connection went with it (the lock
                // release after it): the window may never learn, and keep
                // the related changes the host has now, a row begun among
                // them. A reconnect cannot tell, so it cannot keep them
                // (#152).
                if had_related && !lost_before && self.connection_lost() {
                    self.s.file.lost_commit = true;
                }
                committed
            }
            Err(e) => {
                self.s.file.window.restore(pending);
                let carried = self.p().carried_record_sessions();
                let window = &mut self.s.file.window;
                // A standalone new parent's unreserved portal preview follows
                // the preview carry path. Only reserved rows belong to the
                // retained multi-file receipt.
                window.state.uncommitted.related_recoverable =
                    !window.state.uncommitted.related.is_empty()
                        && window
                            .state
                            .uncommitted
                            .related
                            .iter()
                            .all(|draft| draft.record.is_some() || draft.reserved.is_some())
                        && carried.contains(&window.id());
                self.s.file.window.changed_data();
                // A commit lost on its way with related changes (#152). If
                // it sent no host any record data, through any file, they
                // stay as they were before it: read back and checked after
                // a reconnect. The rows begun take their match fields
                // again at the next save. Otherwise whether a host has
                // them cannot be told, so a reconnect cannot keep them,
                // unless the connection was lost before the commit began.
                if (e.lost || self.connection_lost())
                    && !self.s.file.window.state.uncommitted.related.is_empty()
                {
                    let later = self.p().submission_mark();
                    match mark.map(|mark| mark.until(later)) {
                        Some(om_data::Submitted::Nothing) => {
                            self.s.file.window.state.uncommitted.related = unmatched;
                            self.s.file.unsent_save = true;
                        }
                        _ if lost_before => {}
                        _ => self.s.file.lost_commit = true,
                    }
                }
                return Err(self.failed(e));
            }
        };
        let entries = committed
            .into_iter()
            .filter_map(|c| {
                let table = c.table.or(table)?;
                let record = u64::try_from(c.record).ok()?;
                let (operation, content) = if c.deleted {
                    let content = deleted
                        .iter()
                        .find(|(t, r, _)| *t == table && *r == record)
                        .map(|(_, _, c)| c.clone())
                        .unwrap_or_default();
                    (transaction::Operation::Deleted, content)
                } else if c.new {
                    (
                        transaction::Operation::New,
                        self.transaction_content(table, record),
                    )
                } else {
                    (
                        transaction::Operation::Modified,
                        self.transaction_content(table, record),
                    )
                };
                Some(transaction::Entry {
                    table,
                    record: c.record,
                    operation,
                    content,
                })
            })
            .collect();
        if apart {
            // Committed apart from the open transaction: its records are
            // not the transaction's, for OnWindowTransaction or a later
            // Revert Transaction (#1103).
            let transaction = self.s.file.transaction.take();
            self.committed_records(entries);
            self.s.file.transaction = transaction;
        } else {
            self.committed_records(entries);
        }
        Ok(())
    }

    /// The current window holds its record in its own record session, set
    /// aside while a script's transaction of another window is open
    /// (#892): that record and its changes are outside the transaction
    /// (script-runtime.md 8.5), and the transaction's steps leave them
    /// alone (#1103).
    fn record_apart(&mut self) -> bool {
        if !self.s.file.window.state.uncommitted.session {
            return false;
        }
        let provider = self.p();
        provider.record_session_held() && !provider.record_session_open()
    }

    /// The user's typing into a global field of the layout's table sets
    /// the session's value, as `DataProvider::update_record` does: it is
    /// no change of the record.
    fn take_typed_globals(&mut self) -> HostResult {
        let Some(schema) = self.schema().cloned() else {
            return Ok(());
        };
        let Some(table) = self.s.file.occurrence.and_then(|o| schema.table_of(o)) else {
            return Ok(());
        };
        let globals: Vec<(String, u64, bool)> = self
            .s
            .file
            .window
            .state
            .drafts
            .keys()
            .filter_map(|name| {
                let field = table.fields.values().find(|f| f.name == *name)?;
                field.global.then(|| {
                    (
                        name.clone(),
                        field.id,
                        matches!(
                            field.value_type,
                            om_data::calc::ValueType::Date
                                | om_data::calc::ValueType::Time
                                | om_data::calc::ValueType::Timestamp
                        ),
                    )
                })
            })
            .collect();
        for (name, field, temporal) in globals {
            let text = self
                .s
                .file
                .window
                .state
                .drafts
                .get(&name)
                .cloned()
                .flatten()
                .unwrap_or_default();
            let result = if temporal {
                self.p().set_typed_global(table.id, field, 1, &text)
            } else {
                self.p().set_global(table.id, field, 1, &text)
            };
            result.map_err(|e| self.failed(e))?;
            self.s.file.window.state.drafts.remove(&name);
            *self.s.file.cache.borrow_mut() = CalcData::default();
            self.s.file.window.changed_data();
        }
        // Later repetitions keep their cell keys in the same draft store as
        // related editors. Globals still change only this session's values.
        let mut globals = Vec::new();
        for (index, draft) in self
            .s
            .file
            .window
            .state
            .uncommitted
            .related
            .iter()
            .enumerate()
        {
            if draft.table != table.id {
                continue;
            }
            for (cell, value) in &draft.cells {
                let (field, repetition) = match table.fields.get(cell) {
                    Some(field) => (field, 1),
                    None => {
                        let Some(field) = table.fields.get(&(cell >> 8)) else {
                            continue;
                        };
                        (field, (cell & 255) as u32)
                    }
                };
                if field.global {
                    globals.push((
                        index,
                        *cell,
                        field.id,
                        repetition,
                        matches!(
                            field.value_type,
                            om_data::calc::ValueType::Date
                                | om_data::calc::ValueType::Time
                                | om_data::calc::ValueType::Timestamp
                        ),
                        value.clone().unwrap_or_default(),
                    ));
                }
            }
        }
        for (index, cell, field, repetition, temporal, text) in globals {
            let result = if temporal {
                self.p()
                    .set_typed_global(table.id, field, repetition, &text)
            } else {
                self.p().set_global(table.id, field, repetition, &text)
            };
            result.map_err(|error| self.failed(error))?;
            let draft = &mut self.s.file.window.state.uncommitted.related[index];
            draft.cells.remove(&cell);
            draft.typed.remove(&cell);
            *self.s.file.cache.borrow_mut() = CalcData::default();
            self.s.file.window.changed_data();
        }
        self.s
            .file
            .window
            .state
            .uncommitted
            .related
            .retain(|draft| draft.deleted || draft.record.is_none() || !draft.cells.is_empty());
        Ok(())
    }

    /// The user saved the window's record (`Start::Commit`): it is
    /// committed with what scripts left on it, as a script commits it, the
    /// user's typing by a user's rules (`ScriptWrite::typed`); its
    /// triggers fired before. A failure leaves the record open and is
    /// the user's message.
    pub(crate) fn save_record(&mut self) -> Result<(), String> {
        self.s.failure = None;
        let record = self
            .s
            .file
            .window
            .state
            .layout
            .zip(self.s.file.window.state.record);
        match self.commit_window() {
            Ok(()) => Ok(()),
            Err(error) => {
                let code = match &error {
                    HostError::Error(code) => Some(code.0),
                    _ => None,
                };
                let message = self.failure_message(error);
                if let (Some((layout, record)), Some(code @ (101 | 300 | 301 | 306))) =
                    (record, code)
                {
                    self.send(ScriptEvent::SaveRefused {
                        layout,
                        record,
                        error: DataError::with_code(code, message.clone()),
                    });
                }
                Err(message)
            }
        }
    }

    /// The user reverted the window's record (`Start::Revert`): its drafts
    /// and what scripts left on it go, and a record New Record made is
    /// deleted.
    pub(crate) fn revert_record(&mut self) -> Result<(), String> {
        self.discard_record().map_err(|e| e.message)
    }

    /// Revert the window's record, for the user (`Start::Revert`) and for
    /// Revert Record/Request (51), which Records > Revert Record... runs:
    /// its drafts and what scripts left on it go, and a record New Record
    /// made is deleted. The references its container values took, its
    /// related records' included, go with them (containers.md 5.1).
    fn discard_record(&mut self) -> Result<(), DataError> {
        let mut taken = self.container_drafts();
        let mut related = self.related_container_drafts();
        self.leave_provider_owned(&mut taken, &mut related);
        let (window, provider) = self.wp();
        let reverted = window.revert(provider);
        // A confirmed delete settles the drafts even if reading the new
        // found count then fails. A refused delete retains them all.
        let settled = window.state.drafts.is_empty() && window.state.uncommitted.is_empty();
        if settled {
            self.adjust_containers(&taken, -1);
            for (table, reference) in related {
                let _ = self.p().adjust_related_container(table, &reference, -1);
            }
        }
        *self.s.file.cache.borrow_mut() = CalcData::default();
        reverted
    }

    /// The message of a step's failure: the provider's, else the error's.
    fn failure_message(&mut self, error: HostError) -> String {
        match (self.s.failure.take(), error) {
            (Some(message), _) => message,
            (None, HostError::Error(code)) => code.to_string(),
            (None, _) => ErrorCode(3).to_string(),
        }
    }

    /// OnWindowTransaction's entry for deleting the current record, its
    /// field read before the deletion.
    fn deleted_entry(&mut self) -> Option<transaction::Entry> {
        let table = self.s.file.window.table?;
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).ok()??
        };
        Some(transaction::Entry {
            table,
            record,
            operation: transaction::Operation::Deleted,
            content: self.transaction_content(table, u64::try_from(record).ok()?),
        })
    }

    /// The ids of the layout table's fields by name; empty without a
    /// calculation schema.
    fn layout_field_ids(&self) -> HashMap<String, u64> {
        self.schema()
            .zip(self.s.file.occurrence)
            .and_then(|(schema, occurrence)| schema.table_of(occurrence))
            .map(|table| {
                table
                    .fields
                    .values()
                    .map(|f| (f.name.clone(), f.id))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn unchanged_container_drafts(&mut self) -> Result<Vec<String>, HostError> {
        let (window, provider) = self.wp();
        if window.state.mode != Mode::Browse || window.state.drafts.is_empty() {
            return Ok(Vec::new());
        }
        let drafts = window.state.drafts.clone();
        let Some((fields, record)) = window.current(provider).map_err(data_error)? else {
            return Ok(Vec::new());
        };
        Ok(fields
            .iter()
            .zip(&record.values)
            .filter_map(|(field, stored)| {
                let value = stored.as_ref()?;
                (field.kind == om_data::FieldKind::Container
                    && drafts.get(&field.name)?.as_ref() == Some(value)
                    && om_data::containers::reference(value).is_some())
                .then(|| value.clone())
            })
            .collect())
    }

    /// The current record's values a duplicate takes; each container
    /// value copied takes a reference to its data (containers.md 5.1).
    fn duplicate_values(&mut self) -> Result<(FieldChanges, Vec<String>), HostError> {
        let layout = self.s.file.window.state.layout.unwrap_or_default();
        let capabilities = self.p().record_capabilities(layout).map_err(data_error)?;
        if !capabilities.create {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        let containers: Option<BTreeSet<String>> = self.schema().and_then(|schema| {
            let table = schema.table_of(self.s.file.occurrence?)?;
            Some(
                table
                    .fields
                    .values()
                    .filter(|f| {
                        f.class == FieldClass::Data
                            && f.stored
                            && !f.global
                            && f.value_type == ValueType::Container
                    })
                    .map(|f| f.name.clone())
                    .collect(),
            )
        });
        // A serial number is not copied: the copy takes the next one
        // (`duplicate-record-request.html`).
        let stored_fields: Option<BTreeSet<String>> = self.schema().and_then(|schema| {
            Some(
                schema
                    .table_of(self.s.file.occurrence?)?
                    .fields
                    .values()
                    .filter(|f| {
                        f.class == FieldClass::Data
                            && f.stored
                            && !f.global
                            && !f.auto_enters_serial()
                    })
                    .map(|f| f.name.clone())
                    .collect(),
            )
        });
        let (window, provider) = self.wp();
        let Some((fields, record)) = window.current(provider).map_err(data_error)? else {
            return Err(ErrorCode::RECORD_MISSING.into());
        };
        let container = |field: &om_data::Field| field.kind == om_data::FieldKind::Container;
        if containers.is_none() && fields.iter().any(container) {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        let containers = containers.unwrap_or_default();
        let values: FieldChanges = fields
            .iter()
            .zip(&record.values)
            .filter(|(field, value)| {
                capabilities.can_edit(field)
                    && stored_fields
                        .as_ref()
                        .is_none_or(|fields| fields.contains(&field.name))
                    || container(field)
                        && containers.contains(&field.name)
                        && value.as_ref().is_some_and(|v| !v.is_empty())
            })
            .map(|(field, value)| (field.name.clone(), value.clone()))
            .collect();
        let copied: Vec<String> = values
            .iter()
            .filter(|(name, _)| containers.contains(*name))
            .filter_map(|(_, value)| value.as_ref())
            .filter(|value| om_data::containers::reference(value).is_some())
            .cloned()
            .collect();
        let mut taken = Vec::new();
        for reference in copied {
            if let Err(error) = self.p().adjust_container(layout, &reference, 1) {
                self.adjust_containers(&taken, -1);
                return Err(self.failed(error));
            }
            taken.push(reference);
        }
        Ok((values, taken))
    }

    fn create_duplicate(&mut self) -> HostResult {
        // Held as New Record holds it (#892). Its session opens before the
        // copy takes its container references, which are then the
        // session's: released if it is reverted, kept by its commit.
        let session = {
            let (window, provider) = self.wp();
            window.open_new_record(provider)
        };
        let (values, taken) = match self.duplicate_values() {
            Ok(copy) => copy,
            Err(error) => {
                if session {
                    let _ = self.p().revert_transaction();
                }
                return Err(error);
            }
        };
        let ids = self.layout_field_ids();
        let cells = values
            .iter()
            .filter_map(|(name, value)| Some((*ids.get(name)?, value.clone())))
            .collect();
        let privilege_set = self.current_set_name();
        let made = {
            let (window, provider) = self.wp();
            window.make_new_record(provider, session, (values, cells), privilege_set)
        };
        let made = match made {
            Ok(made) => made,
            Err(error) => {
                // A coded refusal did not create a record. A transport or
                // verification failure may have committed: retain its
                // references rather than risk deleting stored data. A
                // session's own went with it.
                if error.code.is_some() && !session {
                    self.adjust_containers(&taken, -1);
                }
                return Err(self.failed(error));
            }
        };
        let (window, provider) = self.wp();
        window.adopt_new_record(provider, made).map_err(data_error)
    }

    /// Whether the window's field `name` is a container field.
    fn is_container_field(&mut self, name: &str) -> bool {
        let (window, provider) = self.wp();
        matches!(window.current(provider), Ok(Some((fields, _)))
            if fields.iter().any(|f| f.name == name && f.kind == om_data::FieldKind::Container))
    }

    /// The container references the window's unsaved values hold.
    fn container_drafts(&mut self) -> Vec<String> {
        let (window, provider) = self.wp();
        let containers: Vec<String> = match window.current(provider) {
            Ok(Some((fields, _))) => fields
                .iter()
                .filter(|f| f.kind == om_data::FieldKind::Container)
                .map(|f| f.name.clone())
                .collect(),
            _ => return Vec::new(),
        };
        containers
            .iter()
            .filter_map(|name| window.state.drafts.get(name).cloned().flatten())
            .collect()
    }

    /// Provider transactions and detached recovery receipts settle their own
    /// acquisitions. Subtract counts, rather than deduplicating strings: two
    /// fields can hold independently acquired references to the same member.
    fn leave_provider_owned(&mut self, own: &mut Vec<String>, related: &mut Vec<(u64, String)>) {
        let table = self.s.file.window.table;
        let mut counts = self.p().owned_container_drafts();
        let mut caller_owned = |table: u64, reference: &String| {
            let Some(n) = counts.get_mut(&(table, reference.clone())) else {
                return true;
            };
            if *n == 0 {
                return true;
            }
            *n -= 1;
            false
        };
        if let Some(table) = table {
            own.retain(|r| caller_owned(table, r));
        }
        related.retain(|(table, reference)| caller_owned(*table, reference));
    }

    /// Add `delta` to the counts of the library members `references` name
    /// (op87); a failure only leaves a count off, never data lost.
    fn adjust_containers(&mut self, references: &[String], delta: i32) {
        let layout = self.s.file.window.state.layout.unwrap_or_default();
        for reference in references {
            let _ = self.p().adjust_container(layout, reference, delta);
        }
    }

    /// Set Field into a container field (containers.md 3, 5.1; script
    /// oracle s34): data goes into the table's library and the field holds
    /// its reference, anything else its text. A reference the same edit set
    /// before is released.
    fn set_container(&mut self, name: &str, value: Value) -> HostResult {
        let layout = self.s.file.window.state.layout.unwrap_or_default();
        let mut taken = None;
        let text = match &value {
            Value::Container(c) if !c.is_empty() => {
                let open = &self.s.file.window.state.uncommitted;
                // Related-first entry has a session but no Root field yet.
                let prepare_root = !open.new && (!open.session || open.attachment_field.is_none());
                if prepare_root {
                    let (record, version) = {
                        let (window, provider) = self.wp();
                        let record = window
                            .current(provider)
                            .map_err(data_error)?
                            .ok_or(ErrorCode::RECORD_MISSING)?
                            .1
                            .id;
                        (record, window.state.record_version)
                    };
                    let field = self.layout_field_ids().get(name).copied();
                    let held = if c.library.is_some() {
                        self.p()
                            .prepare_stored_container_edit(
                                om_data::ContainerEditBaseline {
                                    layout,
                                    record,
                                    version,
                                },
                                name,
                                c,
                            )
                            .map_err(|e| self.failed(e))?
                            == om_data::EditPreparation::Prepared
                    } else {
                        self.p()
                            .prepare_container_edit(layout, record, version, name)
                            .map_err(|e| self.failed(e))?
                    };
                    if held {
                        let open = &mut self.s.file.window.state.uncommitted;
                        open.session = true;
                        open.attachment_field = field;
                    }
                }
                match self.p().container_reference(layout, name, c) {
                    Ok(Some(reference)) => {
                        taken = Some(reference.clone());
                        reference
                    }
                    Ok(None) => c.text.as_str().to_owned(),
                    Err(e) => return Err(self.failed(e)),
                }
            }
            other => self.text(other),
        };
        if taken.is_none() && om_data::containers::reference(&text).is_some() {
            // A literal string is not an owned container reference. In
            // particular, later draft cleanup must not release stored data
            // on behalf of a string that never acquired a reference.
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        let replaced = self.s.file.window.state.drafts.get(name).cloned().flatten();
        let assigned = text.clone();
        if let Err(error) = self.set_named(name, text) {
            if let Some(taken) = taken {
                self.adjust_containers(&[taken], -1);
            }
            return Err(error);
        }
        if self.s.file.window.state.drafts.contains_key(name) {
            if let Some(field) = self.layout_field_ids().get(name).copied() {
                self.s
                    .file
                    .window
                    .state
                    .uncommitted
                    .cells
                    .insert(field, (!assigned.is_empty()).then_some(assigned));
            }
        }
        if let Some(replaced) = replaced {
            self.adjust_containers(&[replaced], -1);
        }
        if !self.s.file.window.state.drafts.contains_key(name) {
            // Setting the stored value to itself leaves no draft to own
            // the new reference, and no later commit to release the old.
            if let Some(taken) = taken {
                self.adjust_containers(&[taken], -1);
            }
        }
        Ok(())
    }

    // ----- calculations -------------------------------------------------------

    /// Run `f` on the file's data through the provider's calculation
    /// schema, with the window's drafts on the current record. `None`
    /// without a schema.
    fn file<R>(&self, f: impl Fn(&FileCalc<'_>) -> R) -> Option<R> {
        let schema = self.current_schema()?;
        let (result, waited) = self.file_over(&schema, &f);
        if !waited {
            return Some(result);
        }
        // It read an occurrence of a related file not open yet, which the
        // provider opened meanwhile (#983): a script waits for it, so the
        // read runs again over the schema that has it.
        let grown = self.provider.borrow_mut().calc_schema().ok().flatten()?;
        Some(self.file_over(&grown, &f).0)
    }

    /// [`Self::file`] over `schema`, and whether a read waited for a
    /// related file ([`CalcNeed::Occurrence`]).
    fn file_over<R>(&self, schema: &Arc<CalcSchema>, f: &impl Fn(&FileCalc<'_>) -> R) -> (R, bool) {
        let schema = Arc::clone(schema);
        let provider = &self.provider;
        let fetch = |needs: &[CalcNeed]| provider.borrow_mut().calc_data(needs).ok();
        let mut source = CachedSource::new(&self.s.file.cache).with_fetch(&fetch);
        for (table, record, values) in self.overlays(&schema) {
            source = source.with_overlay(table, record, values);
        }
        // Summaries break into the groups of the window's sort. A related
        // key stays in place as unresolved rather than being dropped.
        let window = &self.s.file.window;
        if let (true, Some(occurrence)) = (window.sorted, self.s.file.occurrence) {
            if let Some(table) = schema.table_of(occurrence) {
                let fields: Arc<[u64]> = window
                    .last_sort
                    .iter()
                    .map(|s| schema.sort_key_field(table.id, &s.field))
                    .collect();
                source = source.with_found_sort(occurrence, fields);
            }
        }
        if let Some(info) = &self.report_session {
            source = source.with_calculation_session(info);
        }
        let file = FileCalc::new(&schema, &source, self.today);
        let file = self
            .report_session
            .as_ref()
            .map_or(file, |info| file.with_session(info));
        let result = f(&file);
        let waited = source
            .missing()
            .iter()
            .any(|need| matches!(need, CalcNeed::Occurrence { .. } | CalcNeed::Layout { .. }));
        (result, waited)
    }

    /// Field `field` (repetition `repetition`) of occurrence `occurrence`
    /// of `record` when it is a record a related draft is creating
    /// ([`BEGUN_RECORD`]): what was typed or set into it, else what its
    /// rules enter as previewed for an ordinary commit
    /// (`RelatedDraft::prepared`, #1047), read in the field's type; empty
    /// for a field neither gives (its auto-entry is not known before then);
    /// `None` for any other record. It never reaches the provider.
    fn begun_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Option<Value> {
        if record == NO_RECORD {
            return None;
        }
        let index = usize::try_from(record.checked_sub(BEGUN_RECORD)?).ok()?;
        let draft = self.s.file.window.state.uncommitted.related.get(index)?;
        let schema = self.schema()?;
        let table = schema.table(draft.table)?;
        if schema.table_of(occurrence)?.id != table.id {
            return None;
        }
        let def = table.fields.get(&field)?;
        let cell = writes::cell_of(field, repetition.max(1));
        // What was typed or set, over what its rules entered before an
        // ordinary commit's OnObjectSave script (#1047).
        let text = draft
            .cells
            .get(&cell)
            .or_else(|| draft.prepared.as_ref()?.values.get(&cell))
            .cloned()
            .flatten()
            .unwrap_or_default();
        let cx = Ctx::new(&schema.locale, self.today);
        Some(stored_value(&text, def.value_type, &cx))
    }

    /// The records with unsaved values, as calculations see them: the
    /// current record with the window's drafts (and the repetitions a
    /// script set), and the related records a script changed through it.
    /// Records a script is creating are not seen until they are written.
    fn overlays(&self, schema: &CalcSchema) -> Vec<(u64, u64, Arc<om_data::host::RecordValues>)> {
        let state = &self.s.file.window.state;
        if state.mode != Mode::Browse {
            return Vec::new();
        }
        let mut out = Vec::new();
        let cx = Ctx::new(&schema.locale, self.today);
        let own = (!state.drafts.is_empty()
            || !self.s.file.window.cells().is_empty()
            || state.uncommitted.related.iter().any(|draft| {
                Some(draft.table) == self.s.file.window.table
                    && draft.record == state.record.and_then(|id| u64::try_from(id).ok())
            }))
        .then(|| {
            let record = u64::try_from(state.record?).ok()?;
            let table = schema.table_of(self.s.file.occurrence?)?;
            Some((table, record))
        })
        .flatten();
        if let Some((table, record)) = own {
            if let Some(mut values) = self.stored_values(table.id, record) {
                // Its stored calculations and other entered values as the
                // commit would store them over what the window changed
                // (#1104); a field the provider cannot know now keeps its
                // stored value. The window's own changes come over them.
                if let Some(pending) = self.pending_own(table, record) {
                    let known: super::Cells = pending
                        .values
                        .iter()
                        .filter(|(cell, _)| {
                            let field = if table.fields.contains_key(cell) {
                                **cell
                            } else {
                                **cell / 256
                            };
                            !pending.unknown.contains(&field)
                        })
                        .map(|(cell, value)| (*cell, value.clone()))
                        .collect();
                    apply_cells(&mut values, table, &known, &cx);
                }
                // The window's changes over them in the order its commit
                // writes them (`Window::commit`): a script's cells, then the
                // user's typing, newer where both hold a cell (a script's
                // write of a typed cell takes that typing away,
                // `Window::forget_typed_cell`).
                apply_cells(&mut values, table, self.s.file.window.cells(), &cx);
                for (name, value) in &state.drafts {
                    if let Some(def) = table.fields.values().find(|f| f.name == *name) {
                        let text = value.clone().unwrap_or_default();
                        values.set(def.id, 1, stored_value(&text, def.value_type, &cx));
                    }
                }
                // What the user typed into its later repetitions (its own
                // related draft, #185); a global's is the session's.
                for draft in state
                    .uncommitted
                    .related
                    .iter()
                    .filter(|d| d.table == table.id && d.record == Some(record) && !d.deleted)
                {
                    let typed: super::Cells = draft
                        .cells
                        .iter()
                        .filter(|(cell, _)| {
                            let field = if table.fields.contains_key(cell) {
                                **cell
                            } else {
                                **cell >> 8
                            };
                            table.fields.get(&field).is_some_and(|def| !def.global)
                        })
                        .map(|(cell, value)| (*cell, value.clone()))
                        .collect();
                    apply_cells(&mut values, table, &typed, &cx);
                }
                out.push((table.id, record, Arc::new(values)));
            }
        }
        for draft in &self.s.file.window.state.uncommitted.related {
            if Some(draft.table) == self.s.file.window.table
                && draft.record == state.record.and_then(|id| u64::try_from(id).ok())
            {
                continue;
            }
            let (Some(record), Some(table)) = (draft.record, schema.table(draft.table)) else {
                continue;
            };
            if let Some(mut values) = self.stored_values(table.id, record) {
                apply_cells(&mut values, table, &draft.cells, &cx);
                out.push((table.id, record, Arc::new(values)));
            }
        }
        out
    }

    /// The window's own record `record` of `table` as its commit would
    /// store it, computed by the provider over the window's unsaved changes
    /// and its other unsaved records (`DataProvider::compute_pending`,
    /// #1104), once per change of them. `None` when the provider cannot
    /// compute it, or the window holds a change it cannot be told: a
    /// related record to delete with the commit.
    fn pending_own(
        &self,
        table: &om_data::host::TableDef,
        record: u64,
    ) -> Option<Arc<om_data::PendingValues>> {
        let state = &self.s.file.window.state;
        let open = &state.uncommitted;
        if open.related.iter().any(|draft| draft.deleted) {
            return None;
        }
        // Its cells as its commit writes them (`Window::commit`): a value
        // no script set is the user's typing.
        let mut cells = open.cells.clone();
        let mut typed = std::collections::BTreeSet::new();
        for (name, value) in &state.drafts {
            if let Some(def) = table.fields.values().find(|f| f.name == *name) {
                if open.cells.get(&def.id) != Some(value) {
                    typed.insert(def.id);
                }
                cells.insert(def.id, value.clone());
            }
        }
        for draft in &open.related {
            if draft.table == table.id && draft.record == Some(record) {
                cells.extend(draft.cells.clone());
                typed.extend(draft.typed.iter().copied());
            }
        }
        // The write its commit would make (`Window::commit`), in the
        // window's context: its layout and the session's privilege set.
        let own = om_data::ScriptWrite {
            layout: state.layout?,
            table: None,
            record: Some(i64::try_from(record).ok()?),
            cells,
            fields: state.drafts.clone(),
            typed,
            skip_validation: false,
            privilege_set: self.privilege_set_name(),
            record_number: None,
            prepared: None,
        };
        let others: Vec<om_data::PendingRecord> = open
            .related
            .iter()
            .filter(|draft| draft.table != table.id || draft.record != Some(record))
            .map(|draft| {
                Some(om_data::PendingRecord {
                    table: draft.table,
                    record: match draft.record {
                        Some(record) => Some(i64::try_from(record).ok()?),
                        None => None,
                    },
                    cells: draft.previewed_cells(),
                    typed: draft.typed.clone(),
                })
            })
            .collect::<Option<_>>()?;
        // A new record the window holds in its own record session, set
        // aside while another window's transaction is open (#892), is
        // computed in that session, as its commit enters it
        // (`Window::commit`): the other transaction does not hold its
        // creation values.
        let apart = open.session && {
            let provider = self.provider.borrow();
            provider.record_session_held() && !provider.record_session_open()
        };
        let generation = self.provider.borrow().calc_generation();
        let key = (own, others, generation, apart);
        if let Some((cached, computed)) = &*self.s.file.pending.borrow() {
            if *cached == key {
                return computed.clone();
            }
        }
        let (computed, generation) = {
            let mut provider = self.provider.borrow_mut();
            let entered = apart && provider.enter_record_session();
            let computed = if apart && !entered {
                // It cannot be computed where its commit would write it.
                None
            } else {
                // Left again whatever the computation does, a panic
                // included: the other transaction must not stay set aside.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    provider.compute_pending(&key.0, &key.1)
                }));
                if entered {
                    provider.leave_record_session();
                }
                match result {
                    Ok(answer) => answer.ok().flatten(),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            };
            // The generation after entering and leaving, which the next
            // read finds when nothing changed since.
            (computed.map(Arc::new), provider.calc_generation())
        };
        let key = (key.0, key.1, generation, key.3);
        *self.s.file.pending.borrow_mut() = Some((key, computed.clone()));
        computed
    }

    /// Record `record` of `table` as stored, read into the cache when
    /// needed.
    fn stored_values(&self, table: u64, record: u64) -> Option<om_data::host::RecordValues> {
        if !self
            .s
            .file
            .cache
            .borrow()
            .records
            .contains_key(&(table, record))
        {
            let need = CalcNeed::Record { table, record };
            let data = self.provider.borrow_mut().calc_data(&[need]).ok()?;
            self.s.file.cache.borrow_mut().merge(data);
        }
        let cache = self.s.file.cache.borrow();
        cache.records.get(&(table, record))?.as_deref().cloned()
    }

    /// A field of the current record, as a value of its kind (without a
    /// calculation schema), read in the file's formats when known.
    fn value_of(&mut self, name: &str) -> Option<Value> {
        let (window, provider) = self.wp();
        let (fields, _) = window.current(provider).ok()??;
        let kind = fields.iter().find(|f| f.name == name)?.kind;
        let text = window.value(provider, name).ok()??;
        let ty = match kind {
            om_data::FieldKind::Number => ValueType::Number,
            om_data::FieldKind::Date => ValueType::Date,
            om_data::FieldKind::Time => ValueType::Time,
            om_data::FieldKind::Timestamp => ValueType::Timestamp,
            om_data::FieldKind::Container => ValueType::Container,
            om_data::FieldKind::Text => ValueType::Text,
        };
        Some(if text.is_empty() {
            Value::empty(ty)
        } else {
            stored_value(&text, ty, &Ctx::new(CalcHost::locale(self), self.today))
        })
    }

    /// `GetLayoutObjectAttribute` on the window's layout, its record and
    /// focused field (`om_data::host::layout_objects`), in screen points
    /// from the desktop area as the window was last drawn
    /// (calc-functions.md 6.3 O5).
    fn layout_object_attribute(&mut self, args: &[Value], context: &Context) -> Value {
        let Some(schema) = self.schema().cloned() else {
            return Value::empty_text();
        };
        let info = self.session_info();
        om_data::host::layout_objects::answer(self, &schema, &info, args, context)
    }

    /// `Get ( RecordAccess )` for the window's current record under a set
    /// the desktop enforces (session-identity.md 2.4): 0 without a current
    /// record; a custom set's answer judges the record as the provider reads
    /// it, so it is asked only for this selector. `None` (the set's own
    /// answer) for a built-in set's record, in Find mode, whose cursor is a
    /// request [I], and when the record cannot be read.
    fn record_access_here(&mut self) -> Option<i64> {
        let state = &self.s.file.window.state;
        if state.mode == Mode::Find {
            return None;
        }
        let layout = state.layout?;
        let custom = match self
            .schema()
            .and_then(|s| s.authenticated_account.set_access.as_ref())?
        {
            om_data::host::security::SetAccess::Custom(_) => true,
            om_data::host::security::SetAccess::BuiltIn(_) => false,
        };
        let (window, provider) = self.wp();
        let current = window.current(provider).ok()?.map(|(_, record)| record.id);
        match current {
            None => Some(0),
            Some(id) if custom => self.p().record_access(layout, id),
            Some(_) => None,
        }
    }

    /// What the window tells `Get`: the file and account, the window on
    /// the desktop and its bars, the layout and view, the found set (in
    /// Find mode the requests), the active field with its text and
    /// selection, the record's open state and changed fields
    /// (calc-functions.md 6.3).
    fn session_info(&mut self) -> SessionInfo {
        // Nested SQL/design hosts must share the physical report page too.
        if let Some(info) = &self.report_session {
            return info.clone();
        }
        let context = self.s.file.context.clone();
        let file_name = om_data::file_refs::file_stem(&context.file).to_owned();
        let layouts = self.layouts();
        let state = self.s.file.window.state.clone();
        let layout = state.layout.and_then(|id| {
            layouts
                .iter()
                .position(|l| l.id == id)
                .map(|i| (i, &layouts[i]))
        });
        let occurrence = self.s.file.occurrence;
        let schema = self.schema().cloned();
        // A portal's field answers the focus selectors while no field of
        // the layout's table has the focus.
        let in_portal = state.focus.is_none();
        let portal_field = self.portal_field_name(&state).filter(|_| in_portal);
        let portal_object = self.portal_object_name(&state).filter(|_| in_portal);
        let own_repetition = self.own_repetition_focus();
        let own_text = own_repetition.and_then(|field| {
            let record = u64::try_from(state.record?).ok()?;
            let value = self
                .file(|file| {
                    file.field_value(field.occurrence, record, field.field, field.repetition)
                })?
                .ok()?;
            Some(self.text(&value))
        });
        let portal_text = self
            .portal_field_text(&state)
            .filter(|_| in_portal)
            .or(own_text);
        let find = state.mode == Mode::Find;
        let modified = self.modified_fields();
        let host_ip = self.host_ip();
        let windows = self.window_names();
        let privileges = self.p().account_privileges();
        // The account's set on this layout (session-identity.md 2.4); the
        // record's answer is read only for `Get ( RecordAccess )`
        // (`Self::record_access_here`).
        let layout_access_here = self
            .schema()
            .and_then(|s| s.authenticated_account.set_access.as_ref())
            .zip(state.layout)
            .map(|(access, layout)| access.layout_access(u64::from(layout)));
        // A layout the set hides has no number: FileMaker Pro answers 0
        // (session-identity.md 2.5, #696).
        let layout_shown = self
            .schema()
            .and_then(|s| s.authenticated_account.set_access.as_ref())
            .zip(state.layout)
            .is_none_or(|(access, layout)| access.shows_layout(u64::from(layout)));
        let current_privileges = self.elevated_here().then(|| {
            let mut current =
                self.p()
                    .full_access_privileges()
                    .unwrap_or_else(|| om_data::host::Privileges {
                        account_set_name: "[Full Access]".into(),
                        ..Default::default()
                    });
            // The runtime establishes full access for this step
            // (script-runtime.md 9.1), independently of catalog availability.
            current.layout_access = Some(2);
            current.record_access = Some(2);
            current
        });
        let (window, provider) = self.wp();
        let found = if find {
            Some(state.find.len() as u64)
        } else {
            window.found_count(provider).ok()
        };
        let focus_text = match &state.focus {
            Some(field) if !find => window.value(provider, field).ok().flatten(),
            Some(field) => state
                .find
                .get(state.find_index)
                .and_then(|r| r.get(field).map(str::to_owned)),
            None => portal_text,
        };
        // A script's later repetitions are cells only, not drafts by name.
        let record_open_state = if find {
            0
        } else if window.is_new() {
            1
        } else if !state.drafts.is_empty() || !window.cells().is_empty() {
            2
        } else {
            0
        };
        let sorted = window.sorted;
        let table_name = occurrence
            .and_then(|o| schema.as_ref()?.occurrence(o).map(|o| o.name.clone()))
            .unwrap_or_default();
        // A field in a portal row has the focus: its name and occurrence.
        let (active_field_name, active_field_table_name) = match portal_field {
            Some(named) => named,
            None if state.focus.is_some() => {
                (state.focus.clone().unwrap_or_default(), table_name.clone())
            }
            None => own_repetition
                .and_then(|field| {
                    let schema = schema.as_ref()?;
                    Some((
                        schema.field(field.occurrence, field.field)?.name.clone(),
                        schema.occurrence(field.occurrence)?.name.clone(),
                    ))
                })
                .unwrap_or_default(),
        };
        let length = focus_text
            .as_deref()
            .map_or(0, |t| t.chars().count() as u64);
        let mut windows: Vec<_> = windows
            .into_iter()
            .map(|name| (file_name.clone(), name))
            .collect();
        for file in self.files.peers.values() {
            let name = om_data::file_refs::file_stem(&file.state.context.file);
            windows.extend(
                std::iter::once(&file.state.window)
                    .chain(file.state.others.iter().rev())
                    .filter(|w| !w.state.hidden)
                    .map(|w| {
                        (
                            name.to_owned(),
                            w.state.title.clone().unwrap_or_else(|| name.to_owned()),
                        )
                    }),
            );
        }
        let other_files = self
            .files
            .peers
            .values()
            .filter_map(|f| f.state.schema.as_ref()?.as_ref())
            .map(|schema| om_data::host::session::OpenFile {
                schema: schema.clone(),
            })
            .collect();
        let open_position = self
            .files
            .peers
            .keys()
            .filter(|id| **id < self.s.file.id)
            .count();
        SessionInfo {
            account_name: context.account.clone(),
            account_info: self.p().account_info(),
            privilege_set_name: privileges
                .as_ref()
                .map(|p| p.account_set_name.clone())
                .unwrap_or_default(),
            privileges,
            current_privileges,
            file_path: match &context.local {
                Some(path) => om_data::file_refs::local_file_path(path),
                None => format!("fmnet:/{}/{file_name}.fmp12", context.host),
            },
            window_name: state.title.clone().unwrap_or_else(|| file_name.clone()),
            file_name,
            // A local file's host is this computer.
            host_name: match &context.local {
                Some(_) => om_data::host::machine::Machine::current().host_name.clone(),
                None => context.host.clone(),
            },
            host_ip_address: host_ip,
            host_application_version: self.p().host_application_version().unwrap_or_default(),
            // Encrypted, the client not verifying the host's certificate;
            // a local file has no connection.
            connection_state: if context.local.is_some() { 0 } else { 2 },
            window_mode: match state.mode {
                Mode::Browse => WindowMode::Browse,
                Mode::Find => WindowMode::Find,
                Mode::Preview => WindowMode::Preview,
                Mode::Layout => WindowMode::Layout,
            },
            window: state.placement.as_ref().map(|p| {
                let mut info = p.window_info(state.toolbar, state.menubar, state.zoom, &state.uuid);
                info.style = state.style.number();
                info
            }),
            layout_name: layout.map(|(_, l)| l.name.clone()).unwrap_or_default(),
            layout_number: layout.map(|(i, _)| if layout_shown { i as u64 + 1 } else { 0 }),
            layout_count: Some(layouts.len() as u64),
            layout_access_here,
            record_access_here: None,
            layout_table_name: table_name.clone(),
            layout_view_state: Some(match state.view {
                View::Form => 0,
                View::List => 1,
                View::Table => 2,
            }),
            found_count: found,
            page_number: Some(state.preview_page + 1),
            active_record_number: Some(if find {
                state.find_index as u64 + 1
            } else if found.unwrap_or(0) > 0 {
                state.index + 1
            } else {
                0
            }),
            active_field_name,
            active_field_table_name,
            // 0 without an active row, as FileMaker Server answers it
            // (calc-functions.md 6, s31).
            active_portal_row_number: Some(state.portal.map_or(0, |p| p.row)),
            // In a portal: the field's repetition in the row, 0 with the
            // row or the portal itself active (script-runtime.md 13.24);
            // elsewhere the Get selectors' own answer.
            active_repetition_number: state
                .portal
                .filter(|_| in_portal)
                .map(|p| p.field.map_or(0, |_| u64::from(p.repetition.max(1))))
                .or(own_repetition.map(|field| u64::from(field.repetition))),
            active_layout_object: portal_object,
            active_field_contents: focus_text.unwrap_or_default(),
            active_selection: state
                .selection
                .or_else(|| state.select.then_some((1, length))),
            modifier_keys: state.modifiers,
            request_count: if find {
                state.find.len() as u64
            } else {
                state.requests
            },
            request_omit: state.find.get(state.find_index).is_some_and(|r| r.omit),
            sorted,
            record_open_state: Some(record_open_state),
            modified_fields: modified,
            windows,
            other_files,
            open_position,
            utc_offset: om_provider::local_utc_offset(),
            ..SessionInfo::for_this_machine()
        }
    }

    /// `Get ( ModifiedFields )`: the fields Set Field and other steps
    /// changed (not one still being typed into), the stored calculations
    /// on them and a new record's auto-entered fields.
    fn modified_fields(&mut self) -> Vec<String> {
        let (Some(schema), Some(occurrence)) = (self.schema().cloned(), self.s.file.occurrence)
        else {
            return Vec::new();
        };
        let Some(table) = schema.table_of(occurrence) else {
            return Vec::new();
        };
        let state = &self.s.file.window.state;
        if state.mode != Mode::Browse {
            return Vec::new();
        }
        let id = |name: &str| table.fields.values().find(|f| f.name == name).map(|f| f.id);
        let mut changed: Vec<u64> = state
            .drafts
            .keys()
            .filter(|f| state.typing.as_ref() != Some(*f))
            .filter_map(|f| id(f))
            .collect();
        // A script's later repetitions (cell `F * 256 + n`) change field F,
        // and so does the user's typing into them (the record's own related
        // draft, #185), but for the one being typed into.
        let record = state.record.and_then(|id| u64::try_from(id).ok());
        let typed: Vec<u64> = state
            .uncommitted
            .related
            .iter()
            .filter(|d| d.table == table.id && d.record == record && !d.deleted)
            .flat_map(|d| d.cells.keys().copied())
            .collect();
        let focused = self
            .own_repetition_focus()
            .and_then(|f| om_data::rules::cell_of(f.field, f.repetition));
        let state = &self.s.file.window.state;
        for cell in self
            .s
            .file
            .window
            .cells()
            .keys()
            .chain(typed.iter().filter(|cell| Some(**cell) != focused))
        {
            let field = if table.fields.contains_key(cell) {
                *cell
            } else {
                cell >> 8
            };
            let typing = table
                .fields
                .get(&field)
                .is_some_and(|f| state.typing.as_deref() == Some(f.name.as_str()));
            if table.fields.contains_key(&field) && !typing && !changed.contains(&field) {
                changed.push(field);
            }
        }
        let entered: Vec<u64> = self
            .s
            .file
            .window
            .auto_entered()
            .iter()
            .filter_map(|f| table.fields.values().find(|d| d.name == *f))
            .filter(|d| auto_enters(d))
            .map(|d| d.id)
            .collect();
        modified_fields(&schema, table.id, &changed, &entered)
    }

    /// The host's IPv4 address, resolved once.
    fn host_ip(&mut self) -> String {
        let host = self.s.file.context.host.clone();
        if let Some((name, ip)) = &self.s.file.host_ip {
            if *name == host {
                return ip.clone();
            }
        }
        use std::net::ToSocketAddrs;
        let ip = (host.as_str(), 5003)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addresses| addresses.find(|a| a.is_ipv4()))
            .map(|a| a.ip().to_string())
            .unwrap_or_default();
        self.s.file.host_ip = Some((host, ip.clone()));
        ip
    }

    /// Text of a value, as the field would store it.
    fn text(&self, value: &Value) -> String {
        display(value, self)
    }

    /// The field an insert or clipboard step works on.
    fn target_name(&mut self, field: Option<FieldRef>) -> Result<String, HostError> {
        match field {
            Some(field) => self.field_name(field),
            None => self
                .s
                .file
                .window
                .state
                .focus
                .clone()
                .ok_or(HostError::Error(ErrorCode::FIELD_MISSING)),
        }
    }

    fn set_named(&mut self, name: &str, text: String) -> HostResult {
        let (window, provider) = self.wp();
        window.set(provider, name, text).map_err(data_error)
    }

    fn change(&mut self, change: FoundSetChange) -> HostResult {
        let (window, provider) = self.wp();
        let result = window.change_found_set(provider, change);
        *self.s.file.cache.borrow_mut() = CalcData::default();
        result.map(|_| ()).map_err(data_error)
    }

    /// The window's find requests, or 400 outside Find mode.
    fn current_requests(&self) -> Result<Vec<FindRequest>, HostError> {
        if self.s.file.window.state.mode != Mode::Find {
            return Err(ErrorCode(code::EMPTY_CRITERIA).into());
        }
        Ok(self.s.file.window.state.find.clone())
    }

    /// Resolve a spec's field reference. A key that is not a table
    /// occurrence (a base table, or none) means the layout's occurrence.
    fn spec_field_ref(&mut self, field: om_data::specs::SpecField) -> FieldRef {
        let occurrence = field
            .occurrence
            .filter(|o| {
                self.schema()
                    .map_or(*o >= 0xd0_0000, |s| s.occurrence(*o).is_some())
            })
            .or(self.s.file.occurrence)
            .unwrap_or(0);
        // A repetition criterion on a field matches its value (11.2.4 F12).
        FieldRef {
            occurrence,
            field: field.field,
            repetition: 1,
        }
    }

    /// A spec's field as a field of the window's table.
    fn spec_field(&mut self, field: om_data::specs::SpecField) -> Result<String, HostError> {
        let field = self.spec_field_ref(field);
        self.field_name(field)
    }

    /// The window's layout name and the fields it shows with their names,
    /// alphabetically, for the Sort Records dialog's "Current Layout".
    fn sort_dialog_layout(&mut self) -> (String, Vec<(om_data::specs::SpecField, String)>) {
        let Some(layout) = self.s.file.window.state.layout else {
            return (String::new(), Vec::new());
        };
        let name = self
            .layouts()
            .into_iter()
            .find(|l| l.id == layout)
            .map(|l| l.name)
            .unwrap_or_default();
        let Some(render) = self.render(layout) else {
            return (name, Vec::new());
        };
        let mut fields: Vec<(om_data::specs::SpecField, String)> = Vec::new();
        for object in &render.layout.objects.objects {
            let (Some(spec), Some(bound)) =
                (object.field_spec(), render.field_bindings.get(&object.id))
            else {
                continue;
            };
            let (Some(occurrence), Some(field)) =
                (om_layout::occurrence_id(spec.table_key), spec.field_id())
            else {
                continue;
            };
            let field = om_data::specs::SpecField {
                occurrence: Some(occurrence),
                field,
                repetition: 1,
            };
            if !fields.iter().any(|(f, _)| *f == field) {
                fields.push((field, bound.clone()));
            }
        }
        // Alphabetical, as FileMaker Pro lists them (#840's reading:
        // Category, Due Date, Status, Task on the Tasks starter's Task List).
        fields.sort_by_cached_key(|(_, name)| name.to_lowercase());
        (name, fields)
    }

    /// A sort made from field names (no stored order) as the order the
    /// Sort Records dialog shows: each name's field of the window's table
    /// (without the file's schema, the layout's field of that name), with
    /// its direction and language; a value list's or summary's reordering
    /// is not recovered from names.
    fn spec_of(
        &mut self,
        sort: &[SortField],
        layout: &[(om_data::specs::SpecField, String)],
    ) -> om_data::specs::SortSpec {
        let occurrence = self.occurrence();
        let schema = self.schema().cloned();
        let table = occurrence
            .zip(schema.as_deref())
            .and_then(|(o, s)| s.table_of(o));
        let fields = sort
            .iter()
            .filter_map(|s| {
                let field = match table {
                    Some(table) => om_data::specs::SpecField {
                        occurrence,
                        field: table.fields.values().find(|f| f.name == s.field)?.id,
                        repetition: 1,
                    },
                    None => layout.iter().find(|(_, name)| *name == s.field)?.0,
                };
                Some(om_data::specs::SortEntry {
                    field,
                    summary: None,
                    value_list: None,
                    descending: s.order == SortOrder::Descending,
                    locale: s.language.unwrap_or(0),
                })
            })
            .collect();
        om_data::specs::SortSpec {
            keep_sorted: true,
            blanks_last: sort.first().is_some_and(|s| s.blanks_last),
            fields,
        }
    }

    /// A stored sort order (script-steps.md 11.1) as sort fields of the
    /// window's table: a value list's order from the provider (dividers
    /// left out), the language override, and a summary field's reordering
    /// (its kind and summarized field from the calculation schema).
    fn sort_fields(&mut self, spec: om_data::specs::SortSpec) -> Result<Vec<SortField>, HostError> {
        // Check actual occurrences before spec_field can collapse another
        // occurrence of the same imported table to a bare base-field name,
        // and before resolving any value-list/summary options.
        for entry in &spec.fields {
            self.check_imported_sort_field(entry.field.occurrence, entry.field.repetition.into())?;
        }
        let mut sort = Vec::new();
        for entry in spec.fields {
            let value_list = match entry.value_list {
                Some(id) => Some(
                    self.p()
                        .value_list(id)
                        .map_err(|e| self.failed(e))?
                        .into_iter()
                        .filter(|v| v.trim() != "-")
                        .collect(),
                ),
                None => None,
            };
            let summary = match entry.summary {
                Some(summary) => Some(self.summary_order(summary)?),
                None => None,
            };
            sort.push(SortField {
                field: self.spec_field(entry.field)?,
                order: if entry.descending {
                    SortOrder::Descending
                } else {
                    SortOrder::Ascending
                },
                blanks_last: spec.blanks_last,
                value_list,
                language: (entry.locale != 0).then_some(entry.locale),
                summary,
            });
        }
        Ok(sort)
    }

    /// Imported sorts are bounded to the layout's base occurrence. A stored
    /// base-table key (or none) still means that occurrence, as in spec_field.
    /// An actual occurrence must retain its identity even for a self join.
    fn check_imported_sort_field(
        &mut self,
        occurrence: Option<u64>,
        repetition: u32,
    ) -> HostResult {
        let base = self.occurrence();
        if let (Some(schema), Some(base)) = (self.schema(), base) {
            let imported = schema
                .table_of(base)
                .is_some_and(|table| schema.table_context(table.id).is_some());
            if imported
                && (repetition > 1
                    || occurrence.is_some_and(|occurrence| {
                        occurrence != base
                            && (schema.occurrence(occurrence).is_some() || occurrence >= 0xd0_0000)
                    }))
            {
                return Err(ErrorCode(code::UNSUPPORTED).into());
            }
        }
        Ok(())
    }

    /// "Reorder based on summary field": the summary's function and the
    /// field it summarizes (file-metadata.md 5.1).
    fn summary_order(
        &mut self,
        summary: om_data::specs::SpecField,
    ) -> Result<om_data::found::SummaryOrder, HostError> {
        use om_data::found::{SummaryFunction, SummaryOrder};
        use om_data::host::SummaryKind;
        let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
        let occurrence = summary
            .occurrence
            .filter(|o| schema.occurrence(*o).is_some())
            .or(self.s.file.occurrence)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        let def = schema
            .field(occurrence, summary.field)
            .and_then(|f| f.summary)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        let function = match def.kind {
            SummaryKind::Total => SummaryFunction::Total,
            SummaryKind::Count => SummaryFunction::Count,
            SummaryKind::Average => SummaryFunction::Average,
            SummaryKind::Minimum => SummaryFunction::Minimum,
            SummaryKind::Maximum => SummaryFunction::Maximum,
            _ => return Err(HostError::Unsupported),
        };
        // The summarized field of the same table (file-metadata.md:
        // `None` occurrence); a summary of nothing or of another table's
        // field cannot order the found set.
        let summarized = def
            .field
            .filter(|f| f.occurrence.is_none())
            .ok_or(HostError::Unsupported)?;
        let field = schema
            .field(occurrence, summarized.field)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?
            .name
            .clone();
        Ok(SummaryOrder { function, field })
    }

    /// Stored find requests (script-steps.md 11.2, variables already
    /// expanded by the runtime) as the window's requests.
    fn stored_requests(&mut self, node: &OwnedNode) -> Result<Vec<FindRequest>, HostError> {
        let stored = om_data::specs::requests(node).ok_or(HostError::Error(ErrorCode::INVALID))?;
        let mut requests = Vec::new();
        for request in stored {
            let mut find = FindRequest {
                omit: request.omit,
                criteria: Vec::new(),
            };
            for (field, text) in request.criteria {
                let field = self.spec_field_ref(field);
                let name = self.find_field_name(field)?;
                find.set(&name, text);
            }
            requests.push(find);
        }
        Ok(requests)
    }
}

/// The first of the record ids a script's calculations give the records
/// the window's related drafts are creating (`Uncommitted::related`, by
/// index): above every id a host gives a stored record, below
/// [`NO_RECORD`] (#1014).
const BEGUN_RECORD: u64 = 1 << 62;

/// Whether `record` is one a related draft is creating ([`BEGUN_RECORD`]).
fn is_begun(record: u64) -> bool {
    record != NO_RECORD && record >= BEGUN_RECORD
}

/// `context` with a record a related draft is creating, which no host
/// knows, as no record: what reads that record's identity answers as for
/// none rather than asking for it.
fn without_begun(context: &Context) -> std::borrow::Cow<'_, Context> {
    let begun = context.record.is_some_and(is_begun)
        || context.portal.is_some_and(|(_, record)| is_begun(record));
    if !begun {
        return std::borrow::Cow::Borrowed(context);
    }
    std::borrow::Cow::Owned(Context {
        record: context
            .record
            .map(|record| if is_begun(record) { NO_RECORD } else { record }),
        portal: context.portal.filter(|(_, record)| !is_begun(*record)),
        ..context.clone()
    })
}

impl WorkerHost<'_> {
    /// A provider failure as a step error, its words kept for the alert.
    fn failed(&mut self, error: DataError) -> HostError {
        self.s.failure = Some(error.message.clone());
        data_error(error)
    }
}

/// A failed commit that may have stored records in one of its files:
/// one committed, or whose outcome is not known (#1006).
fn partly_applied(error: &DataError) -> bool {
    use om_data::FileOutcome;
    error.commit.iter().any(|file| {
        matches!(
            file.outcome,
            FileOutcome::Committed | FileOutcome::Unknown | FileOutcome::Unresolved
        )
    })
}

/// `error` with what its commit did in each file appended, as the window's
/// partly saved bar words it (recovery.rs): the alert and
/// `Get ( LastErrorDetail )` name the files a script's commit stored.
fn with_file_outcomes(mut error: DataError) -> DataError {
    use om_data::FileOutcome;
    let files: Vec<String> = error
        .commit
        .iter()
        .map(|file| {
            let name = file.file.rsplit('/').next().unwrap_or(&file.file);
            let name = name.strip_suffix(".fmp12").unwrap_or(name);
            let state = match file.outcome {
                FileOutcome::Committed => "saved",
                FileOutcome::NotSubmitted | FileOutcome::NotApplied => "not saved",
                FileOutcome::Unknown | FileOutcome::Unresolved => "not known whether it was saved",
            };
            format!("{name}: {state}.")
        })
        .collect();
    error.message = format!("{} {}", error.message.trim_end(), files.join(" "));
    error
}

impl WorkerHost<'_> {
    /// A field of another occurrence as find requests name it,
    /// `Occurrence::Field`.
    fn qualified_name(&mut self, field: FieldRef) -> Result<String, HostError> {
        let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
        let occurrence = schema
            .occurrence(field.occurrence)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        let def = schema
            .field(field.occurrence, field.field)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        Ok(format!("{}::{}", occurrence.name, def.name))
    }
}

/// `cells` (a field's id, or `F * 256 + n` for repetition n) over a
/// record's values.
pub(crate) fn apply_cells(
    values: &mut om_data::host::RecordValues,
    table: &om_data::host::TableDef,
    cells: &super::window::Cells,
    cx: &Ctx,
) {
    for (cell, value) in cells {
        let (field, repetition) = match table.fields.get(cell) {
            Some(def) => (def, 1),
            None => match table.fields.get(&(cell >> 8)) {
                Some(def) => (def, (cell & 0xff) as u32),
                None => continue,
            },
        };
        let text = value.clone().unwrap_or_default();
        values.set(
            field.id,
            repetition,
            stored_value(&text, field.value_type, cx),
        );
    }
}

impl WorkerHost<'_> {
    /// Find mode's criterion for a field in the current request, when it
    /// has one: what the field reads there (script-runtime.md 13.21).
    fn criterion(&self, occurrence: u64, field: u64, repetition: u32) -> Option<String> {
        let state = &self.s.file.window.state;
        if state.mode != Mode::Find || repetition > 1 {
            return None;
        }
        let name = match self.schema() {
            Some(schema) => {
                let def = schema.field(occurrence, field)?;
                let own = self
                    .s
                    .file
                    .occurrence
                    .and_then(|o| schema.table_of(o))
                    .map(|t| t.id);
                if schema.table_of(occurrence).map(|t| t.id) == own {
                    def.name.clone()
                } else {
                    format!("{}::{}", schema.occurrence(occurrence)?.name, def.name)
                }
            }
            None => self.s.file.fields.get(&(occurrence, field))?.clone(),
        };
        state
            .find
            .get(state.find_index)?
            .get(&name)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    }
}

/// A provider failure as a step error: its FileMaker code, else -1.
pub(crate) fn data_error(error: DataError) -> HostError {
    HostError::Error(ErrorCode(error.code.unwrap_or(-1)))
}

impl CalcHost for WorkerHost<'_> {
    fn locale(&self) -> &Locale {
        match self.schema() {
            Some(schema) => &schema.locale,
            None => &self.locale,
        }
    }

    fn today(&self) -> Date {
        self.today
    }

    fn names(&self) -> &dyn Names {
        match self.schema() {
            Some(schema) => schema.names(),
            None => &self.s.file.names,
        }
    }

    fn names_for(&self, context: &Context) -> &dyn Names {
        self.schema()
            .and_then(|s| s.context_file(context))
            .map_or_else(|| self.names(), |f| &f.names)
    }

    fn locale_for(&self, context: &Context) -> &Locale {
        self.schema()
            .and_then(|s| s.context_file(context))
            .map_or_else(|| self.locale(), |f| &f.locale)
    }

    fn table_file(&self, occurrence: u64) -> Option<u64> {
        self.schema()?.occurrence_table_file(occurrence)
    }

    fn variable_name<'n>(&self, name: &'n str, context: &Context) -> std::borrow::Cow<'n, str> {
        match self.schema().and_then(|s| s.context_file(context)) {
            Some(file) if VarScope::of(name) == VarScope::Global => {
                std::borrow::Cow::Owned(format!("\u{1f}{}\u{1f}{name}", file.file_name))
            }
            _ => std::borrow::Cow::Borrowed(name),
        }
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        if let Some(schema) = self.current_schema() {
            if let Some(def) = schema.field(occurrence, field) {
                return Some(def.info());
            }
            // A related file not open yet opens now: a script waits for it
            // (#983), and reads its field from the schema that has it.
            if !schema.is_pending(occurrence) {
                return None;
            }
            let mut provider = self.provider.borrow_mut();
            provider
                .calc_data(&[CalcNeed::Occurrence { occurrence }])
                .ok()?;
            let grown = provider.calc_schema().ok().flatten()?;
            return grown.field(occurrence, field).map(|f| f.info());
        }
        self.s
            .file
            .fields
            .contains_key(&(occurrence, field))
            .then(|| FieldInfo::stored(ValueType::Text, 1))
    }

    /// An unstored calculation whose referenced fields are all empty reads
    /// empty unless it evaluates always (calc-values.md 8.8), in a step's
    /// calculations as everywhere else.
    fn evaluate_always(&self, occurrence: u64, field: u64) -> bool {
        self.schema()
            .and_then(|s| s.field(occurrence, field))
            .is_none_or(om_data::host::FieldDef::evaluates_always)
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        if let Some(value) = self.begun_value(occurrence, record, field, repetition) {
            return Ok(value);
        }
        if record == NO_RECORD {
            if let Some(criterion) = self.criterion(occurrence, field, repetition) {
                return Ok(Value::text(criterion));
            }
        }
        if let Some(value) = self.file(|f| f.field_value(occurrence, record, field, repetition)) {
            return value;
        }
        let name = self
            .s
            .file
            .fields
            .get(&(occurrence, field))
            .ok_or(CalcError::FIELD_MISSING)?;
        let state = &self.s.file.window.state;
        if record == NO_RECORD {
            return Ok(Value::empty_text());
        }
        if repetition != 1 || state.record.and_then(|r| u64::try_from(r).ok()) != Some(record) {
            return Err(CalcError::FIELD_MISSING);
        }
        let text = match state.drafts.get(name) {
            Some(draft) => draft.clone().unwrap_or_default(),
            None => self
                .s
                .file
                .record_values
                .get(name)
                .cloned()
                .ok_or(CalcError::FIELD_MISSING)?,
        };
        Ok(Value::text(text))
    }

    fn field_value_in_context(
        &self,
        context: &Context,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        if let Some(value) = self.begun_value(occurrence, record, field, repetition) {
            return Ok(value);
        }
        // Find mode's criterion, as `field_value` reads it.
        if record == NO_RECORD {
            if let Some(criterion) = self.criterion(occurrence, field, repetition) {
                return Ok(Value::text(criterion));
            }
        }
        self.file(|file| {
            file.field_value_in_context(context, occurrence, record, field, repetition)
        })
        .unwrap_or_else(|| self.field_value(occurrence, record, field, repetition))
    }

    fn relationship_origin(&self, context: &Context, related: u64) -> Option<(u64, u64)> {
        self.schema().map_or_else(
            || context.occurrence.zip(context.record),
            |schema| schema.relationship_origin(context, related),
        )
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        // A record a related draft is creating has no related records the
        // host could find, and is never named to it.
        if is_begun(record) {
            return Ok(Vec::new());
        }
        let mut records = self
            .file(|f| f.related_records(occurrence, record, related))
            .unwrap_or_else(|| Err(om_data::host::relationship_missing()))?;
        // Delete Portal Row's records are gone before the commit.
        let current = self
            .s
            .file
            .window
            .state
            .record
            .and_then(|r| u64::try_from(r).ok());
        let table = self
            .schema()
            .and_then(|s| s.table_of(related))
            .map(|t| t.id);
        if let (true, Some(table)) = (
            Some(occurrence) == self.s.file.occurrence && Some(record) == current,
            table,
        ) {
            let open = &self.s.file.window.state.uncommitted;
            records.retain(|r| !open.deletes(table, *r));
        }
        Ok(records)
    }

    fn related_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        index: usize,
    ) -> Result<Option<u64>, CalcError> {
        if is_begun(record) {
            return Ok(None);
        }
        // In the active portal row a related field is that row's record.
        if let Some((row_occurrence, row_record)) = self.s.file.window.row_record {
            let current = self
                .s
                .file
                .window
                .state
                .record
                .and_then(|r| u64::try_from(r).ok());
            if index == 0
                && related == row_occurrence
                && Some(occurrence) == self.s.file.occurrence
                && Some(record) == current
            {
                return Ok(row_record);
            }
        }
        self.related_records(occurrence, record, related)
            .map(|records| records.get(index).copied())
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        self.file(|f| f.found_record(occurrence, index))
            .unwrap_or(Ok(None))
    }

    fn lookup_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        _direction: Option<LookupDirection>,
    ) -> Result<Option<u64>, CalcError> {
        self.related_record(occurrence, record, related, 0)
    }

    fn summary(
        &self,
        context: &Context,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Value, CalcError> {
        let context = without_begun(context);
        self.file(|f| f.summary(&context, summary, break_field))
            .unwrap_or(Err(CalcError::NOT_SUPPORTED))
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        self.schema()?.table_of(occurrence).map(|t| t.name.as_str())
    }

    /// From the schema reads go through, which has a related file opened
    /// while the script runs (`current_schema`, #1123); the name in the
    /// table's own file, as the window's calculations give it.
    fn table_name(&self, occurrence: u64) -> Option<String> {
        let schema = self.current_schema()?;
        let table = schema.table_of(occurrence)?;
        schema.local_table_name(table.id).map(str::to_owned)
    }

    /// As [`Self::table_name`], in the file `names_for` picks.
    fn qualified_field_name(
        &self,
        context: &Context,
        occurrence: u64,
        field: u64,
    ) -> Option<String> {
        let Some(schema) = self.current_schema() else {
            return qualified_field_name(self.names_for(context), occurrence, field);
        };
        let names: &dyn Names = match schema.context_file(context) {
            Some(file) => &file.names,
            None => schema.names(),
        };
        qualified_field_name(names, occurrence, field)
    }

    fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        self.schema()?.custom_function(id)
    }

    /// A container field's data, from the file's library through the
    /// provider (containers.md 6.1).
    fn container_data(
        &mut self,
        container: &om_data::calc::Container,
        _context: &Context,
    ) -> Result<om_data::calc::eval::StoredContainer, CalcError> {
        self.file(|f| om_data::host::library_data(f.source, container))
            .unwrap_or(Err(CalcError::NOT_SUPPORTED))
    }

    fn get(&mut self, selector: u8, context: &Context) -> Result<Value, CalcError> {
        let context = without_begun(context);
        let context = &*context;
        if let Some((record, index, page)) = self.report_record {
            match selector {
                om_data::host::session::selector::PAGE_NUMBER => {
                    return Ok(Value::number(page as i64 + 1))
                }
                RECORD_NUMBER => return Ok(Value::number(index as i64 + 1)),
                RECORD_ID => return Ok(Value::number(record as i64)),
                _ => (),
            }
        }
        if let Some(file) = self.schema().and_then(|s| s.context_file(context)) {
            use om_data::host::session::selector::{FILE_LOCALE_ELEMENTS, FILE_NAME, FILE_PATH};
            if let Some(value) = file.privilege_selector(selector) {
                return Ok(value);
            }
            match selector {
                om_data::host::session::selector::ACCOUNT_TYPE
                | om_data::host::session::selector::ACCOUNT_PASSWORD_DAYS_REMAINING => {
                    let account = file.account_info.clone();
                    let today = self.session_info().evaluation_date();
                    return Ok(account
                        .answer(selector, today)
                        .unwrap_or_else(Value::empty_text));
                }
                FILE_NAME => return Ok(Value::text(&file.file_name)),
                FILE_PATH => {
                    let file_name = file.file_name.clone();
                    let info = self.session_info();
                    let parent = info.file_path.rsplit_once('/').map_or("", |(p, _)| p);
                    return Ok(Value::text(format!("{parent}/{file_name}.fmp12")));
                }
                FILE_LOCALE_ELEMENTS => return Ok(Value::text(file.locale.elements_json(true))),
                _ => {}
            }
        }
        Ok(match selector {
            CURRENT_DATE => Value::Date(self.today),
            // In Find mode the requests and the current one.
            FOUND_COUNT if self.s.file.window.state.mode == Mode::Find => {
                Value::number(self.s.file.window.state.find.len() as i64)
            }
            RECORD_NUMBER if self.s.file.window.state.mode == Mode::Find => {
                Value::number(self.s.file.window.state.find_index as i64 + 1)
            }
            FOUND_COUNT => {
                let (window, provider) = self.wp();
                Value::number(window.found_count(provider).unwrap_or(0) as i64)
            }
            // A portal row's context (a portal's filter, #192) answers its
            // row number, as Browse's `FileCalc::record_number` does.
            RECORD_NUMBER if context.portal_row_number.is_some() => Value::number(
                i64::try_from(context.portal_row_number.unwrap_or(0)).unwrap_or(i64::MAX),
            ),
            RECORD_NUMBER => {
                let (window, provider) = self.wp();
                let found = window.found_count(provider).unwrap_or(0);
                Value::number(if found == 0 {
                    0
                } else {
                    window.state.index as i64 + 1
                })
            }
            // 0 without a record (Find mode, an empty found set).
            RECORD_ID => {
                let (window, provider) = self.wp();
                let record = match window.state.mode {
                    Mode::Find => None,
                    _ => window.record_id(provider).ok().flatten(),
                };
                Value::number(record.unwrap_or(0))
            }
            FILEMAKER_PATH => Value::text(files::application_folder()),
            PREFERENCES_PATH => Value::text(files::preferences_folder()),
            SYSTEM_DRIVE => Value::text(files::system_drive()),
            // FileMaker Pro's cache file is `FMTEMPFM<process id>_<n>.tmp`
            // in the temporary folder; OpenMaker keeps none of its own.
            CACHE_FILE_NAME => Value::text(format!("FMTEMPFM{}_1.tmp", std::process::id())),
            CACHE_FILE_PATH => Value::text(files::filemaker_path(&std::env::temp_dir(), true)),
            DOCUMENTS_PATH_LISTING => {
                Value::text(files::listing(&self.s.file.data_files.folders.documents))
            }
            LAYOUT_NAME => Value::text(self.session_info().layout_name),
            // Empty when the folder does not exist, as in FileMaker Pro's
            // capture copy (calc-functions.md 6.3 O1).
            DESKTOP_PATH => Value::text(files::existing_folder(
                &self.s.file.data_files.folders.desktop,
            )),
            DOCUMENTS_PATH => Value::text(files::existing_folder(
                &self.s.file.data_files.folders.documents,
            )),
            TEMPORARY_PATH => Value::text(files::filemaker_path(
                self.s.file.data_files.temporary_folder(),
                true,
            )),
            OPEN_DATA_FILE_INFO => Value::text(self.s.file.data_files.info()),
            // A report reads its own mode (3 while printing).
            WINDOW_MODE if self.report_session.is_some() => Value::number(
                self.report_session
                    .as_ref()
                    .map_or(0, |info| info.window_mode.number()),
            ),
            WINDOW_MODE => Value::number(match self.s.file.window.state.mode {
                Mode::Browse => 0,
                Mode::Find => 1,
                Mode::Preview => 2,
                Mode::Layout => 4,
            }),
            _ => {
                let mut info = self.session_info();
                if selector == om_data::host::session::selector::RECORD_ACCESS {
                    info.record_access_here = self.record_access_here();
                }
                let answer = self
                    .file(|f| info.get(selector, context, f))
                    .flatten()
                    .or_else(|| {
                        // Session answers that need no file data.
                        let schema = CalcSchema::new("", Locale::en_us());
                        let source = om_data::host::MemorySource::new();
                        info.get(
                            selector,
                            context,
                            &FileCalc::new(&schema, &source, self.today),
                        )
                    });
                return answer.unwrap_or_else(|| Ok(Value::empty_text()));
            }
        })
    }

    fn variables(&mut self, _scope: VarScope) -> &mut Variables {
        &mut self.s.file.variables
    }

    fn call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Value, CalcError> {
        if let HostFunction::Builtin(builtin) = function {
            // With the window's session: its windows and the other open
            // files.
            let info = self.session_info();
            if let Some(result) = self
                .file(|f| f.with_session(&info).design(builtin.name, args, context))
                .flatten()
            {
                return result;
            }
        }
        if let HostFunction::Named(name) = function {
            if name.eq_ignore_ascii_case("GetLayoutObjectAttribute") {
                return Ok(self.layout_object_attribute(args, context));
            }
            // A script's calculation is no layout object's (error 10); a
            // report's layout calculation is its object's.
            if name.eq_ignore_ascii_case("LayoutObjectUUID") {
                return match &self.report_object {
                    Some(uuid) => Ok(Value::text(uuid)),
                    None => Err(CalcError::from_code(10).expect("nonzero")),
                };
            }
            if name.eq_ignore_ascii_case("GetLayoutObjectOwnerInfo") {
                return Ok(Value::text(
                    om_data::host::layout_objects::owner_info_not_found(),
                ));
            }
        }
        default_call(function)
    }

    /// `ExecuteSQL` and `ExecuteSQLe` query the file through the provider,
    /// waiting for what they read as other script calculations do, with
    /// the window's unsaved edits on the current record.
    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> (Value, Option<CalcError>) {
        if let Some(variant) = om_data::sql::calc::Variant::of(&function) {
            let Some(schema) = self.schema().cloned() else {
                return (Value::invalid(), Some(CalcError::NOT_SUPPORTED));
            };
            let info = self.session_info();
            let overlays = self.overlays(&schema);
            let provider = &self.provider;
            let fetch = |needs: &[CalcNeed]| provider.borrow_mut().calc_data(needs).ok();
            let mut source = CachedSource::new(&self.s.file.cache).with_fetch(&fetch);
            for (table, record, values) in overlays {
                source = source.with_overlay(table, record, values);
            }
            if self.report_session.is_some() {
                source = source.with_calculation_session(&info);
            }
            let file = FileCalc::new(&schema, &source, self.today);
            let file = if self.report_session.is_some() {
                file.with_session(&info)
            } else {
                file
            };
            return om_data::host::sql::execute(
                file,
                &info,
                Vars::Shared(&mut self.s.file.variables),
                0,
                variant,
                args,
            );
        }
        match self.call(function, args, context) {
            Ok(v) => (v, None),
            Err(e) => (Value::invalid(), Some(e)),
        }
    }
}

impl ScriptHost for WorkerHost<'_> {
    fn other_step(&mut self, call: &StepCall<'_>) -> HostResult<Option<Value>> {
        match call.id {
            // Not printing: FileMaker Pro 26.0.3 opens Page Setup when the
            // set denies printing (#651's reading).
            42 => self.print_setup(call).map(|()| None),
            43 => {
                self.require_options(true, false)?;
                self.print_records(call).map(|()| None)
            }
            // Saving records as PDF is printing (FileMaker Pro Help,
            // "Editing other privileges").
            144 => {
                self.require_options(true, false)?;
                self.save_records_pdf(call).map(|()| None)
            }
            _ => Err(HostError::Unsupported),
        }
    }
    fn active_script_files(&mut self, files: &[FileId]) {
        self.s.active_files.clear();
        self.s.active_files.extend(files.iter().copied());
    }

    fn script_access(&mut self, file: FileId, script: u64) -> ScriptAccess {
        ScriptAccess {
            executable: self.script_executable(file, script),
            full_access: self
                .script(file, script)
                .is_some_and(|s| s.runs_with_full_access()),
        }
    }

    fn set_full_access(&mut self, file: FileId, on: bool) {
        self.elevated = on;
        self.elevated_file = on.then_some(file);
    }

    fn calc_context(&mut self) -> Context {
        let occurrence = self.s.file.occurrence;
        self.s.file.window.row_record = match self.s.file.window.state.portal {
            Some(row) if row.row > 0 => {
                let record = self.active_row_record(row.occurrence).or_else(|| {
                    // A row begun in the creation row: its draft (#1014).
                    let index = self.active_begun_row(row.occurrence)?;
                    BEGUN_RECORD.checked_add(u64::try_from(index).ok()?)
                });
                Some((row.occurrence, record))
            }
            _ => None,
        };
        let record = {
            let (window, provider) = self.wp();
            if window.state.mode == Mode::Browse {
                window.record_id(provider).ok().flatten()
            } else {
                None
            }
        };
        self.s.file.record_values = {
            let (window, provider) = self.wp();
            match window.current(provider) {
                Ok(Some((fields, record))) => fields
                    .iter()
                    .zip(&record.values)
                    .map(|(f, v)| (f.name.clone(), v.clone().unwrap_or_default()))
                    .collect(),
                _ => HashMap::new(),
            }
        };
        // Without a current record (Find mode, an empty found set) fields
        // read Find mode's criteria, empty values or globals, not 1224
        // (script-runtime.md 13.21).
        match (occurrence, record.and_then(|r| u64::try_from(r).ok())) {
            (Some(occurrence), Some(record)) => Context::record(occurrence, record),
            (Some(occurrence), None) => Context::record(occurrence, NO_RECORD),
            (None, _) => Context::new(),
        }
    }

    fn current_window(&mut self) -> Option<WindowId> {
        Some(u64::from(self.s.file.window.id()))
    }

    fn current_file(&mut self) -> FileId {
        self.s.file.id
    }

    fn script(&mut self, file: FileId, id: u64) -> Option<Arc<Script>> {
        self.lookup_script(file, id)
    }
    fn script_named(&mut self, file: FileId, name: &str) -> Option<u64> {
        self.lookup_script_name(file, name)
    }
    fn file_named(&mut self, name: &str) -> Option<FileId> {
        self.resolve_named_source(self.s.file.id, name).ok()
    }
    fn file_named_from(&mut self, from: FileId, name: &str) -> Option<FileId> {
        self.resolve_named_source(from, name).ok()
    }
    fn resolve_file(&mut self, from: FileId, reference: &OwnedNode) -> HostResult<FileId> {
        self.resolve_database(from, FileSource::Reference(reference))
    }
    fn window_for_file(&mut self, file: FileId) -> HostResult<Option<WindowId>> {
        self.database_window(file)
    }
    fn use_window(&mut self, window: WindowId) -> HostResult {
        self.select_database_window(window)
    }

    fn window_mode(&mut self) -> Mode {
        self.s.file.window.state.mode
    }

    fn exit_layout_mode(&mut self) -> HostResult {
        self.s.file.window.set_mode(Mode::Browse);
        Ok(())
    }

    fn abort_requested(&mut self) -> bool {
        self.pump(None);
        // A lost connection interrupts the session: the run loop ends every
        // script at the end of this time slice even with Allow User Abort
        // off, and until then each step that asks the user or the host is
        // refused.
        let lost = self.connection_lost();
        lost || self.s.abort.swap(false, Ordering::SeqCst) || self.s.interrupted || self.s.halt
    }

    fn take_triggers(&mut self) -> Vec<Trigger> {
        // The layout may have changed: names and fields follow it.
        self.sync_layout(false);
        self.flush();
        std::mem::take(&mut self.triggers)
    }

    fn trigger_action(&mut self, action: &TriggerAction) -> HostResult {
        if action.action & UI_TOKEN != 0 {
            let fired_in = self
                .s
                .firing
                .remove(&action.action)
                .map(|(window, _)| window);
            let record = if fired_in == Some(self.s.file.window.id()) {
                let (window, provider) = self.wp();
                window.record_id(provider).ok()
            } else {
                None
            };
            self.send(ScriptEvent::Action {
                token: action.action,
                proceed: action.proceed,
                record,
            });
            return Ok(());
        }
        let Some(held) = self.s.held.remove(&action.action) else {
            return Ok(());
        };
        self.s.firing.remove(&action.action);
        if !action.proceed {
            return Ok(());
        }
        // An external trigger finished in its own file. Resume the held
        // action in the window that caused it, never the peer's colliding
        // layout/record ids.
        self.select_database_window(u64::from(held.window))?;
        if let Some(saving) = held.saving {
            self.related_field_saved(saving);
        }
        self.run_act_from(held.act, held.stage, Some(held.fired))
    }

    fn error_alert(&mut self, alert: &ErrorAlert) -> AlertChoice {
        if self.denied_quietly(alert.step, alert.code) {
            return AlertChoice::Continue;
        }
        if alert.code == ACCESS_DENIED && alert.step == 98 && self.s.options_refused {
            // Copy All Records/Requests the set denies: FileMaker Pro
            // 26.0.3's privilege alert, with OK only (#651's reading).
            let _ = self.ask(Prompt::Alert {
                message: om_data::PRIVILEGE_DENIED.into(),
                detail: String::new(),
                confirm: false,
                can_cancel: false,
            });
            return AlertChoice::Continue;
        }
        if alert.code == ErrorCode::RECORD_MISSING && alert.step == 16 {
            // Go to Record past the last record is silent.
            return AlertChoice::Continue;
        }
        let detail = self.s.failure.take().unwrap_or_default();
        if alert.script.is_empty()
            && matches!(alert.step, 28 | 126 | 127)
            && alert.code == ErrorCode(code::NO_MATCH)
        {
            // A find from the window that matched nothing (client-ui.md,
            // "Dialogs"): Modify Find returns to Find mode with the
            // requests, Cancel to Browse with the found set as it was.
            let button = |label: &str| DialogButton {
                label: label.into(),
                commit: false,
            };
            let modify = matches!(
                self.ask(Prompt::Dialog(CustomDialog {
                    title: String::new(),
                    message: "No records match this find criteria.".into(),
                    buttons: vec![button("Modify Find"), button("Cancel")],
                    inputs: Vec::new(),
                })),
                Some(Answer::Dialog(DialogResponse { button: 1, .. }))
            );
            let finding = self.s.file.window.state.mode == Mode::Find;
            if modify && !finding {
                // A stored find (a one-step button) from Browse.
                let _ = self.modify_last_find();
            } else if !modify && finding {
                // As Enter Browse Mode: OnModeExit may keep Find mode, and
                // OnModeEnter and OnRecordLoad follow.
                let _ = self.run_act(Act::Mode(Mode::Browse), 0);
            }
            return AlertChoice::Continue;
        }
        if alert.script.is_empty() && alert.step == 150 && alert.code == ErrorCode(code::NO_MATCH) {
            // Quick Find from the toolbar (or a one-step button) that found
            // nothing: FileMaker's own message (query-program.md 6.6).
            let _ = self.ask(Prompt::Alert {
                message: "No records match this find criteria.".into(),
                detail: String::new(),
                confirm: false,
                can_cancel: false,
            });
            return AlertChoice::Continue;
        }
        let message = format!(
            "{}: step {} ({}): {}",
            alert.script, alert.line, alert.step_name, alert.code
        );
        match self.ask(Prompt::Alert {
            message,
            detail,
            confirm: alert.confirm,
            can_cancel: alert.can_cancel,
        }) {
            Some(Answer::Alert(choice)) => choice,
            _ => AlertChoice::Cancel,
        }
    }

    fn pause(&mut self, _seconds: Option<f64>) -> HostResult<PauseEnd> {
        // The session waits for the user between run slices.
        Err(HostError::Pending)
    }

    fn install_on_timer(&mut self, timer: Option<OnTimer>) -> HostResult {
        let interval = timer.as_ref().map(|t| t.interval);
        self.s.set_timer(timer);
        self.send(ScriptEvent::Timer(interval));
        Ok(())
    }

    fn plugin_state(&mut self) -> Option<&mut om_data::runtime::plugins::PluginState> {
        Some(&mut self.s.plugin)
    }

    /// `FM.RunScriptIdle`: this window's session keeps the script, which
    /// names the window that scheduled it; the UI keeps the clock and
    /// starts it there (`Start::Idle`).
    fn schedule_idle_script(
        &mut self,
        idle: Option<om_data::runtime::plugins::IdleScript>,
    ) -> HostResult {
        let delay = idle.as_ref().map(|i| i.delay);
        self.s.file.idle = idle;
        self.send(ScriptEvent::Idle(delay));
        Ok(())
    }
}

impl Navigation for WorkerHost<'_> {
    fn go_to_layout(&mut self, layout: &LayoutTarget, _animation: Option<u32>) -> HostResult {
        let layouts = self.layouts();
        let id = match layout {
            LayoutTarget::Key(id) => layouts
                .iter()
                .find(|l| u64::from(l.id) == *id)
                .map(|l| l.id),
            LayoutTarget::Name(name) => layouts
                .iter()
                .find(|l| l.name == *name)
                .or_else(|| layouts.iter().find(|l| l.name.eq_ignore_ascii_case(name)))
                .map(|l| l.id),
            LayoutTarget::Number(n) => usize::try_from(*n - 1)
                .ok()
                .and_then(|i| layouts.get(i))
                .map(|l| l.id),
            LayoutTarget::Original => self.s.file.window.original_layout,
        }
        .ok_or(HostError::Error(ErrorCode::LAYOUT_MISSING))?;
        // A layout the account may not see is entered, as FileMaker Pro
        // enters it, and shows `<No Access>`: the provider reads nothing
        // through it (#137's reading).
        if Some(id) == self.s.file.window.state.layout {
            // Going there names it no more, as any way to a layout but Go
            // to Related Record (#696).
            self.s.file.window.state.hidden_named = false;
            return Ok(());
        }
        self.run_act(Act::Layout(id), 0)
    }

    fn go_to_record(&mut self, movement: RecordMove, _with_dialog: bool) -> HostResult<bool> {
        let (current, total) = if self.s.file.window.state.mode == Mode::Find {
            let state = &self.s.file.window.state;
            (state.find_index as u64, state.find.len() as u64)
        } else if self.s.file.window.state.mode == Mode::Preview {
            let layout = self
                .s
                .file
                .window
                .state
                .layout
                .ok_or(ErrorCode::RECORD_MISSING)?;
            let document = self.render(layout).ok_or(HostError::Error(ErrorCode(3)))?;
            let paper = self
                .report_paper(&document.layout)
                .map_err(|_| HostError::Error(ErrorCode(3)))?;
            let (window, provider) = self.wp();
            let count = window.found_count(provider).map_err(data_error)?;
            if count == 0 {
                return Err(ErrorCode::RECORD_MISSING.into());
            }
            // The same sub-summary groups and measured part heights as
            // Preview and output, when the layout has any.
            let grouped = !om_layout::pagination::report_breaks(&document.layout)
                .is_ok_and(|fields| fields.is_empty());
            let resizes = crate::report::resizes_parts(&document.layout);
            let mut found: Vec<i64> = Vec::new();
            let mut source = None;
            if grouped || resizes {
                let occurrence = self.occurrence().ok_or(ErrorCode::RECORD_MISSING)?;
                let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
                let (window, provider) = self.wp();
                found = window.found_ids(provider).map_err(data_error)?;
                source = Some((occurrence, schema));
            }
            let breaks = match &source {
                Some((occurrence, schema)) if grouped => {
                    let ids: Vec<u64> = found.iter().map(|id| *id as u64).collect();
                    self.report_groups(&document.layout, *occurrence, schema, &ids)
                        .map_err(|_| HostError::Error(ErrorCode(3)))?
                }
                _ => Vec::new(),
            };
            let records = usize::try_from(count).map_err(|_| HostError::Error(ErrorCode(3)))?;
            if resizes && found.len() != records {
                return Err(HostError::Error(ErrorCode(3)));
            }
            self.report_session = Some(self.session_info());
            let panels = (self.s.file.window.state.panels.iter())
                .filter(|((layout, _), _)| *layout == document.layout.id)
                .map(|((_, control), panel)| (*control, *panel))
                .collect();
            let plan = match &source {
                Some((occurrence, schema)) => crate::report::plan_report_with(
                    &document.layout,
                    records,
                    paper,
                    &breaks,
                    &mut pdf::HostContent {
                        panels,
                        host: self,
                        found: &found,
                        selected: &found,
                        occurrence: *occurrence,
                        schema,
                    },
                ),
                // Without records only a report of no record content plans.
                None => {
                    crate::report::plan_report(&document.layout, records, paper, &breaks, |_, _| {
                        Err(crate::report::ReportError::unsupported("No report records"))
                    })
                }
            };
            self.report_record = None;
            self.report_session = None;
            let plan = plan.map_err(|_| HostError::Error(ErrorCode(3)))?;
            (
                self.s
                    .file
                    .window
                    .state
                    .preview_page
                    .min(plan.pages.len().saturating_sub(1) as u64),
                plan.pages.len() as u64,
            )
        } else {
            let (window, provider) = self.wp();
            let total = window.found_count(provider).map_err(data_error)?;
            (window.state.index, total)
        };
        if total == 0 {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        let last = total - 1;
        let index = match movement {
            RecordMove::First => 0,
            RecordMove::Last => last,
            RecordMove::Previous if current == 0 => return Ok(false),
            RecordMove::Previous => current - 1,
            RecordMove::Next if current >= last => return Ok(false),
            RecordMove::Next => current + 1,
            RecordMove::Number(n) if n < 1 => return Err(ErrorCode::RECORD_MISSING.into()),
            RecordMove::Number(n) => (n as u64 - 1).min(last),
        };
        if self.s.file.window.state.mode == Mode::Preview {
            self.s.file.window.state.preview_page = index;
            self.s.file.window.touch();
            return Ok(true);
        }
        if self.s.file.window.state.mode == Mode::Find {
            self.s.file.window.go_to(index);
            return Ok(true);
        }
        self.run_act(Act::Record(index), 0)?;
        Ok(true)
    }

    fn go_to_field(&mut self, field: Option<FieldRef>, select: bool) -> HostResult {
        if let Some(field) = field.filter(|f| self.is_related(f.occurrence)) {
            // A field shown in a portal: its row (portals.rs).
            return self.go_to_portal_field(field, select);
        }
        let name = match field {
            Some(field) => {
                let name = self.field_name(field)?;
                if self.field_object(&name).is_none() && !self.tab_order().is_empty() {
                    // Not on the layout.
                    return Err(ErrorCode::FIELD_MISSING.into());
                }
                Some(name)
            }
            None => None,
        };
        self.s.file.window.state.select = select && name.is_some();
        self.run_act(Act::Focus(name), 0)
    }

    fn go_to_next_field(&mut self, forward: bool) -> HostResult {
        let order = self.tab_order();
        if order.is_empty() {
            return Ok(());
        }
        let current = self
            .s
            .file
            .window
            .state
            .focus
            .as_ref()
            .and_then(|f| order.iter().position(|o| o == f));
        let next = match (current, forward) {
            (None, true) => 0,
            (None, false) => order.len() - 1,
            (Some(i), true) => (i + 1) % order.len(),
            (Some(i), false) => (i + order.len() - 1) % order.len(),
        };
        self.run_act(Act::Focus(Some(order[next].clone())), 0)
    }

    fn go_to_portal_row(
        &mut self,
        movement: RecordMove,
        select: bool,
        _with_dialog: bool,
    ) -> HostResult<bool> {
        self.go_to_row(movement, select)
    }

    fn go_to_object(&mut self, name: &str, repetition: u32) -> HostResult {
        self.go_to_named_object(name, repetition)
    }

    fn go_to_related_record(&mut self, request: &RelatedRecords<'_>) -> HostResult {
        self.go_to_related(request)
    }

    fn go_to_list_of_records(&mut self, request: &RecordList) -> HostResult {
        if let Some(window) = &request.new_window {
            let style = self.new_window_style(window.window_options(), window.bounds)?;
            self.open_window(window.name.clone(), style)?;
            // Its size and place, as New Window and Go to Related Record
            // give theirs.
            self.move_resize_window(&WindowTarget::Current, window.bounds, false)?;
        }
        if let Some(layout) = &request.layout {
            self.go_to_layout(layout, request.animation)?;
        }
        let ids: Vec<i64> = request
            .ids
            .split(['\r', '\n'])
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        self.change(FoundSetChange::Records(ids))
    }

    fn enter_mode(&mut self, mode: Mode, _pause: bool, requests: Option<&OwnedNode>) -> HostResult {
        if mode == Mode::Layout {
            return Err(HostError::Unsupported);
        }
        let restored = requests
            .map(|node| self.stored_requests(node))
            .transpose()?;
        if mode != self.s.file.window.state.mode {
            self.run_act(Act::Mode(mode), 0)?;
        }
        if mode == Mode::Find {
            self.s.file.window.state.find = restored
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| vec![FindRequest::default()]);
            self.s.file.window.state.find_index = 0;
            self.s.file.window.touch();
        }
        Ok(())
    }

    fn close_popover(&mut self) -> HostResult {
        let Some(layout) = self.s.file.window.state.layout else {
            return Ok(());
        };
        let Some(&popover) = self.s.file.window.state.popovers.get(&u64::from(layout)) else {
            return Ok(());
        };
        let button = self.render(layout).and_then(|render| {
            render
                .layout
                .objects
                .get(popover)?
                .popover
                .as_ref()
                .map(|p| p.button_key)
        });
        let field = self.focused_object();
        // As in the UI's field exit lifecycle, another field's draft
        // does not validate/save the clean field that has the focus.
        let modified = self.s.file.window.state.mode == Mode::Browse
            && self.s.file.window.state.focus.as_ref().map_or_else(
                || self.s.file.window.is_modified(),
                |name| self.s.file.window.state.drafts.contains_key(name),
            );
        self.run_act(
            Act::ClosePopover {
                window: self.s.file.window.id(),
                layout,
                popover,
                button,
                field,
                modified,
            },
            0,
        )
    }
}

impl Records for WorkerHost<'_> {
    fn new_record(&mut self) -> HostResult {
        if self.s.file.window.state.mode == Mode::Find {
            let state = &mut self.s.file.window.state;
            state.find.push(FindRequest::default());
            state.find_index = state.find.len() - 1;
            self.s.file.window.touch();
            return Ok(());
        }
        self.run_act(Act::NewRecord, 0)
    }

    fn duplicate_record(&mut self) -> HostResult {
        if self.s.file.window.state.mode == Mode::Find {
            let state = &mut self.s.file.window.state;
            let request = state
                .find
                .get(state.find_index)
                .cloned()
                .unwrap_or_default();
            state.find.push(request);
            state.find_index = state.find.len() - 1;
            self.s.file.window.touch();
            return Ok(());
        }
        self.run_act(Act::Duplicate, 0)
    }

    fn delete_record(&mut self, with_dialog: bool) -> HostResult {
        if self.s.file.window.state.mode == Mode::Find {
            let state = &mut self.s.file.window.state;
            if state.find.len() > 1 {
                state.find.remove(state.find_index);
                state.find_index = state.find_index.min(state.find.len() - 1);
            } else {
                state.find = vec![FindRequest::default()];
            }
            self.s.file.window.touch();
            return Ok(());
        }
        if with_dialog {
            // FileMaker Pro's alert has no title of its own.
            self.confirm("", crate::app::DELETE_RECORD_MESSAGE.into(), "Delete")?;
        }
        let deleted = self.deleted_entry();
        let (window, provider) = self.wp();
        window.delete(provider).map_err(data_error)?;
        *self.s.file.cache.borrow_mut() = CalcData::default();
        self.committed_records(deleted.into_iter().collect());
        self.after(TriggerEvent::RecordLoad, None);
        Ok(())
    }

    fn delete_portal_row(&mut self, with_dialog: bool) -> HostResult {
        self.delete_row(with_dialog)
    }

    fn delete_all_records(&mut self, with_dialog: bool) -> HostResult {
        let ids = {
            let (window, provider) = self.wp();
            window.found_ids(provider).map_err(data_error)?
        };
        if with_dialog {
            self.confirm(
                "Delete All Records",
                format!(
                    "Permanently delete ALL {} record(s) in the current found set?",
                    ids.len()
                ),
                "Delete All",
            )?;
        }
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        let table = self.s.file.window.table;
        let mut deleted = Vec::new();
        for id in ids {
            let content = table
                .zip(u64::try_from(id).ok())
                .map(|(table, record)| self.transaction_content(table, record))
                .unwrap_or_default();
            if let Err(e) = self.p().delete_record(layout, id) {
                self.committed_records(deleted);
                self.s.file.window.changed_data();
                return Err(data_error(e));
            }
            deleted.extend(table.map(|table| transaction::Entry {
                table,
                record: id,
                operation: transaction::Operation::Deleted,
                content,
            }));
        }
        self.committed_records(deleted);
        self.s.file.window.changed_data();
        self.s.file.window.found = None;
        self.s.file.window.state.index = 0;
        *self.s.file.cache.borrow_mut() = CalcData::default();
        Ok(())
    }

    fn revert_record(&mut self, with_dialog: bool) -> HostResult {
        if self.s.file.window.state.mode == Mode::Find {
            let state = &self.s.file.window.state;
            let typed = state
                .find
                .get(state.find_index)
                .is_some_and(|r| r.criteria.iter().any(|(_, text)| !text.is_empty()));
            if with_dialog && typed {
                // Pro's Revert Request... alert, as Revert Record's (#871).
                self.confirm_with(
                    "",
                    om_data::scripts::revert_record::REQUEST_MESSAGE.into(),
                    "Revert",
                    false,
                )?;
            }
            let state = &mut self.s.file.window.state;
            if let Some(request) = state.find.get_mut(state.find_index) {
                *request = FindRequest::default();
            }
            self.s.file.window.touch();
            return Ok(());
        }
        // A new record reverts even unchanged: it goes (6.3 O3).
        if !self.s.file.window.is_modified() && !self.s.file.window.is_new() {
            return Ok(());
        }
        if with_dialog {
            // FileMaker Pro 26's alert: no title, Cancel then Revert, and
            // neither the default (#871, observed).
            self.confirm_with(
                "",
                om_data::scripts::revert_record::MESSAGE.into(),
                "Revert",
                false,
            )?;
        }
        self.run_act(Act::Revert, 0)
    }

    fn commit_records(&mut self, options: CommitOptions) -> HostResult {
        // "With dialog" asks only on a layout that does not save record
        // changes automatically (script-runtime.md, desktop notes). That
        // is layout flag 0x10 (layout-model.md 4.2), which OpenMaker does
        // not read yet (#434), so every layout saves automatically and
        // `with_dialog` asks nothing here.
        if self.s.file.window.state.mode != Mode::Browse {
            return Ok(());
        }
        self.run_act(Act::Commit(options.skip_validation), 0)
    }

    fn open_record(&mut self) -> HostResult {
        // Records are locked when they are committed (write-path.md).
        let (window, provider) = self.wp();
        match window.current(provider).map_err(data_error)? {
            Some(_) => Ok(()),
            None => Err(ErrorCode::RECORD_MISSING.into()),
        }
    }

    fn copy_records(&mut self, all: bool) -> HostResult {
        // Copying all records of the found set is exporting (FileMaker Pro
        // Help, "Editing other privileges"); one record is not. Denied, it
        // leaves the clipboard and fails with 200 after FileMaker's
        // privilege alert (`error_alert`, #651's reading).
        if all {
            self.s.options_refused = !self.options_allowed(false, true);
            if self.s.options_refused {
                return Err(ACCESS_DENIED.into());
            }
        }
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        let page = if all {
            let found = {
                let (window, provider) = self.wp();
                window.found_count(provider).map_err(data_error)?
            };
            self.p()
                .records(layout, 0, u32::try_from(found).unwrap_or(u32::MAX))
                .map_err(data_error)?
        } else {
            let index = self.s.file.window.state.index;
            self.p().records(layout, index, 1).map_err(data_error)?
        };
        let text: Vec<String> = page
            .records
            .iter()
            .map(|r| {
                r.values
                    .iter()
                    .map(|v| v.clone().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect();
        let text = text.join("\r");
        self.s.clipboard = Some(text.clone());
        self.send(ScriptEvent::Copy(text));
        Ok(())
    }

    fn begin_transaction(&mut self, options: TransactionOptions) -> HostResult {
        // Open Transaction exits the record first (script-runtime.md 8.1).
        self.commit_window()?;
        self.p()
            .begin_transaction(options)
            .map_err(|e| self.failed(e))?;
        self.s.file.transaction = Some(Vec::new());
        Ok(())
    }

    fn commit_transaction(&mut self, skip_validation: bool) -> HostResult {
        // The active record joins the transaction; neither it nor the
        // commit fires OnRecordCommit (8.2). A record held apart from it
        // is committed apart, and keeps its typing if the transaction
        // fails (#1103).
        let apart = self.record_apart();
        let drafts = self.s.file.window.state.drafts.clone();
        let committed = self.commit_window();
        // A commit across files that failed after a file was committed, or
        // may have been (#1006).
        let mut partly = false;
        let result = match committed {
            Ok(()) => self.p().commit_transaction(skip_validation).map_err(|e| {
                partly = partly_applied(&e);
                let e = if partly { with_file_outcomes(e) } else { e };
                self.failed(e)
            }),
            Err(e) => {
                let _ = self.p().revert_transaction();
                Err(e)
            }
        };
        // A failed commit aborts the transaction (script-runtime.md 8.2),
        // unless the connection went first.
        let mut rolled_back = false;
        if result.is_err() {
            if self.p().connection_lost().is_some() {
                // Whether the host applied the commit is unknown: the
                // record's edits stay in the window, where the user can
                // still read them, and a reconnect is refused.
                self.s.file.window.state.drafts = drafts;
                self.s.file.lost_transaction = true;
            } else if partly {
                // Some of it is stored: what it created is not all gone,
                // and the window keeps what was typed, as the error's
                // report names each file's outcome.
                self.s.file.window.state.drafts = drafts;
            } else {
                if !apart {
                    self.s.file.window.state.drafts.clear();
                }
                rolled_back = true;
            }
        }
        self.s.file.window.state.focus = None;
        self.s.file.window.state.object = None;
        self.s.file.window.state.portal = None;
        self.s.file.window.changed_data();
        self.s.file.window.found = None;
        *self.s.file.cache.borrow_mut() = CalcData::default();
        // OnWindowTransaction names every record the transaction wrote.
        let entries = self.s.file.transaction.take().unwrap_or_default();
        if result.is_ok() {
            let entries = transaction::merged(entries)
                .into_iter()
                .map(|mut e| {
                    if e.operation != transaction::Operation::Deleted {
                        if let Ok(record) = u64::try_from(e.record) {
                            e.content = self.transaction_content(e.table, record);
                        }
                    }
                    e
                })
                .collect();
            self.committed_records(entries);
        } else if rolled_back {
            self.release_records(&entries, transaction::Operation::New);
        }
        result
    }

    fn revert_transaction(&mut self) -> HostResult {
        // The active record is exited without OnRecordRevert (8.3), unless
        // the window holds it apart from the transaction: its typing stays
        // (#1103).
        if !self.record_apart() {
            self.s.file.window.state.drafts.clear();
            self.s.file.window.state.focus = None;
            self.s.file.window.state.object = None;
            self.s.file.window.state.portal = None;
        }
        let held = self.s.file.transaction.take().unwrap_or_default();
        let result = self.p().revert_transaction().map_err(|e| self.failed(e));
        if result.is_ok() {
            // The records the transaction created are gone.
            self.release_records(&held, transaction::Operation::New);
        }
        // The held writes went with the connection, not by this revert.
        self.s.file.lost_transaction |= self.p().connection_lost().is_some();
        self.s.file.window.changed_data();
        self.s.file.window.found = None;
        *self.s.file.cache.borrow_mut() = CalcData::default();
        result
    }

    fn truncate_table(&mut self, table: Option<u64>, with_dialog: bool) -> HostResult {
        let current = self.s.file.window.table;
        if table.is_some() && table != current {
            return Err(HostError::Unsupported);
        }
        if with_dialog {
            self.confirm(
                "Truncate Table",
                "Permanently delete every record of this table?".into(),
                "Truncate",
            )?;
        }
        self.change(FoundSetChange::ShowAll)?;
        self.delete_all_records(false)
    }
}

impl Fields for WorkerHost<'_> {
    fn insert_file(&mut self, request: &InsertFile) -> HostResult<Value> {
        self.insert_file_step(request)
    }

    fn export_field_contents(
        &mut self,
        field: Option<FieldRef>,
        paths: &[String],
        create_folders: bool,
    ) -> HostResult {
        // Not exporting: FileMaker Pro 26.0.3 saves a field's contents when
        // the set denies exporting, from the menu and the step (#651).
        self.export_contents(field, paths, create_folders)
    }

    fn set_field(&mut self, field: FieldRef, value: Value) -> HostResult {
        self.write_field(field, &value)?;
        // A step's value is no longer being typed (6.3 O2).
        if let Ok(name) = self.field_name(field) {
            let state = &mut self.s.file.window.state;
            if state.typing.as_deref() == Some(name.as_str()) {
                state.typing = None;
            }
        }
        Ok(())
    }

    fn set_field_by_name(&mut self, name: &str, value: Value) -> HostResult {
        let Some(schema) = self.schema().cloned() else {
            let field = name.rsplit_once("::").map_or(name, |(_, f)| f).to_owned();
            let text = self.text(&value);
            return self.write_named(&field, text);
        };
        // `Occurrence::Field`, or a field of the layout's occurrence.
        let (occurrence, field) = match name.rsplit_once("::") {
            Some((occurrence, field)) => (
                schema
                    .occurrences()
                    .find(|o| o.name == occurrence)
                    .map(|o| o.id)
                    // One of a related file not open yet, opened below
                    // (#1149).
                    .or_else(|| {
                        schema
                            .external_occurrences()
                            .find(|o| o.name == occurrence)
                            .map(|o| o.id)
                    })
                    .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?,
                field,
            ),
            None => (
                self.s
                    .file
                    .occurrence
                    .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?,
                name,
            ),
        };
        let schema = self.open_pending(&schema, occurrence)?;
        let id = schema
            .table_of(occurrence)
            .and_then(|t| t.fields.values().find(|f| f.name == field))
            .map(|f| f.id)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        self.write_field(
            FieldRef {
                occurrence,
                field: id,
                repetition: 1,
            },
            &value,
        )
    }

    fn field_value_of(&mut self, field: FieldRef) -> HostResult<Value> {
        let context = self.calc_context();
        if let Some(record) = context.record {
            if let Some(value) = self
                .file(|f| f.field_value(field.occurrence, record, field.field, field.repetition))
            {
                return value.map_err(|_| HostError::Error(ErrorCode::FIELD_MISSING));
            }
        }
        let name = self.field_name(field)?;
        self.value_of(&name)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))
    }

    /// Insert Text and the other insert steps: into the focused field the
    /// text replaces its selection, as typing does, and the insertion
    /// point follows it; the field is being typed into until it is left.
    fn insert(&mut self, target: InsertTarget, value: Value) -> HostResult {
        let name = self.target_name(target.field)?;
        if matches!(value, Value::Container(_))
            && self.s.file.window.state.mode == Mode::Browse
            && self.is_container_field(&name)
        {
            // Container data replaces the field's contents, as Set Field
            // puts it into a container field.
            return self.set_container(&name, value);
        }
        // Plain-text insertion cannot safely concatenate a library
        // reference. Typed container insertion above remains supported.
        if self.s.file.window.state.mode == Mode::Browse && self.is_container_field(&name) {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        let inserted = self.text(&value);
        let focused = self.s.file.window.state.focus.as_deref() == Some(name.as_str());
        let current = {
            let (window, provider) = self.wp();
            window
                .value(provider, &name)
                .map_err(data_error)?
                .unwrap_or_default()
        };
        let state = &self.s.file.window.state;
        let chars: Vec<char> = current.chars().collect();
        let (start, size) = match (target.select_all, focused) {
            (true, _) => (0, chars.len()),
            (false, true) => match state.selection {
                Some((start, size)) => {
                    let start = (start as usize).saturating_sub(1).min(chars.len());
                    (start, (size as usize).min(chars.len() - start))
                }
                None if state.select => (0, chars.len()),
                None => (chars.len(), 0),
            },
            (false, false) => (chars.len(), 0),
        };
        let mut text: String = chars[..start].iter().collect();
        text.push_str(&inserted);
        text.extend(&chars[start + size..]);
        let caret = start + inserted.chars().count();
        let at_end = start + size == chars.len();
        self.write_named(&name, text)?;
        if focused {
            let state = &mut self.s.file.window.state;
            state.typing = Some(name);
            state.select = false;
            state.selection = (!at_end).then_some((caret as u64 + 1, 0));
        }
        Ok(())
    }

    fn insert_from(&mut self, target: InsertTarget, last_visited: bool) -> HostResult {
        let name = self.target_name(target.field)?;
        if self.s.file.window.state.mode == Mode::Browse && self.is_container_field(&name) {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        if !last_visited {
            return self.insert_from_index(target);
        }
        let value = self
            .s
            .file
            .window
            .last_visited(&name)
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?
            .to_owned();
        self.write_named(&name, value)
    }

    fn set_next_serial_value(&mut self, field: FieldRef, value: Value) -> HostResult {
        let text = self.text(&value);
        // Any table's field: the occurrence names it.
        let (occurrence, id) = match self.schema().cloned() {
            Some(schema) => {
                let def = schema
                    .field(field.occurrence, field.field)
                    .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
                (field.occurrence, def.id)
            }
            None => (field.occurrence, field.field),
        };
        self.p()
            .set_next_serial_value(occurrence, id, &text)
            .map_err(|e| self.failed(e))
    }

    fn clipboard(&mut self, op: Clipboard, target: InsertTarget) -> HostResult {
        let name = self.target_name(target.field)?;
        // The text clipboard carries raw field strings, not owned
        // container values. Refuse transfers until it preserves those
        // values and acquires a reference for every pasted target.
        if op != Clipboard::Clear
            && self.s.file.window.state.mode == Mode::Browse
            && self.is_container_field(&name)
        {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        let current = {
            let (window, provider) = self.wp();
            window
                .value(provider, &name)
                .map_err(data_error)?
                .unwrap_or_default()
        };
        match op {
            Clipboard::Copy | Clipboard::Cut => {
                self.s.clipboard = Some(current.clone());
                self.send(ScriptEvent::Copy(current));
                if op == Clipboard::Cut {
                    self.write_named(&name, String::new())?;
                }
                Ok(())
            }
            Clipboard::Paste { .. } => {
                let pasted = self
                    .s
                    .clipboard
                    .clone()
                    .ok_or(HostError::Error(ErrorCode::UNAVAILABLE))?;
                let text = if target.select_all {
                    pasted
                } else {
                    current + &pasted
                };
                self.write_named(&name, text)
            }
            Clipboard::Clear => self.write_named(&name, String::new()),
        }
    }

    fn select_all(&mut self) -> HostResult {
        self.s.file.window.state.select = true;
        self.s.file.window.touch();
        Ok(())
    }

    /// Set Selection: characters `start` to `end` (1-based, inclusive) of
    /// the field, which gets the focus; an end before the start puts the
    /// insertion point at the start.
    fn set_selection(&mut self, field: Option<FieldRef>, start: i64, end: i64) -> HostResult {
        if let Some(field) = field {
            let name = self.field_name(field)?;
            self.run_act(Act::Focus(Some(name)), 0)?;
        }
        let state = &mut self.s.file.window.state;
        if state.focus.is_some() {
            let start = start.max(1);
            state.select = false;
            state.selection = Some((start as u64, (end - start + 1).max(0) as u64));
            self.s.file.window.touch();
        }
        Ok(())
    }

    fn replace_field_contents(
        &mut self,
        field: Option<FieldRef>,
        with: &mut Replacement<'_>,
        with_dialog: bool,
    ) -> HostResult {
        let name = self.target_name(field)?;
        // Every target would need an owned typed value and its own +1.
        // Refuse before committing or changing any record in the set.
        if self.s.file.window.state.mode == Mode::Browse && self.is_container_field(&name) {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        self.commit_window()?;
        let found = {
            let (window, provider) = self.wp();
            window.found_count(provider).map_err(data_error)?
        };
        if with_dialog {
            self.confirm(
                "Replace Field Contents",
                format!(
                    "Permanently replace the contents of the field \u{201c}{name}\u{201d} in all {found} records of the current found set?"
                ),
                "Replace",
            )?;
        }
        let original = self.s.file.window.state.index;
        let current_value = {
            let (window, provider) = self.wp();
            window
                .value(provider, &name)
                .map_err(data_error)?
                .unwrap_or_default()
        };
        for index in 0..found {
            self.s.file.window.go_to(index);
            let text = match with {
                Replacement::CurrentContents => current_value.clone(),
                Replacement::Serial {
                    initial, increment, ..
                } => serial_text(initial, *increment, index),
                Replacement::Calculation(evaluate) => {
                    let value = evaluate(self);
                    self.text(&value)
                }
            };
            self.write_named(&name, text)?;
            self.commit_window()?;
        }
        self.s
            .file
            .window
            .go_to(original.min(found.saturating_sub(1)));
        self.s.file.window.changed_data();
        Ok(())
    }
}

impl WorkerHost<'_> {
    /// Insert from Index: the View Index dialog lists the field's index
    /// values; the chosen one goes in at the insertion point (the end of
    /// the field) or replaces the contents with "Select entire contents".
    /// Cancel is error 1.
    fn insert_from_index(&mut self, target: InsertTarget) -> HostResult {
        let name = self.target_name(target.field)?;
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        let values = self
            .p()
            .index_values(layout, &name)
            .map_err(|e| self.failed(e))?;
        let chosen = match self.ask(Prompt::Index {
            field: name.clone(),
            values,
        }) {
            Some(Answer::Index(Some(value))) => value,
            _ => return Err(ErrorCode::CANCELED.into()),
        };
        let text = if target.select_all {
            chosen
        } else {
            let (window, provider) = self.wp();
            let current = window
                .value(provider, &name)
                .map_err(data_error)?
                .unwrap_or_default();
            current + &chosen
        };
        self.write_named(&name, text)
    }
}

/// The `index`th serial number from `initial`: its last run of digits
/// counts up, the rest stays ("INV0009" + 1 = "INV0010").
fn serial_text(initial: &str, increment: i64, index: u64) -> String {
    let end = initial
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_ascii_digit())
        .map(|(i, c)| i + c.len_utf8());
    let Some(end) = end else {
        return initial.to_owned();
    };
    let start = initial[..end]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()
        .map_or(end, |(i, _)| i);
    let digits = &initial[start..end];
    let number: i128 = digits.parse().unwrap_or(0) + i128::from(increment) * index as i128;
    format!(
        "{}{:0width$}{}",
        &initial[..start],
        number,
        &initial[end..],
        width = digits.len()
    )
}

impl FoundSets for WorkerHost<'_> {
    fn show_all_records(&mut self) -> HostResult {
        self.commit_window()?;
        self.s.file.window.set_mode(Mode::Browse);
        self.change(FoundSetChange::ShowAll)?;
        self.after(TriggerEvent::RecordLoad, None);
        Ok(())
    }

    fn show_omitted_only(&mut self) -> HostResult {
        self.commit_window()?;
        self.s.file.window.set_mode(Mode::Browse);
        self.change(FoundSetChange::ShowOmitted)?;
        self.after(TriggerEvent::RecordLoad, None);
        Ok(())
    }

    fn omit_records(&mut self, count: i64, with_dialog: bool) -> HostResult {
        if self.s.file.window.state.mode == Mode::Find {
            let state = &mut self.s.file.window.state;
            if let Some(request) = state.find.get_mut(state.find_index) {
                request.omit = !request.omit;
            }
            self.s.file.window.touch();
            return Ok(());
        }
        let count = if with_dialog {
            self.ask_omit_count(count)?
        } else {
            count
        };
        self.commit_window()?;
        let (window, provider) = self.wp();
        let ids = window.found_ids(provider).map_err(data_error)?;
        let start = window.state.index as usize;
        let omit: Vec<i64> = ids
            .iter()
            .skip(start)
            .take(count.max(1) as usize)
            .copied()
            .collect();
        if omit.is_empty() {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        self.change(FoundSetChange::Omit(omit))
    }

    fn unsort_records(&mut self) -> HostResult {
        // Changing the found set leaves the record: commit it first, as the
        // other found-set steps do, or its uncommitted edits are dropped.
        self.commit_window()?;
        self.change(FoundSetChange::Unsort)
    }

    fn modify_last_find(&mut self) -> HostResult {
        self.commit_window()?;
        let last = self.s.file.window.last_find.clone();
        self.s.file.window.set_mode(Mode::Find);
        if !last.is_empty() {
            self.s.file.window.state.find = last;
            self.s.file.window.state.find_index = 0;
        }
        self.after(TriggerEvent::ModeEnter, None);
        Ok(())
    }

    fn perform_find(&mut self, kind: FindKind, requests: Option<&OwnedNode>) -> HostResult {
        self.find_with(kind, requests, false)
    }

    fn perform_find_command(&mut self, kind: FindKind, requests: Option<&OwnedNode>) -> HostResult {
        self.find_with(kind, requests, true)
    }

    fn perform_quick_find(&mut self, text: &str) -> HostResult {
        // A layout with Quick Find turned off searches no field: nothing
        // is found (query-program.md 6.6).
        let fields = self.quick_find_fields().unwrap_or_default();
        let today = om_data::found::today(om_provider::local_utc_offset());
        let change = om_data::quick_find::change(text, &fields, today).map_err(data_error)?;
        self.commit_window()?;
        self.change(change)?;
        self.after(TriggerEvent::RecordLoad, None);
        Ok(())
    }

    fn find_matching(&mut self, field: Option<FieldRef>, kind: FindKind) -> HostResult {
        let name = self.target_name(field)?;
        let value = {
            let (window, provider) = self.wp();
            window
                .value(provider, &name)
                .map_err(data_error)?
                .unwrap_or_default()
        };
        let mut request = FindRequest::default();
        request.set(
            &name,
            if value.is_empty() {
                "=".into()
            } else {
                format!("=={value}")
            },
        );
        self.commit_window()?;
        self.change(FoundSetChange::Find {
            requests: vec![request],
            kind,
        })
    }

    fn sort_records(&mut self, order: Option<&OwnedNode>, with_dialog: bool) -> HostResult {
        let (mut sort, mut spec) = match order {
            Some(node) => {
                let spec = om_data::specs::SortSpec::from_node(node)
                    .ok_or(HostError::Error(ErrorCode::INVALID))?;
                (self.sort_fields(spec.clone())?, Some(spec))
            }
            None => {
                // Another table's stored order is not this table's.
                let window = &self.s.file.window;
                let spec = window
                    .last_sort_spec
                    .clone()
                    .filter(|(table, _)| *table == window.table)
                    .map(|(_, spec)| spec);
                (window.last_sort.clone(), spec)
            }
        };
        if with_dialog {
            let (layout, fields) = self.sort_dialog_layout();
            let current = match spec.clone() {
                Some(spec) => spec,
                None => self.spec_of(&sort, &fields),
            };
            let occurrence = self.occurrence();
            match self.ask(Prompt::Sort {
                layout,
                occurrence,
                fields,
                current,
            }) {
                Some(Answer::Sort(BrowseSort::Sort(chosen))) => {
                    sort = self.sort_fields(chosen.clone())?;
                    spec = Some(chosen);
                }
                // The dialog's Unsort, as Records > Unsort (OpenMaker's
                // reading for a script's dialog: the step then succeeds).
                Some(Answer::Sort(BrowseSort::Unsort)) => return self.unsort_records(),
                _ => return Err(ErrorCode::CANCELED.into()),
            }
        }
        if sort.is_empty() {
            return Ok(());
        }
        self.commit_window()?;
        self.change(FoundSetChange::Sort(sort))?;
        let table = self.s.file.window.table;
        self.s.file.window.last_sort_spec = spec.map(|spec| (table, spec));
        self.after(TriggerEvent::RecordLoad, None);
        Ok(())
    }

    fn sort_by_field(&mut self, field: Option<FieldRef>, direction: u16) -> HostResult {
        if let Some(field) = field {
            self.check_imported_sort_field(Some(field.occurrence), field.repetition)?;
        }
        let name = self.target_name(field)?;
        self.commit_window()?;
        self.change(FoundSetChange::Sort(vec![SortField::new(
            name,
            if direction == 2 {
                SortOrder::Descending
            } else {
                SortOrder::Ascending
            },
        )]))
    }
}

impl Windows for WorkerHost<'_> {
    fn new_window(&mut self, window: &NewWindow) -> HostResult {
        let style = self.new_window_style(window.window_options(), window.bounds)?;
        let id = window.layout.and_then(|layout| {
            self.layouts()
                .iter()
                .find(|l| u64::from(l.id) == layout)
                .map(|l| l.id)
        });
        // A layout the account may not see opens the window there too, on
        // FileMaker Pro's blank page, with 0 (#696's reading); it is never
        // entered, so no layout trigger runs [I].
        self.open_window(window.name.clone(), style)?;
        // Not the name Go to Related Record gave the window it came from.
        self.s.file.window.state.hidden_named = false;
        if let Some(id) = id.filter(|id| Some(*id) != self.s.file.window.state.layout) {
            // The new window enters its layout (s35 B1: OnLayoutEnter runs
            // before the next step).
            self.show_layout(id);
            if self.layout_shown(id) {
                self.after_layout_entered();
            }
        }
        self.move_resize_window(&WindowTarget::Current, window.bounds, false)
    }

    fn close_window(&mut self, target: &WindowTarget, _current_file_only: bool) -> HostResult {
        if self.s.file.others.is_empty() && self.s.active_files.len() > 1 {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        self.close(target)
    }

    fn select_window(&mut self, target: &WindowTarget, current_file_only: bool) -> HostResult {
        self.check_dialog_selection(target)?;
        if let Err(error) = self.select(target) {
            // Another file's window, which must be shown already: one
            // opened hidden is not found from here (script-runtime.md 9.2).
            if !current_file_only {
                if let WindowTarget::Named(name) = target {
                    if let Some(window) = self.named_database_window(name) {
                        self.select_database_window(window)?;
                        self.send(ScriptEvent::Viewport(egui::ViewportCommand::Focus));
                        return Ok(());
                    }
                }
            }
            return Err(error);
        }
        // A window of the script's own file is shown, hidden or not: a file
        // a script in it was opened for, hidden, shows here (#1313). Not
        // over a dialog: showing a document window then fails, as opening
        // one does (`new_window_style`).
        if self.s.file.window.state.hidden && self.dialog_open() {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        self.reveal_window();
        Ok(())
    }

    fn set_window_title(
        &mut self,
        target: &WindowTarget,
        title: &str,
        _current_file_only: bool,
    ) -> HostResult {
        self.on_window(target, |host| {
            host.s.file.window.state.title = Some(title.to_owned());
            host.s.file.window.touch();
            Ok(())
        })
    }

    fn move_resize_window(
        &mut self,
        target: &WindowTarget,
        bounds: Bounds,
        _current_file_only: bool,
    ) -> HostResult {
        self.on_window(target, |host| {
            // A card moves on its parent, not on the desktop.
            if host.move_resize_card(bounds) {
                return Ok(());
            }
            // FileMaker measures the position from the desktop area and the
            // size as the outer frame's (calc-functions.md 6.3 O1); the
            // window system is told the client area's size.
            let mut placement = host
                .s
                .file
                .window
                .state
                .placement
                .clone()
                .unwrap_or_default();
            let (chrome_w, chrome_h) = placement.chrome();
            let size = (bounds.width, bounds.height);
            if size.0.is_some() || size.1.is_some() {
                let w = size.0.unwrap_or(placement.frame.width) - chrome_w;
                let h = size.1.unwrap_or(placement.frame.height) - chrome_h;
                host.send(ScriptEvent::Viewport(egui::ViewportCommand::InnerSize(
                    egui::vec2(w.max(1.0) as f32, h.max(1.0) as f32),
                )));
            }
            if bounds.top.is_some() || bounds.left.is_some() {
                let left = bounds
                    .left
                    .unwrap_or(placement.frame.left - placement.desktop.left);
                let top = bounds
                    .top
                    .unwrap_or(placement.frame.top - placement.desktop.top);
                host.send(ScriptEvent::Viewport(egui::ViewportCommand::OuterPosition(
                    egui::pos2(
                        (placement.desktop.left + left) as f32,
                        (placement.desktop.top + top) as f32,
                    ),
                )));
            }
            placement.move_resize(bounds.left, bounds.top, bounds.width, bounds.height);
            host.s.file.window.state.placement = Some(placement);
            host.s.file.window.touch();
            Ok(())
        })
    }

    fn adjust_window(&mut self, choice: u16) -> HostResult {
        // Cards cannot be minimized or maximized, and have no window of
        // their own to resize (Claris, "About window styles").
        if self.s.file.window.state.style.card().is_some() {
            return Ok(());
        }
        let command = match choice {
            1 => return self.resize_to_fit(),
            2 => egui::ViewportCommand::Maximized(true),
            3 | 5 => egui::ViewportCommand::Minimized(true),
            4 => egui::ViewportCommand::Maximized(false),
            _ => return Err(ErrorCode::INVALID.into()),
        };
        self.send(ScriptEvent::Viewport(command));
        Ok(())
    }

    fn arrange_windows(&mut self, _choice: u16) -> HostResult {
        Ok(())
    }

    fn freeze_window(&mut self) -> HostResult {
        self.flush();
        self.s.file.window.frozen = true;
        Ok(())
    }

    fn refresh_window(&mut self, flush_joins: bool, flush_external: bool) -> HostResult {
        // A first-step refresh may precede any worker record read. Keep the
        // originals for drafted fields (including a user's cleared field),
        // so the UI does not mistake an empty draft for an unchanged value.
        if !self.s.file.window.state.drafts.is_empty() {
            let (window, provider) = self.wp();
            window.current(provider).map_err(data_error)?;
        }
        // The provider's working set must expire before the UI can request
        // replacement data. Worker requests and events are serialized.
        self.p()
            .refresh_window(flush_joins, flush_external)
            .map_err(data_error)?;
        self.s.file.cache.borrow_mut().refresh_window(flush_joins);
        self.s.file.window.frozen = false;
        self.s.file.window.refresh_data();
        Ok(())
    }

    fn scroll_window(&mut self, choice: u16) -> HostResult {
        self.s.file.window.scroll(choice)
    }

    fn set_zoom_level(&mut self, choice: u16, _lock: bool) -> HostResult {
        let zoom = &mut self.s.file.window.state.zoom;
        *zoom = match choice {
            1 => 25,
            2 => 50,
            3 => 75,
            4 => 100,
            5 => 150,
            6 => 200,
            7 => 300,
            8 => 400,
            9 | 10 => {
                let mut level = crate::document::zoom::Zoom::default();
                level.set_percent(*zoom);
                if choice == 9 {
                    level.zoom_in();
                } else {
                    level.zoom_out();
                }
                level.percent()
            }
            _ => return Err(ErrorCode::INVALID.into()),
        };
        self.s.file.window.touch();
        Ok(())
    }

    fn view_as(&mut self, choice: u16) -> HostResult {
        let view = &mut self.s.file.window.state.view;
        *view = match (choice, *view) {
            (1, _) => View::Form,
            (2, _) => View::List,
            (3, _) => View::Table,
            (4, View::Form) => View::List,
            (4, View::List) => View::Table,
            (4, View::Table) => View::Form,
            _ => return Err(ErrorCode::INVALID.into()),
        };
        let state = &mut self.s.file.window.state;
        if let Some(layout) = state.layout {
            state.views.insert(layout, state.view);
        }
        self.s.file.window.touch();
        self.after(TriggerEvent::ViewChange, None);
        Ok(())
    }

    fn show_hide(&mut self, bar: Bar, visibility: Visibility, _lock: bool) -> HostResult {
        let state = &mut self.s.file.window.state;
        let flag = match bar {
            Bar::Toolbars { .. } => &mut state.toolbar,
            Bar::Menubar => &mut state.menubar,
            Bar::TextRuler | Bar::TouchKeyboard => return Err(HostError::Unsupported),
        };
        *flag = match visibility {
            Visibility::Show => true,
            Visibility::Hide => false,
            Visibility::Toggle => !*flag,
        };
        self.s.file.window.touch();
        Ok(())
    }

    fn refresh_object(&mut self, name: &str, repetition: u32, portal: bool) -> HostResult {
        self.refresh_named_object(name, repetition, portal)
    }

    fn set_web_viewer(&mut self, object: &str, action: u16, url: Option<&str>) -> HostResult {
        self.set_named_web_viewer(object, action, url)
    }

    fn perform_javascript(&mut self, object: &str, function: &str, args: &[Value]) -> HostResult {
        self.perform_named_javascript(object, function, args)
    }
}

impl Dialogs for WorkerHost<'_> {
    fn show_custom_dialog(&mut self, dialog: &CustomDialog) -> HostResult<DialogResponse> {
        match self.ask(Prompt::Dialog(dialog.clone())) {
            Some(Answer::Dialog(response)) => Ok(response),
            _ => Err(ErrorCode::CANCELED.into()),
        }
    }

    /// `Dialog.Run`: the timeout counts from now, when the alert is asked
    /// for.
    fn show_plugin_dialog(
        &mut self,
        dialog: &om_data::runtime::plugins::PluginDialog,
    ) -> HostResult<om_data::runtime::plugins::PluginDialogAnswer> {
        let deadline = (dialog.timeout).and_then(|t| std::time::Instant::now().checked_add(t));
        let prompt = Prompt::PluginDialog {
            dialog: dialog.clone(),
            deadline,
        };
        match self.ask(prompt) {
            Some(Answer::PluginDialog(answer)) => Ok(answer),
            _ => Err(ErrorCode::CANCELED.into()),
        }
    }

    fn open_menu_item(&mut self, step: u16) -> HostResult {
        match step {
            OPEN_SCRIPT_WORKSPACE | OPEN_MANAGE_DATABASE => {
                self.send(ScriptEvent::OpenMenu(step));
                Ok(())
            }
            _ => Err(HostError::Unsupported),
        }
    }
}

impl WorkerHost<'_> {
    /// Whether a data file the scripts of any open database hold is the
    /// file at native path `path`: a whole-file write or delete must not
    /// replace or remove one (a file another database's script opened
    /// included).
    fn data_file_held(&self, path: &str) -> bool {
        let Ok(target) = files::native_target(path) else {
            return false;
        };
        std::iter::once(&self.s.file.data_files)
            .chain(self.files.peers.values().map(|peer| &peer.state.data_files))
            .any(|data_files| data_files.holds(&target))
    }
}

impl Files for WorkerHost<'_> {
    fn import_records(&mut self, call: &om_data::runtime::TransferCall<'_>) -> HostResult {
        self.import_step(call)
    }

    fn export_records(&mut self, call: &om_data::runtime::TransferCall<'_>) -> HostResult {
        // Export Records and Save Records as Excel.
        self.require_options(false, true)?;
        self.export_step(call)
    }

    fn file_exists(&mut self, paths: &[String]) -> HostResult<bool> {
        Ok(self.s.file.data_files.exists(paths)?)
    }

    fn file_size(&mut self, paths: &[String]) -> HostResult<i64> {
        Ok(self.s.file.data_files.size(paths)?)
    }

    fn create_data_file(&mut self, paths: &[String], create_folders: bool) -> HostResult {
        Ok(self.s.file.data_files.create(paths, create_folders)?)
    }

    fn open_data_file(&mut self, paths: &[String]) -> HostResult<i64> {
        Ok(self.s.file.data_files.open(paths)?)
    }

    fn write_data_file(
        &mut self,
        id: i64,
        data: &Value,
        encoding: Encoding,
        line_feed: bool,
    ) -> HostResult {
        let text = self.text(data);
        Ok(self
            .s
            .file
            .data_files
            .write(id, data, |_| text, encoding, line_feed)?)
    }

    fn read_data_file(
        &mut self,
        id: i64,
        amount: Option<i64>,
        encoding: Encoding,
    ) -> HostResult<Value> {
        Ok(self.s.file.data_files.read(id, amount, encoding)?)
    }

    fn data_file_position(&mut self, id: i64) -> HostResult<i64> {
        Ok(self.s.file.data_files.position(id)?)
    }

    fn set_data_file_position(&mut self, id: i64, position: i64) -> HostResult {
        Ok(self.s.file.data_files.set_position(id, position)?)
    }

    fn close_data_file(&mut self, id: i64) -> HostResult {
        Ok(self.s.file.data_files.close(id)?)
    }

    fn delete_file(&mut self, paths: &[String]) -> HostResult {
        Ok(self.s.file.data_files.delete(paths)?)
    }

    fn rename_file(&mut self, paths: &[String], new_name: &str) -> HostResult {
        Ok(self.s.file.data_files.rename(paths, new_name)?)
    }

    fn desktop_folder(&mut self) -> Option<String> {
        self.s.file.data_files.desktop_folder()
    }

    fn create_folder(&mut self, path: &str) -> HostResult {
        Ok(self.s.file.data_files.create_folder(path)?)
    }

    fn read_file(&mut self, path: &str, limit: u64) -> HostResult<Vec<u8>> {
        Ok(self.s.file.data_files.read_native(path, limit)?)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> HostResult {
        if self.data_file_held(path) {
            return Err(ErrorCode(files::code::IN_USE).into());
        }
        Ok(self.s.file.data_files.write_native(path, data)?)
    }

    fn native_path(&mut self, path: &str, pass_through: bool) -> HostResult<String> {
        Ok(self.s.file.data_files.native_of(path, pass_through)?)
    }

    fn delete_native_file(&mut self, path: &str) -> HostResult {
        if self.data_file_held(path) {
            return Err(ErrorCode(files::code::IN_USE).into());
        }
        Ok(self.s.file.data_files.delete_native(path)?)
    }

    fn check_pdf_text(&mut self, block: &om_data::runtime::dynapdf_text::TextBlock) -> HostResult {
        pdf_text::check(block).map_err(pdf_text::Refusal::error)
    }

    fn render_pdf_text(
        &mut self,
        pages: &[&[om_data::runtime::dynapdf_text::TextBlock]],
    ) -> HostResult<Vec<u8>> {
        pdf_text::render(pages).map_err(pdf_text::Refusal::error)
    }

    fn open_file(&mut self, source: FileSource<'_>, hidden: bool) -> HostResult {
        self.open_database(source, hidden).map(|_| ())
    }
    fn open_file_from(&mut self, from: FileId, source: FileSource<'_>, hidden: bool) -> HostResult {
        self.open_database_from(from, source, hidden)
    }
    fn close_file_from(&mut self, from: FileId, source: FileSource<'_>) -> HostResult {
        self.close_database_from(from, source)
    }
    fn close_file(&mut self, source: FileSource<'_>) -> HostResult {
        self.close_database(source)
    }
}

impl Network for WorkerHost<'_> {
    fn open_url(&mut self, url: &str, _with_dialog: bool, _external: bool) -> HostResult {
        if url.trim().is_empty() {
            return Err(ErrorCode::INVALID.into());
        }
        self.send(ScriptEvent::OpenUrl(url.trim().to_owned()));
        Ok(())
    }

    /// Insert from URL (`om_url`): into a container field as container
    /// data, another field as text; a `file:` URL reads a local file.
    fn fetch_url(&mut self, request: &UrlRequest) -> HostResult<UrlResponse> {
        let into = match request.target {
            None => om_url::Into::Variable,
            Some(target) => {
                let name = self.target_name(target.field)?;
                if self.is_container_field(&name) {
                    om_url::Into::Container
                } else {
                    om_url::Into::TextField
                }
            }
        };
        let url = om_url::request_url(request);
        let fetched = match om_url::file_path(&url) {
            Some(path) => containers::read_url_file(&path, request, url)?,
            None => {
                // Esc, Cmd-. and a closing window end the step at any point
                // of the transfer; the abort stays pending for the runtime
                // to act on. A desktop script has no time limit.
                let mut cancelled = || {
                    self.pump(None);
                    self.connection_lost()
                        || self.s.abort.load(Ordering::SeqCst)
                        || self.s.interrupted
                        || self.s.halt
                };
                om_url::fetch(request, None, &mut cancelled)?
            }
        };
        Ok(om_url::response(fetched, into)?)
    }

    fn perform_script_on_server(
        &mut self,
        call: &ServerScriptCall,
    ) -> HostResult<ServerScriptResult> {
        self.commit_window()?;
        let result = self
            .p()
            .perform_script_on_server(call)
            .map_err(|e| HostError::Error(ErrorCode(e.code.unwrap_or(ErrorCode::UNAVAILABLE.0))))?;
        if call.wait {
            // The server script may have changed records.
            self.s.file.window.changed_data();
            *self.s.file.cache.borrow_mut() = CalcData::default();
        }
        Ok(result)
    }

    fn perform_script_on_server_with_callback(&mut self, call: &ServerCallbackCall) -> HostResult {
        self.commit_window()?;
        self.s.file.next_callback += 1;
        let id = self.s.file.next_callback;
        let started = self
            .p()
            .perform_script_on_server_with_callback(&call.call, id)
            .map_err(|e| HostError::Error(ErrorCode(e.code.unwrap_or(ErrorCode::UNAVAILABLE.0))))?;
        if started.is_error() {
            return Err(started.into());
        }
        if let Some((_, script)) = call.callback {
            self.s.file.callbacks.insert(
                id,
                super::session::Callback {
                    script,
                    parameter: call.callback_parameter.clone(),
                },
            );
        }
        Ok(())
    }
}

impl Accounts for WorkerHost<'_> {}

impl Platform for WorkerHost<'_> {
    fn beep(&mut self) -> HostResult {
        self.send(ScriptEvent::Beep);
        Ok(())
    }

    fn exit_application(&mut self) -> HostResult {
        self.commit_window()?;
        self.s.halt = true;
        self.send(ScriptEvent::Quit);
        Ok(())
    }

    fn flush_cache(&mut self) -> HostResult {
        *self.s.file.cache.borrow_mut() = CalcData::default();
        Ok(())
    }
}

/// A layout trigger parameter as a step calculation.
pub(crate) fn parameter(stored: &om_layout::triggers::StoredFormula) -> Calculation {
    Calculation {
        context: stored.context.clone(),
        formatted: None,
        polish: Some(stored.polish.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_numbers_count_up_the_last_digits() {
        assert_eq!(serial_text("1", 1, 0), "1");
        assert_eq!(serial_text("1", 1, 4), "5");
        assert_eq!(serial_text("INV0009", 1, 1), "INV0010");
        assert_eq!(serial_text("A-10-x", 5, 2), "A-20-x");
        assert_eq!(serial_text("none", 1, 3), "none");
    }
}
