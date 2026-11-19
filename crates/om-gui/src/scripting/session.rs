//! The script runtime of an open file, on the provider worker: it starts
//! scripts the UI asks for, runs them in time slices (serving the UI's
//! other requests between steps), waits out pauses while the user works in
//! the window, and keeps OnTimer and Perform Script on Server callbacks.
//!
//! Once the open file's connection to the host is lost, scripts stop
//! wherever they are (a step, a pause, a dialog), Allow User Abort or not:
//! nothing they do could reach the host.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use om_data::calc::eval::{evaluate_formula, Variables};
use om_data::calc::Value;
use om_data::formula::NameTable;
use om_data::host::{CalcData, CalcSchema};
use om_data::runtime::{
    OnTimer, QueueMode, Records, RunState, Runtime, ScriptCall, ScriptHost, Trigger, TriggerEvent,
};
use om_data::{
    Calculation, Connection, DataError, DataProvider, LayoutInfo, RenderLayout, Script,
    ScriptCatalog,
};
use zeroize::Zeroize;

use super::host::{self, Held, WorkerHost};
use super::window::Window;
use super::{Answer, ScriptEvent, ScriptJob, SessionContext, Start};
use crate::licensing::LicenseStore;
use crate::worker::{self, Job, Request, Response};

/// How often a waiting worker asks the host for callback results.
const CALLBACK_POLL: Duration = Duration::from_millis(250);

/// The worker's side of the channels and the provider.
pub(crate) struct Io<'a> {
    pub provider: &'a mut Box<dyn DataProvider>,
    pub files: &'a mut super::databases::OpenFiles,
    pub store: &'a mut dyn LicenseStore,
    pub jobs: &'a Receiver<Job>,
    pub responses: &'a Sender<Response>,
}

impl Io<'_> {
    pub fn route(&mut self, s: &mut Shared, window: u32) -> bool {
        let Some(id) = self.files.owner(&s.file, window) else {
            return false;
        };
        self.files.select(id, self.provider, s)
    }
}

/// A Perform Script on Server with Callback whose result has not come.
#[derive(Clone, Debug)]
pub(crate) struct Callback {
    pub script: u64,
    pub parameter: Option<Calculation>,
}

/// Where a file was opened, and with which account (never a password).
#[derive(Clone, Debug)]
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub file: String,
    pub account: String,
    pub local: Option<std::path::PathBuf>,
}

/// [`FileState::pending`]: the window's own record and its other unsaved
/// records it was computed over, with the generation of the provider's
/// calculation data then (`DataProvider::calc_generation`: a stored record
/// it read may have changed since) and whether it was computed in the
/// window's record session set aside for another window's transaction,
/// and what the provider answered (`None`: it cannot compute it).
pub(crate) type PendingComputed = (
    (
        om_data::ScriptWrite,
        Vec<om_data::PendingRecord>,
        Option<u64>,
        bool,
    ),
    Option<std::sync::Arc<om_data::PendingValues>>,
);

/// What the host keeps between steps and scripts.
pub(crate) struct FileState {
    pub id: u64,
    /// Where the file was opened: the user's reconnect opens it there
    /// again, with the same account.
    pub endpoint: Option<Endpoint>,
    pub page_setup: Option<crate::page_setup::Settings>,
    /// The window the script works on, and the file's other windows, the
    /// most recently current last (Close Window returns to it).
    pub window: Window,
    pub others: Vec<Window>,
    pub context: SessionContext,
    /// The file's calculation schema (read once); script catalog and
    /// layouts as read.
    pub schema: Option<Option<Arc<CalcSchema>>>,
    pub catalog: Option<ScriptCatalog>,
    pub scripts: HashMap<u64, Arc<Script>>,
    pub layouts: Option<Vec<LayoutInfo>>,
    pub rendered: HashMap<u32, Option<Arc<RenderLayout>>>,
    /// The window layout's occurrence, names and shown fields (for
    /// calculations without a schema), and the current record's stored
    /// values.
    pub occurrence: Option<u64>,
    pub names: NameTable,
    pub fields: HashMap<(u64, u64), String>,
    pub record_values: HashMap<String, String>,
    /// Data calculations read, until records change.
    pub cache: RefCell<CalcData>,
    /// The window's own record as its commit would store it, computed by
    /// the provider over its unsaved changes (`DataProvider::compute_pending`,
    /// #1104), with the pending records it was computed over: computed
    /// again when they change.
    pub pending: RefCell<Option<PendingComputed>>,
    /// Variables of calculations outside the runtime (unused by steps).
    pub variables: Variables,
    /// The UI was told the connection to the host was lost: the session's
    /// scripts are interrupted until the file opens again.
    pub told_lost: bool,
    pub timer: Option<OnTimer>,
    /// The script `FM.RunScriptIdle` scheduled from this window (the MBS
    /// plug-in subset, `om_data::runtime::plugins`); the UI keeps the
    /// clock.
    pub idle: Option<om_data::runtime::plugins::IdleScript>,
    pub callbacks: HashMap<u32, Callback>,
    pub next_callback: u32,
    /// Data files scripts opened (until Close Data File or the file closes).
    pub data_files: super::files::DataFiles,
    /// An open transaction's committed records, for OnWindowTransaction
    /// at Commit Transaction.
    pub transaction: Option<Vec<host::transaction::Entry>>,
    /// The connection was lost with a transaction open, or while one was
    /// committed or reverted: what became of its writes is unknown, after
    /// `transaction` is gone too. A reconnect is refused.
    pub lost_transaction: bool,
    /// The connection was lost while a record was committed with changes
    /// to related records, after the commit may have sent a host record
    /// data (`DataProvider::submission_mark`): whether the host has them
    /// is unknown, so they cannot be kept (a row begun would be created
    /// again). A reconnect is refused (#152). A commit that provably sent
    /// none leaves them to be read back and checked.
    pub lost_commit: bool,
    /// A commit with changes to related records failed with the connection
    /// lost having sent its hosts no record data: the window keeps its
    /// changes, and the reconnect tells the window so (#152).
    pub unsent_save: bool,
    /// OnWindowTransaction scripts the commits queued, run when every
    /// script has ended (script-runtime.md 13.26).
    pub window_transactions: VecDeque<Trigger>,
    /// The host's name and IPv4 address (`Get ( HostIPAddress )`).
    pub host_ip: Option<(String, String)>,
}

/// Why a reconnect cannot keep a record's uncommitted changes
/// ([`unrecoverable_changes`]).
pub(crate) const CHANGES_UNRECOVERABLE: &str = "Changes a script left uncommitted, or related \
    records deleted and not saved yet, on a record of this file cannot be recovered by \
    reconnecting. Copy what you need, then close the file and open it again.";

/// Why a reconnect cannot keep a window's new record
/// (`DataProvider::carried_record_sessions`, #152).
pub(crate) const NEW_RECORD_UNRECOVERABLE: &str = "A new record not saved yet in a window of \
    this file holds what a reconnect cannot carry to a new connection (container data, another \
    file's records, or a save that may have reached the host). Copy what you need, then close \
    the file and open it again.";

/// Whether a reconnect cannot keep a record's uncommitted changes (#152):
/// any a script made (its own cells, New Record, a related record's cells
/// the user did not type, or one a step created through the relationship),
/// or a related record deleted through it, which nothing can check
/// against the version the window showed. A user's typing into related
/// records and rows begun can be kept: they are read back and checked.
/// So can a new record held in the window's record session when the
/// reconnect carries that session (`carried`: the provider holds it again
/// under a new id, its values as they were), with the cells scripts set
/// on it: nothing of it reached the host.
pub(crate) fn unrecoverable_changes(
    uncommitted: &crate::scripting::window::Uncommitted,
    carried: bool,
) -> bool {
    let carried_related = carried && uncommitted.related_recoverable;
    let carried_new = carried && uncommitted.new && uncommitted.session && !uncommitted.saved;
    let carried_attachment = carried
        && !uncommitted.new
        && uncommitted.session
        && uncommitted.attachment_field.is_some_and(|field| {
            // The complete mixed group includes Root's prepared scalar
            // footprint, not only its attachment. The worker repeats this
            // check with the provider's actual carried window IDs.
            uncommitted.cells.keys().all(|cell| *cell == field)
                || (carried_related && !uncommitted.saved)
        });
    (!uncommitted.cells.is_empty() && !carried_new && !carried_attachment)
        || (uncommitted.new && !carried_new)
        || uncommitted.related.iter().any(|draft| {
            let carried_attachment = carried
                && draft.record.is_some_and(|record| {
                    uncommitted
                        .related_attachments
                        .get(&(draft.table, record))
                        .is_some_and(|field| {
                            draft.version.is_some() && draft.cells.keys().all(|cell| cell == field)
                        })
                });
            draft.deleted
                || (carried_related && draft.record.is_none() && draft.reserved.is_none())
                || (!(carried_related || carried_attachment)
                    && (draft.matched
                        || draft.cells.keys().any(|cell| {
                            !draft.typed.contains(cell) && !draft.matched_cells.contains(cell)
                        })))
        })
}

impl FileState {
    /// The file opened again in a fresh session after its connection was
    /// lost: what was read of the lost session (catalogs, layouts, data,
    /// the host's address) is read again, and what belonged to it (an open
    /// transaction, OnTimer, Perform Script on Server callbacks, queued
    /// OnWindowTransaction scripts) is gone, not replayed. The windows and
    /// the scripts' variables stay.
    pub(crate) fn forget_session(&mut self) {
        self.schema = None;
        self.catalog = None;
        self.scripts.clear();
        self.layouts = None;
        self.rendered.clear();
        self.occurrence = None;
        self.names = NameTable::default();
        self.fields.clear();
        self.record_values.clear();
        *self.cache.get_mut() = CalcData::default();
        self.told_lost = false;
        self.timer = None;
        self.idle = None;
        self.callbacks.clear();
        self.transaction = None;
        self.lost_transaction = false;
        self.lost_commit = false;
        self.unsent_save = false;
        self.window_transactions.clear();
        self.host_ip = None;
    }

    /// Why the file's lost session holds what a reconnect cannot recover,
    /// if it does: a transaction (open, or lost while committed), or
    /// changes scripts left uncommitted in one of its windows (the window
    /// may never have shown them: the loss reaches it first). `carried`:
    /// the windows whose record sessions the reconnect carries
    /// (`DataProvider::carried_record_sessions`).
    pub(crate) fn unrecoverable(&self, carried: &[u32]) -> Option<&'static str> {
        if self.transaction.is_some() || self.lost_transaction {
            return Some(
                "A transaction a script opened was not committed when the connection was lost, \
                 or whether its commit reached the host is unknown. Reconnecting cannot recover \
                 it: copy what you need, then close the file and open it again.",
            );
        }
        if self.lost_commit {
            return Some(
                "A save of changes to related records was on its way when the connection was \
                 lost, and whether the host has them cannot be told. Reconnecting cannot \
                 recover them: copy what you need, then close the file and open it again.",
            );
        }
        // A user's own changes to related records were never sent (a save
        // that may have sent them is refused, above and by the window):
        // they stay, read back and checked (#152).
        let windows = || std::iter::once(&self.window).chain(&self.others);
        if windows().any(|w| unrecoverable_changes(&w.state.uncommitted, true)) {
            return Some(CHANGES_UNRECOVERABLE);
        }
        if windows().any(|w| w.state.uncommitted.related_recoverable && !carried.contains(&w.id()))
        {
            return Some("Unresolved: the related save no longer has a complete no-data receipt. All drafts and issued values are kept; nothing will be sent.");
        }
        windows()
            .any(|w| unrecoverable_changes(&w.state.uncommitted, carried.contains(&w.id())))
            .then_some(NEW_RECORD_UNRECOVERABLE)
    }

    /// The windows whose new record a reconnect carries
    /// (`DataProvider::carried_record_sessions`), less those with a row
    /// begun whose preview would not outlive it
    /// (`DataProvider::preview_survives_reconnect`): its creation values
    /// would be drawn anew at the save (#152).
    pub(crate) fn carried_windows(&self, provider: &dyn DataProvider) -> Vec<u32> {
        let carried = provider.carried_record_sessions();
        std::iter::once(&self.window)
            .chain(&self.others)
            .filter(|w| carried.contains(&w.id()))
            .filter(|w| {
                w.state.uncommitted.related.iter().all(|draft| {
                    (w.state.uncommitted.related_recoverable && draft.reserved.is_some())
                        || draft
                            .prepared
                            .as_ref()
                            .is_none_or(|prepared| provider.preview_survives_reconnect(prepared.id))
                })
            })
            .map(|w| w.id())
            .collect()
    }

    /// Each window's new record as the reconnect held it again
    /// (`DataProvider::take_recreated_records`, #152): under its new id.
    /// One that could not be keeps everything, its id, its drafts and its
    /// cells, frozen until the user tries again or discards it
    /// (`Request::Recreate`).
    pub(crate) fn renumber_recreated(&mut self, recreated: &[om_data::RecreatedRecord]) {
        for record in recreated {
            let Some(new) = record
                .new
                .as_ref()
                .ok()
                .and_then(|new| i64::try_from(*new).ok())
            else {
                continue;
            };
            let Some(window) = std::iter::once(&mut self.window)
                .chain(self.others.iter_mut())
                .find(|w| w.id() == record.window)
            else {
                continue;
            };
            let mut changed = false;
            for draft in &mut window.state.uncommitted.related {
                if draft.table == record.table && draft.reserved == i64::try_from(record.old).ok() {
                    draft.reserved = Some(new);
                    draft.prepared = None;
                    changed = true;
                }
            }
            if window.table == Some(record.table)
                && window.state.record == i64::try_from(record.old).ok()
            {
                window.state.record = Some(new);
                changed = true;
            }
            if changed {
                window.changed_data();
            }
        }
    }

    /// The user discarded window `id`'s new record a reconnect could not
    /// make again (`DataProvider::discard_unrecreated`): it goes, with its
    /// changes.
    pub(crate) fn discard_new_record(&mut self, id: u32) {
        if let Some(window) = std::iter::once(&mut self.window)
            .chain(self.others.iter_mut())
            .find(|w| w.id() == id)
        {
            window.state.record = None;
            window.state.drafts.clear();
            window.state.uncommitted = Default::default();
            window.changed_data();
        }
    }
}

pub(crate) struct Shared {
    pub print_dispatcher: Arc<dyn crate::printing::Dispatcher>,
    pub file: FileState,
    pub active_files: HashSet<u64>,
    pub closed_files: Vec<u64>,
    pub epoch: u64,
    pub ctx: egui::Context,
    pub abort: Arc<AtomicBool>,
    pub next_prompt: u64,
    pub next_token: u64,
    /// Actions waiting for their pre-event triggers, by token.
    pub held: HashMap<u64, Held>,
    /// Events whose trigger is running with an action held.
    pub firing: HashMap<u64, (u32, u16)>,
    /// Scripts the UI started while a step waited for the user.
    pub queued: VecDeque<(u64, egui::Context, ScriptJob)>,
    /// A request that ended the session's scripts (a file opened or
    /// closed), served after them.
    pub deferred: Option<Job>,
    pub interrupted: bool,
    /// A run took the UI's windows ([`ScriptSession::start`]) and has not
    /// told its end ([`WorkerHost::finish`]): until then the windows'
    /// truth is the worker's, and a panic meanwhile tells it
    /// ([`ScriptSession::abandon`]).
    pub in_run: bool,
    /// Exit Application, Close File or Close Window on the last window.
    pub halt: bool,
    /// Why the last host step failed, in words, for the alert.
    pub failure: Option<String>,
    /// The last print or export step the set's options were checked for
    /// was refused for them: its alert is FileMaker's, not the error's
    /// (`WorkerHost::error_alert`). Each such step sets it, so an elevated
    /// step's later error is never taken for a refusal.
    pub options_refused: bool,
    /// Copy steps' text, for Paste.
    pub clipboard: Option<String>,
    /// The MBS plug-in subset's state: the alert `Dialog.*` builds.
    pub plugin: om_data::runtime::plugins::PluginState,
}

impl Shared {
    pub(crate) fn new(abort: Arc<AtomicBool>) -> Self {
        Shared {
            print_dispatcher: crate::printing::default_dispatcher(),
            file: FileState {
                id: 0,
                endpoint: None,
                page_setup: None,
                window: Window::default(),
                others: Vec::new(),
                context: SessionContext::default(),
                schema: None,
                catalog: None,
                scripts: HashMap::new(),
                layouts: None,
                rendered: HashMap::new(),
                occurrence: None,
                names: NameTable::default(),
                fields: HashMap::new(),
                record_values: HashMap::new(),
                cache: RefCell::default(),
                pending: RefCell::default(),
                variables: Variables::new(),
                told_lost: false,
                timer: None,
                idle: None,
                callbacks: HashMap::new(),
                next_callback: 0,
                data_files: super::files::DataFiles::default(),
                transaction: None,
                lost_transaction: false,
                lost_commit: false,
                unsent_save: false,
                window_transactions: VecDeque::new(),
                host_ip: None,
            },
            active_files: HashSet::new(),
            closed_files: Vec::new(),
            epoch: 0,
            ctx: egui::Context::default(),
            next_prompt: 0,
            next_token: 0,
            held: HashMap::new(),
            firing: HashMap::new(),
            queued: VecDeque::new(),
            deferred: None,
            interrupted: false,
            in_run: false,
            halt: false,
            failure: None,
            options_refused: false,
            clipboard: None,
            plugin: Default::default(),
            abort,
        }
    }

    /// A request the UI made changes what the session has read.
    pub(crate) fn observe(&mut self, request: &Request) {
        match request {
            Request::Scripts { .. } | Request::SaveScript { .. } => {
                self.file.catalog = None;
                self.file.scripts.clear();
            }
            Request::CommitSchema => {
                self.file.schema = None;
                self.file.layouts = None;
                self.file.rendered.clear();
                *self.file.cache.get_mut() = CalcData::default();
            }
            Request::PlaceLayoutField(_) | Request::PlaceLayoutButton(_) => {
                self.file.layouts = None;
                self.file.rendered.clear();
            }
            Request::UpdateRecord { .. } | Request::DeleteRecord { .. } => {
                *self.file.cache.get_mut() = CalcData::default();
            }
            _ => {}
        }
    }

    /// The window's layout changed (or was read): its occurrence, the
    /// names calculations compile with and the fields it shows.
    pub(crate) fn refresh_layout_info(&mut self, render: Option<Arc<RenderLayout>>) {
        self.file.names = NameTable::default();
        self.file.fields.clear();
        self.file.occurrence = None;
        let Some(render) = render else {
            return;
        };
        let alias = render.layout.def.as_ref().map_or(0, |d| d.table_alias_key);
        let Some(occurrence) = om_layout::occurrence_id(alias) else {
            return;
        };
        self.file.occurrence = Some(occurrence);
        self.file
            .names
            .add_occurrence(occurrence, &render.layout.name, occurrence);
        for object in &render.layout.objects.objects {
            let (Some(spec), Some(name)) =
                (object.field_spec(), render.field_bindings.get(&object.id))
            else {
                continue;
            };
            let Some(field) = spec.field_id() else {
                continue;
            };
            if spec.table_key == alias && spec.repetition <= 1 {
                self.file.names.add_field(occurrence, field, name);
                self.file.fields.insert((occurrence, field), name.clone());
            }
        }
    }

    /// A script starts while none runs: from the UI's windows as the user
    /// left them, `current` the one it starts in. A window the worker knew
    /// keeps what only it holds (the record each table showed last, the
    /// last find and sort).
    fn adopt_windows(&mut self, current: super::WindowState, others: Vec<super::WindowState>) {
        let mut known: Vec<Window> = std::mem::take(&mut self.file.others);
        known.push(std::mem::take(&mut self.file.window));
        self.file.window = take_window(&mut known, current);
        self.file.others = others
            .into_iter()
            .map(|state| take_window(&mut known, state))
            .collect();
        self.forget_calc_data();
    }

    /// The window the user left in front during a pause, as they left it:
    /// the script goes on there.
    fn adopt_current(&mut self, state: super::WindowState) {
        if state.id != self.file.window.id() {
            if let Some(i) = self.file.others.iter().position(|w| w.id() == state.id) {
                let window = self.file.others.remove(i);
                let previous = std::mem::replace(&mut self.file.window, window);
                self.file.others.push(previous);
            }
        }
        self.file.window.adopt(state);
        self.forget_calc_data();
    }

    /// Calculations read the windows' found sets by occurrence, and the
    /// window the script works in may be another one than they last read,
    /// or show other records since the user worked in it: they read again.
    fn forget_calc_data(&mut self) {
        *self.file.cache.get_mut() = CalcData::default();
    }

    /// Install OnTimer Script: the UI keeps the clock.
    pub(crate) fn set_timer(&mut self, timer: Option<OnTimer>) {
        self.file.timer = timer;
    }

    /// Whether the open file's connection to the host was lost. The first
    /// time, the UI is told, as of the running script's request, and its
    /// window woken; the session's scripts are interrupted from then on.
    pub(crate) fn connection_lost(
        &mut self,
        provider: &dyn DataProvider,
        responses: &Sender<Response>,
    ) -> bool {
        let ctx = self.ctx.clone();
        self.lost_as_of(provider, responses, self.epoch, &ctx)
    }

    /// [`Self::connection_lost`], telling the UI as of the request of
    /// `epoch` and waking `ctx`.
    fn lost_as_of(
        &mut self,
        provider: &dyn DataProvider,
        responses: &Sender<Response>,
        epoch: u64,
        ctx: &egui::Context,
    ) -> bool {
        if !self.file.told_lost {
            let Some(lost) = provider.connection_lost() else {
                return false;
            };
            self.file.told_lost = true;
            self.file.lost_transaction |= self.file.transaction.is_some();
            // The file's connection: the UI takes it for every window.
            let reply = worker::Reply::ConnectionLost(lost);
            let window = self.file.window.id();
            if responses
                .send(Response {
                    epoch,
                    window,
                    reply,
                })
                .is_ok()
            {
                ctx.request_repaint();
            }
        }
        self.interrupted = true;
        true
    }
}

/// Tell window `window` of the UI about `event`, as of the session's
/// request ([`WorkerHost::send_for`] without a host).
fn send_event(responses: &Sender<Response>, s: &Shared, window: u32, event: ScriptEvent) {
    let _ = responses.send(Response {
        epoch: s.epoch,
        window,
        reply: worker::Reply::Script(event),
    });
    s.ctx.request_repaint();
}

/// The window of `known` with `state`'s id (a new one when none has it),
/// as the UI left it.
fn take_window(known: &mut Vec<Window>, state: super::WindowState) -> Window {
    let mut window = match known.iter().position(|w| w.id() == state.id) {
        Some(i) => known.swap_remove(i),
        None => Window::default(),
    };
    window.adopt(state);
    window
}

pub(crate) struct ScriptSession {
    runtime: Runtime,
    s: Shared,
    /// The timer interval the UI was last told.
    told_timer: Option<f64>,
}

impl ScriptSession {
    pub(crate) fn set_print_dispatcher(
        &mut self,
        dispatcher: Arc<dyn crate::printing::Dispatcher>,
    ) {
        self.s.print_dispatcher = dispatcher;
    }

    pub(crate) fn new(abort: Arc<AtomicBool>) -> Self {
        ScriptSession {
            runtime: Runtime::new(host::options()),
            s: Shared::new(abort),
            told_timer: None,
        }
    }

    /// Between requests: tell the UI if the connection was lost (see
    /// [`Shared::connection_lost`]), as of the request of `epoch`.
    pub(crate) fn notice_loss(
        &mut self,
        provider: &dyn DataProvider,
        responses: &Sender<Response>,
        epoch: u64,
        ctx: &egui::Context,
        files: &mut super::databases::OpenFiles,
    ) {
        self.s.lost_as_of(provider, responses, epoch, ctx);
        for file in files.peers.values_mut() {
            if !file.state.told_lost {
                if let Some(error) = file.provider.connection_lost() {
                    file.state.told_lost = true;
                    file.state.lost_transaction |= file.state.transaction.is_some();
                    let _ = responses.send(Response {
                        epoch,
                        window: file.state.window.id(),
                        reply: worker::Reply::ConnectionLost(error),
                    });
                    ctx.request_repaint();
                }
            }
        }
    }

    /// A panic stopped the session's scripts (`worker::serve`). They end
    /// as Halt Script ends them: an open transaction reverted on the file
    /// that owns it, the actions their triggers held not going on, and the
    /// scripts the UI started meanwhile not run. A run that took the UI's
    /// windows then tells its end as any run does, with what it left in
    /// them (unsaved values, a frozen window), and why it stopped. The
    /// session's files, windows and their providers stay, so every window
    /// goes on being answered (#1247).
    pub(crate) fn abandon(&mut self, io: &mut Io<'_>) {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        const STOPPED: &str = "The script stopped unexpectedly.";
        let ScriptSession {
            runtime,
            s,
            told_timer,
        } = self;
        // Each call below that may reach the provider that panicked runs
        // guarded, the host's own construction too (it may read the
        // calculation schema): a second panic ends that call, not the
        // cleanup.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut host = WorkerHost::new(io, s);
            runtime.halt(&mut host);
            // The halt's trigger outcomes, as a run after Halt Script
            // gives them: held actions do not go on.
            runtime.run(&mut host);
        }));
        // A trigger whose start panicked before it ran has no outcome to
        // give: the user's action it held does not go on either.
        for (&token, &(window, _)) in &s.firing {
            if token & host::UI_TOKEN != 0 {
                send_event(
                    io.responses,
                    s,
                    window,
                    ScriptEvent::Action {
                        token,
                        proceed: false,
                        record: None,
                    },
                );
            }
        }
        runtime.clear();
        let reverted = catch_unwind(AssertUnwindSafe(|| {
            WorkerHost::new(io, s).revert_abandoned()
        }))
        .unwrap_or(false);
        s.queued.clear();
        s.held.clear();
        s.firing.clear();
        s.interrupted = false;
        s.halt = false;
        s.failure = None;
        s.abort.store(false, Ordering::SeqCst);
        let message = if reverted {
            STOPPED.to_owned()
        } else {
            // Closing the file reverts it (`DataProvider::close`).
            format!(
                "{STOPPED} Its open transaction could not be reverted, and changes made in the \
                 file now would join it: close the file and open it again to discard it."
            )
        };
        let ended = s.in_run && {
            let finished = catch_unwind(AssertUnwindSafe(|| {
                let mut host = WorkerHost::new(io, s);
                let timer = host.timer_interval();
                if timer != *told_timer {
                    *told_timer = timer;
                    host.send(ScriptEvent::Timer(timer));
                }
                host.finish(runtime, Some(message.clone()));
            }))
            .is_ok();
            // The end panicked too (in a provider's part of it): the
            // windows and the run's end are told without the providers.
            finished
                || catch_unwind(AssertUnwindSafe(|| {
                    let update = end_window(&mut s.file);
                    let mut thawed = Vec::new();
                    thaw(&mut s.file, false, &mut thawed);
                    for file in io.files.peers.values_mut() {
                        thaw(&mut file.state, true, &mut thawed);
                    }
                    for update in thawed {
                        let id = update.state.id;
                        send_event(io.responses, s, id, ScriptEvent::Window(Box::new(update)));
                    }
                    let finished = ScriptEvent::Finished {
                        window: Box::new(update),
                        globals: Box::new(runtime.globals(s.file.id).clone()),
                        message: Some(message.clone()),
                    };
                    send_event(io.responses, s, s.file.window.id(), finished);
                }))
                .is_ok()
        };
        s.in_run = false;
        if !ended {
            // No run had the windows, or telling its end failed: they stay
            // as the UI shows them.
            let window = s.file.window.id();
            send_event(io.responses, s, window, ScriptEvent::Failed(message));
        }
    }

    /// The request the stopped scripts deferred ([`Shared::deferred`]), to
    /// serve now.
    pub(crate) fn take_deferred(&mut self) -> Option<Job> {
        self.s.deferred.take()
    }

    /// A file opened or closed: its scripts, variables and caches go.
    fn reset(&mut self) {
        let abort = Arc::clone(&self.s.abort);
        let dispatcher = Arc::clone(&self.s.print_dispatcher);
        *self = ScriptSession::new(abort);
        self.s.print_dispatcher = dispatcher;
    }

    /// How long the worker may wait for the next request before
    /// [`Self::wake`] has work: the poll for callback results.
    pub(crate) fn next_wake(&self, files: &super::databases::OpenFiles) -> Option<Duration> {
        (!self.s.file.callbacks.is_empty()
            || files.peers.values().any(|f| !f.state.callbacks.is_empty()))
        .then_some(CALLBACK_POLL)
    }

    /// No request came in time: look for callback results.
    pub(crate) fn wake(&mut self, io: &mut Io<'_>) {
        for file in io
            .files
            .peers
            .values_mut()
            .filter(|f| !f.state.callbacks.is_empty())
        {
            if let Ok(results) = file.provider.server_script_results(Duration::ZERO) {
                for result in results {
                    if file.state.callbacks.contains_key(&result.callback) {
                        let _ = io.responses.send(Response {
                            epoch: self.s.epoch,
                            window: file.state.window.id(),
                            reply: worker::Reply::Script(ScriptEvent::Callback {
                                id: result.callback,
                                error: result.error.0,
                                result: result.result,
                            }),
                        });
                        self.s.ctx.request_repaint();
                    }
                }
            }
        }
        if self.s.file.callbacks.is_empty() {
            return;
        }
        let Ok(results) = io.provider.server_script_results(Duration::ZERO) else {
            return;
        };
        for result in results {
            if self.s.file.callbacks.contains_key(&result.callback) {
                let _ = io.responses.send(Response {
                    epoch: self.s.epoch,
                    window: self.s.file.window.id(),
                    reply: worker::Reply::Script(ScriptEvent::Callback {
                        id: result.callback,
                        error: result.error.0,
                        result: result.result,
                    }),
                });
                self.s.ctx.request_repaint();
            }
        }
    }

    /// Handle one request from the UI.
    pub(crate) fn handle(&mut self, io: &mut Io<'_>, job: Job) {
        let Job {
            epoch,
            window,
            request,
            ctx,
        } = job;
        if matches!(request, Request::ListFiles { .. }) {
            // Not about a file: no file is chosen, nor its window.
            let reply = worker::handle(io.provider.as_mut(), io.store, request);
            if io
                .responses
                .send(Response {
                    epoch,
                    window,
                    reply,
                })
                .is_ok()
            {
                ctx.request_repaint();
            }
            return;
        }
        if let Request::Recreate { retry } = request {
            let reply = self.recreate(io, window, retry);
            if io
                .responses
                .send(Response {
                    epoch,
                    window,
                    reply,
                })
                .is_ok()
            {
                ctx.request_repaint();
            }
            return;
        }
        if let Request::Reconnect(reconnect) = request {
            let reply = self.reconnect(io, window, *reconnect);
            if io
                .responses
                .send(Response {
                    epoch,
                    window,
                    reply,
                })
                .is_ok()
            {
                ctx.request_repaint();
            }
            return;
        }
        let opening = match &request {
            Request::Open { connection, .. } => Some(Endpoint {
                host: connection.host.clone(),
                port: connection.port,
                file: connection.file.clone(),
                account: connection.account.clone(),
                local: None,
            }),
            Request::OpenLocal(file) => Some(Endpoint {
                host: String::new(),
                port: 0,
                file: file
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                account: file
                    .credentials
                    .as_ref()
                    .map(|(account, _)| account.clone())
                    .unwrap_or_default(),
                local: Some(file.path.clone()),
            }),
            _ => None,
        };
        if matches!(request, Request::Open { .. } | Request::OpenLocal(_)) {
            io.files.close_all();
            self.reset();
        } else if !io.route(&mut self.s, window) {
            return;
        }
        match request {
            Request::Close => {
                self.runtime.clear();
                let id = self.s.file.id;
                let reply = worker::handle(io.provider.as_mut(), io.store, Request::Close);
                self.runtime.close_file(id);
                if let Some(next) = io.files.peers.keys().next().copied() {
                    io.files.select(next, io.provider, &mut self.s);
                    io.files.peers.remove(&id);
                } else {
                    self.reset();
                }
                let _ = io.responses.send(Response {
                    epoch,
                    window,
                    reply,
                });
                ctx.request_repaint();
            }
            Request::Script(script) => self.start(io, epoch, ctx, *script),
            // A prompt or pause that no longer waits.
            Request::Answer { .. } => {}
            request => {
                self.s.observe(&request);
                let reply = worker::handle_in(io.provider.as_mut(), io.store, window, request);
                // An open the worker wrapped with the schema it grew is
                // still an open: its endpoint is kept for Reconnect.
                match reply.answered() {
                    worker::Reply::Opened(Ok(_)) => self.s.file.endpoint = opening,
                    worker::Reply::OpenedLocal(Ok((om_data::LocalOpen::Opened { .. }, opened))) => {
                        self.s.file.endpoint = opening.map(|mut endpoint| {
                            if let Some(account) = &opened.account {
                                endpoint.account = account.clone();
                            }
                            endpoint
                        });
                    }
                    _ => {}
                }
                if io
                    .responses
                    .send(Response {
                        epoch,
                        window,
                        reply,
                    })
                    .is_ok()
                {
                    ctx.request_repaint();
                }
            }
        }
        if let Some(job) = self.s.deferred.take() {
            self.s.interrupted = false;
            self.handle(io, job);
        }
    }

    /// The user's reconnect of the window's file after its connection was
    /// lost: its provider opens the file again where it was opened, with
    /// the account it was opened with (`DataProvider::reopen`, a fresh
    /// session), then reads back the records the UI asks for and finds
    /// each window's current record. Only the window's file reconnects:
    /// the session's other files, and their windows, stay as they are. A
    /// failure leaves the file as lost as it was.
    /// The user's answer to a new record a reconnect could not make again
    /// (#152): try again (`retry`), or discard it. Nothing is sent again
    /// but op71, and nothing is computed again.
    fn recreate(&mut self, io: &mut Io<'_>, window: u32, retry: bool) -> worker::Reply {
        if !io.route(&mut self.s, window) {
            return worker::Reply::Recreated(Err(DataError::new(
                "This window's file is no longer open.",
            )));
        }
        // The provider acts on its current window: this one's, not the last
        // it served.
        io.provider.use_window(window);
        let layout = std::iter::once(&self.s.file.window)
            .chain(&self.s.file.others)
            .find(|w| w.id() == window)
            .and_then(|w| w.state.layout);
        let reply = if retry {
            let result = worker::contain(io.provider.as_mut(), |p| p.recreate_record());
            let recreated = io.provider.take_recreated_records();
            self.s.file.renumber_recreated(&recreated);
            let made = recreated
                .iter()
                .filter(|record| record.window == window)
                .find_map(|record| {
                    let new = record.new.as_ref().ok()?;
                    Some((i64::try_from(record.old).ok()?, i64::try_from(*new).ok()?))
                });
            match (result, made) {
                (Err(error), _) => Err(error),
                // Made again, but not this window's: nothing to show.
                (Ok(()), None) => Err(DataError::new(
                    "The new record was made again, but not for this window.",
                )),
                (Ok(()), Some((old, new))) => {
                    // Where it is in the window's found set now: after
                    // records other clients made meanwhile, in an unsorted
                    // one.
                    let position = layout
                        .and_then(|layout| io.provider.record_position(layout, new).ok().flatten());
                    Ok(Some((old, new, position)))
                }
            }
        } else {
            let result = worker::contain(io.provider.as_mut(), |p| p.discard_unrecreated());
            if result.is_ok() {
                self.s.file.discard_new_record(window);
            }
            result.map(|()| None)
        };
        io.provider.use_window(self.s.file.window.id());
        worker::Reply::Recreated(reply)
    }

    fn reconnect(
        &mut self,
        io: &mut Io<'_>,
        window: u32,
        job: worker::ReconnectJob,
    ) -> worker::Reply {
        let failed =
            |message: &str| worker::Reply::Reconnected(Box::new(Err(DataError::new(message))));
        if !io.route(&mut self.s, window) {
            return failed("This window's file is no longer open.");
        }
        if !self.s.file.told_lost && io.provider.connection_lost().is_none() {
            return failed("This file is still connected to the host.");
        }
        // Before the old session goes with what it held.
        let carried = self.s.file.carried_windows(io.provider.as_ref());
        if let Some(why) = self.s.file.unrecoverable(&carried) {
            return failed(why);
        }
        let Some(endpoint) = self.s.file.endpoint.clone() else {
            return failed("OpenMaker cannot tell where this file was opened.");
        };
        let worker::ReconnectJob {
            password,
            use_saved_license,
            records,
            current,
            related,
            keep_lost,
        } = job;
        let local = endpoint.local;
        let mut connection = Connection {
            host: endpoint.host,
            port: endpoint.port,
            file: endpoint.file,
            account: endpoint.account,
            password: password.as_str().to_owned(),
        };
        drop(password);
        let store = &mut *io.store;
        let opened = worker::contain(io.provider.as_mut(), |provider| {
            if let Some(path) = local {
                provider.reopen_local(&om_data::LocalFile {
                    path,
                    credentials: Some((
                        connection.account.clone(),
                        connection.password.clone().into(),
                    )),
                })
            } else {
                worker::with_client_license(provider, store, use_saved_license, |provider| {
                    provider.reopen(&connection)
                })
            }
        });
        connection.password.zeroize();
        if let Err(error) = opened {
            return worker::Reply::Reconnected(Box::new(Err(error)));
        }
        let mut unsent = Vec::new();
        if self.s.file.unsent_save {
            unsent.push(self.s.file.id);
        }
        // The windows' new records the provider held again (#152).
        let mut recreated = io.provider.take_recreated_records();
        self.s.file.renumber_recreated(&recreated);
        self.s.file.forget_session();
        self.s.interrupted = false;
        // The other files that were on the lost connection, on the same
        // host and port and opened with the same account, reopen on the new
        // one with the account they hold (#1011): nothing more is typed. One
        // that does not (another host or account, a refusal, a session that
        // held what a reconnect cannot recover) stays lost, told so, for its
        // own Reconnect; none is tried again.
        let own = self.s.file.id;
        let mut recovered = vec![(own, io.provider.account_privileges())];
        let peers: Vec<u64> = io.files.peers.keys().copied().collect();
        for id in peers {
            let Some(peer) = io.files.peers.get_mut(&id) else {
                continue;
            };
            if peer.provider.connection_lost().is_none()
                || peer
                    .state
                    .unrecoverable(&peer.state.carried_windows(peer.provider.as_ref()))
                    .is_some()
                || keep_lost.contains(&id)
            {
                continue;
            }
            if let Ok(true) = worker::contain(peer.provider.as_mut(), |p| p.reopen_on_successor()) {
                if peer.state.unsent_save {
                    unsent.push(id);
                }
                let peer_recreated = peer.provider.take_recreated_records();
                peer.state.renumber_recreated(&peer_recreated);
                recreated.extend(peer_recreated);
                peer.state.forget_session();
                recovered.push((id, peer.provider.account_privileges()));
            }
        }
        // Each window's records, read from its own file's new session.
        let owners: std::collections::BTreeMap<u32, u64> = records
            .iter()
            .chain(&current)
            .map(|&(window, _, _)| window)
            .chain(related.iter().map(|&(window, _, _)| window))
            .filter_map(|window| Some((window, io.files.owner(&self.s.file, window)?)))
            .collect();
        let tables: std::collections::BTreeMap<_, _> = std::iter::once(&self.s.file)
            .chain(io.files.peers.values().map(|peer| &peer.state))
            .flat_map(|file| std::iter::once(&file.window).chain(&file.others))
            .filter_map(|window| Some((window.id(), window.table?)))
            .collect();
        let mut read_in = |window: u32, read: &mut dyn FnMut(&mut dyn om_data::DataProvider)| {
            let Some(&file) = owners
                .get(&window)
                .filter(|file| recovered.iter().any(|(id, _)| id == *file))
            else {
                return false;
            };
            let provider = if file == own {
                io.provider.as_mut()
            } else {
                let Some(peer) = io.files.peers.get_mut(&file) else {
                    return false;
                };
                peer.provider.as_mut()
            };
            provider.use_window(window);
            read(provider);
            true
        };
        let mut records_read = Vec::new();
        for (window, layout, record) in records {
            let mut answer = None;
            if read_in(window, &mut |provider| {
                answer = Some(worker::contain(provider, |p| {
                    p.record_by_id(layout, record)
                }));
            }) {
                if let Some(answer) = answer {
                    records_read.push(((window, layout, record), answer));
                }
            }
        }
        let mut positions = Vec::new();
        for (window, layout, record) in current {
            // A new record held again is found by its new id (#152).
            let record = recreated
                .iter()
                .find(|r| {
                    r.window == window
                        && Some(&r.table) == tables.get(&window)
                        && i64::try_from(r.old).ok() == Some(record)
                })
                .and_then(|r| r.new.as_ref().ok())
                .and_then(|new| i64::try_from(*new).ok())
                .unwrap_or(record);
            let mut answer = None;
            if read_in(window, &mut |provider| {
                answer = Some(worker::contain(provider, |p| {
                    p.record_position(layout, record)
                }));
            }) {
                if let Some(answer) = answer {
                    positions.push((window, answer));
                }
            }
        }
        // Each related record a window edited, read on its file's new
        // session; a save of it answers to the version read now
        // (`retain_related_record`), as a record read back by id does.
        let mut related_read = Vec::new();
        for (window, table, record) in related {
            let mut answer = None;
            if read_in(window, &mut |provider| {
                answer = Some(worker::read_related(provider, table, record));
            }) {
                if let Some(answer) = answer {
                    related_read.push(((window, table, record), answer));
                }
            }
        }
        let provider = io.provider.as_mut();
        provider.use_window(self.s.file.window.id());
        worker::Reply::Reconnected(Box::new(Ok(worker::Reconnected {
            host_version: provider.host_application_version(),
            records: records_read,
            positions,
            recovered,
            related: related_read,
            unsent,
            recreated: recreated
                .into_iter()
                .filter_map(|record| {
                    let old = i64::try_from(record.old).ok()?;
                    let new = record
                        .new
                        .and_then(|new| {
                            i64::try_from(new)
                                .map_err(|_| DataError::new("The host gave an invalid record ID."))
                        })
                        .map_err(|error| error.message);
                    Some((record.window, record.table, old, new))
                })
                .collect(),
        })))
    }

    /// Start a script and run it until every script ended.
    fn start(&mut self, io: &mut Io<'_>, epoch: u64, ctx: egui::Context, job: ScriptJob) {
        self.s.epoch = epoch;
        self.s.ctx = ctx;
        io.route(&mut self.s, job.window.id);
        self.s.file.context = job.context;
        let fresh = !self.runtime.is_running();
        if fresh {
            self.s.adopt_windows(job.window, job.others);
            self.s.file.window.original_layout = self.s.file.window.state.layout;
            self.s.file.window.frozen = false;
            self.s.abort.store(false, Ordering::SeqCst);
            self.s.halt = false;
            self.s.interrupted = self.s.file.told_lost;
            self.s.in_run = true;
        }
        io.provider.use_window(self.s.file.window.id());
        let ScriptSession { runtime, s, .. } = self;
        let mut host = WorkerHost::new(io, s);
        host.sync_layout(fresh);
        host.send(ScriptEvent::Started);
        let message = start_in(runtime, &mut host, job.start).err();
        drop(host);
        self.run(io, message);
    }

    /// Run until no script is left.
    fn run(&mut self, io: &mut Io<'_>, mut message: Option<String>) {
        // The failure of a start on top of a paused script.
        let mut on_top = None;
        loop {
            let ScriptSession {
                runtime,
                s,
                told_timer,
            } = self;
            let mut host = WorkerHost::new(io, s);
            host.sync_layout(false);
            let state = runtime.run(&mut host);
            // The steps check as they go; this catches a loss in the last.
            host.connection_lost();
            if host.interrupted() {
                runtime.clear();
                host.finish(runtime, None);
                return;
            }
            if host.halting() {
                runtime.halt(&mut host);
            }
            let timer = host.timer_interval();
            if timer != *told_timer {
                *told_timer = timer;
                host.send(ScriptEvent::Timer(timer));
            }
            if let Some((epoch, ctx, job)) = host.next_queued() {
                drop(host);
                self.s.epoch = epoch;
                self.s.ctx = ctx;
                io.provider.use_window(self.s.file.window.id());
                let ScriptSession { runtime, s, .. } = self;
                let mut host = WorkerHost::new(io, s);
                if !runtime.is_running() {
                    host.adopt(job.window);
                }
                if let Err(failure) = start_in(runtime, &mut host, job.start) {
                    message = Some(failure);
                }
                continue;
            }
            match state {
                RunState::Idle => {
                    if let Some(trigger) = host.next_window_transaction() {
                        runtime.fire_trigger(&mut host, trigger);
                        continue;
                    }
                    host.finish(runtime, message.or(on_top));
                    return;
                }
                RunState::Stopped(stop) => {
                    host.finish(runtime, Some(stop.to_string()));
                    return;
                }
                RunState::Yielded | RunState::Waiting => {}
                RunState::Paused { seconds } => {
                    drop(host);
                    on_top = self.pause(io, seconds, on_top.take());
                }
            }
        }
    }

    /// Wait out a pause: the user works in the window (its requests are
    /// served) until Continue, Cancel, the duration, or another script.
    /// `message` is the failure of what last ran on top of the paused
    /// script; returns the failure of what runs on top now.
    fn pause(
        &mut self,
        io: &mut Io<'_>,
        seconds: Option<f64>,
        message: Option<String>,
    ) -> Option<String> {
        let deadline = seconds
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| Instant::now() + Duration::from_secs_f64(s));
        {
            let ScriptSession { s, .. } = self;
            let mut host = WorkerHost::new(io, s);
            host.reveal_window();
            host.flush();
            host.send(ScriptEvent::Paused { seconds, message });
        }
        loop {
            if self.s.connection_lost(io.provider.as_ref(), io.responses) {
                // The run loop finds the session interrupted and ends it.
                return None;
            }
            if self.s.abort.swap(false, Ordering::SeqCst) {
                self.cancel_pause(io);
                return None;
            }
            let wait = deadline.map_or(CALLBACK_POLL, |d| {
                d.saturating_duration_since(Instant::now())
                    .min(CALLBACK_POLL)
            });
            match io.jobs.recv_timeout(wait) {
                Ok(job) => match job.request {
                    Request::Answer {
                        answer: Answer::Resume(window),
                        ..
                    } => {
                        io.route(&mut self.s, window.id);
                        self.s.adopt_current(*window);
                        io.provider.use_window(self.s.file.window.id());
                        self.resume(io);
                        return None;
                    }
                    Request::Answer {
                        answer: Answer::CancelPause,
                        ..
                    } => {
                        self.cancel_pause(io);
                        return None;
                    }
                    Request::Answer { .. } => {}
                    Request::Script(script) => {
                        // A button or trigger while paused runs on top.
                        self.s.epoch = job.epoch;
                        self.s.ctx = job.ctx;
                        let ScriptSession { runtime, s, .. } = self;
                        io.route(s, script.window.id);
                        let mut host = WorkerHost::new(io, s);
                        host.adopt(script.window);
                        host.send(ScriptEvent::Started);
                        return start_in(runtime, &mut host, script.start).err();
                    }
                    Request::Close
                        if io
                            .files
                            .owner(&self.s.file, job.window)
                            .is_some_and(|file| {
                                file != self.s.file.id && !self.runtime.file_is_running(file)
                            }) =>
                    {
                        let file = io
                            .files
                            .owner(&self.s.file, job.window)
                            .expect("guarded owner");
                        let mut closed = io.files.peers.remove(&file).expect("peer owner");
                        let reply =
                            worker::handle(closed.provider.as_mut(), io.store, Request::Close);
                        self.runtime.close_file(file);
                        let _ = io.responses.send(Response {
                            epoch: job.epoch,
                            window: job.window,
                            reply,
                        });
                        job.ctx.request_repaint();
                    }
                    Request::Open { .. } | Request::OpenLocal(_) | Request::Close => {
                        self.runtime.clear();
                        self.s.deferred = Some(Job {
                            epoch: job.epoch,
                            window: job.window,
                            request: job.request,
                            ctx: job.ctx,
                        });
                        let ScriptSession { runtime, s, .. } = self;
                        let mut host = WorkerHost::new(io, s);
                        host.finish(runtime, None);
                        return None;
                    }
                    request => {
                        let file = self.s.file.id;
                        if !io.route(&mut self.s, job.window) {
                            continue;
                        }
                        self.s.observe(&request);
                        let reply =
                            worker::handle_in(io.provider.as_mut(), io.store, job.window, request);
                        // The switch back, whose panic ends the paused
                        // scripts, comes before the reply, which says
                        // whether a save outlives it (#313).
                        let back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            io.files.select(file, io.provider, &mut self.s);
                            io.provider.use_window(self.s.file.window.id());
                        }))
                        .err();
                        let reply = match back {
                            Some(_) => worker::unkept_save(io.provider.as_mut(), reply),
                            None => reply,
                        };
                        let sent = io
                            .responses
                            .send(Response {
                                epoch: job.epoch,
                                window: job.window,
                                reply,
                            })
                            .is_ok();
                        if let Some(payload) = back {
                            std::panic::resume_unwind(payload);
                        }
                        if sent {
                            job.ctx.request_repaint();
                        }
                    }
                },
                Err(RecvTimeoutError::Timeout) => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        self.resume(io);
                        return None;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.runtime.clear();
                    self.s.interrupted = true;
                    return None;
                }
            }
        }
    }

    fn resume(&mut self, io: &mut Io<'_>) {
        self.runtime.resume();
        let ScriptSession { s, .. } = self;
        let host = WorkerHost::new(io, s);
        host.send(ScriptEvent::Started);
    }

    /// Esc or Cancel while paused: halts with Allow User Abort On,
    /// otherwise continues.
    fn cancel_pause(&mut self, io: &mut Io<'_>) {
        let ScriptSession { runtime, s, .. } = self;
        let mut host = WorkerHost::new(io, s);
        if !runtime.user_abort(&mut host) {
            runtime.resume();
        }
        host.send(ScriptEvent::Started);
    }
}

/// Start `start` in the runtime; the message of a start that failed.
fn start_in(runtime: &mut Runtime, host: &mut WorkerHost<'_>, start: Start) -> Result<(), String> {
    let file = host.current_file();
    let result = match start {
        Start::Script { id } => runtime
            .perform_script(host, ScriptCall::new(file, id))
            .map_err(|code| code.to_string()),
        Start::Button {
            step,
            formula,
            literal,
            portal,
        } => {
            // A button in a portal row: that row is active first, or
            // nothing runs.
            if let Some(row) = portal {
                host.enter_clicked_row(row)?;
            }
            // The parameter calculation runs on the button's record (in its
            // portal row).
            let parameter = match &formula {
                Some(formula) => {
                    let context = host.calc_context();
                    let mut calc = runtime.calc_host(host);
                    evaluate_formula(formula, &mut calc, &context).value
                }
                None => Value::text(literal),
            };
            runtime
                .perform_step(host, *step, Some(parameter))
                .map_err(|code| code.to_string())
        }
        Start::Commit => host.save_record(),
        Start::Revert => host.revert_record(),
        Start::Trigger {
            event,
            script,
            file_reference,
            parameter,
            modes,
            action,
            info,
            prepare,
        } => {
            if let Some(token) = action {
                host.firing(event, token);
            }
            if prepare {
                host.prepare_begun_records();
            }
            let (window, mode) = (host.current_window(), host.window_mode());
            runtime.fire_trigger(
                host,
                Trigger {
                    file_reference,
                    parameter,
                    window,
                    modes,
                    mode,
                    action,
                    info: *info,
                    ..Trigger::new(event, file, script)
                },
            );
            Ok(())
        }
        Start::WindowTransaction { script, parameter } => {
            let (window, mode) = (host.current_window(), host.window_mode());
            let trigger = Trigger {
                window,
                mode,
                preset_parameter: Some(Value::text(parameter)),
                ..Trigger::new(TriggerEvent::WindowTransaction, file, script)
            };
            if runtime.is_running() {
                // The user saved while a script is paused: the runtime
                // does not fire a window event while a script runs, so it
                // waits, as a script's commits do, until every running
                // script has ended.
                host.shared().file.window_transactions.push_back(trigger);
            } else {
                runtime.fire_trigger(host, trigger);
            }
            Ok(())
        }
        Start::Timer => {
            let Some(timer) = host.timer() else {
                return Ok(());
            };
            let (window, mode) = (host.current_window(), host.window_mode());
            runtime.fire_trigger(
                host,
                Trigger {
                    parameter: timer.parameter.clone(),
                    window,
                    mode,
                    ..Trigger::new(TriggerEvent::Timer, file, timer.script)
                },
            );
            Ok(())
        }
        Start::Idle => {
            // In the window whose script scheduled it, the window this
            // session serves, with its layout and record: never whichever
            // window is current. The UI starts it only while that window
            // is open.
            // It runs once (docs/re/mbs-plugin.md 2).
            let Some(idle) = host.shared().file.idle.take() else {
                return Ok(());
            };
            let mode = host.window_mode();
            runtime.fire_trigger(
                host,
                Trigger {
                    preset_parameter: idle.parameter.map(Value::text),
                    window: idle.window,
                    mode,
                    ..Trigger::new(TriggerEvent::Timer, idle.file, idle.script)
                },
            );
            Ok(())
        }
        Start::WebViewer { window, call } => web_viewer_script(runtime, host, window, call),
        Start::Callback { id, error, result } => {
            let Some(callback) = host.take_callback(id) else {
                return Ok(());
            };
            let (window, mode) = (host.current_window(), host.window_mode());
            runtime.fire_trigger(
                host,
                Trigger {
                    parameter: callback.parameter,
                    window,
                    mode,
                    preset_error: Some(error),
                    preset_result: Some(Value::text(result)),
                    ..Trigger::new(TriggerEvent::PsosCallback, file, callback.script)
                },
            );
            Ok(())
        }
    };
    // Every UI start path (idle, queued or paused) owns a temporary host.
    // External resolution can open peers and queue their layout events here;
    // deliver them before any caller drops that host.
    loop {
        let triggers = host.take_triggers();
        if triggers.is_empty() {
            break;
        }
        for trigger in triggers {
            runtime.fire_trigger(host, trigger);
        }
    }
    result
}

/// `FileMaker.PerformScriptWithOption`: the script named `call.script` of
/// the file whose window shows the web viewer (never the file a running
/// script is in), with the page's text parameter, started as the option
/// says (options-for-starting-scripts): every option but 5 (Suspend and
/// Resume) queues the script behind the running ones; 1 halts them, 2 exits
/// the current one, 3 resumes a paused one and 4 leaves it paused first.
fn web_viewer_script(
    runtime: &mut Runtime,
    host: &mut WorkerHost<'_>,
    window: u32,
    call: crate::web_viewer::ScriptCall,
) -> Result<(), String> {
    let Some(file) = host.file_of_window(window) else {
        // The window closed (or its file did) before the call arrived.
        return Ok(());
    };
    let Some(script) = host.script_named(file, &call.script) else {
        return Err(format!(
            "The script \u{201c}{}\u{201d} that a web viewer asked to perform is missing.",
            call.script
        ));
    };
    let mode = match call.option {
        1 => QueueMode::Halt,
        2 => QueueMode::Exit,
        3 => QueueMode::Resume,
        4 => QueueMode::Pause,
        _ => QueueMode::Continue,
    };
    runtime
        .perform_script(
            host,
            ScriptCall {
                window: Some(u64::from(window)),
                parameter: Some(Value::text(call.parameter)),
                mode,
                queue_at_bottom: call.option != 5,
                ..ScriptCall::new(file, script)
            },
        )
        .map_err(|code| code.to_string())
}

impl WorkerHost<'_> {
    /// The window's layout information, read when the layout changed.
    pub(crate) fn sync_layout(&mut self, fresh: bool) {
        let layout = self.shared().file.window.state.layout;
        let render = layout.and_then(|id| self.render_layout(id));
        if fresh {
            let table = layout.and_then(|id| self.table_of(id));
            self.shared().file.window.enter_table(table);
        }
        self.shared().refresh_layout_info(render);
    }

    /// The UI's window, for a script started while none ran (or while
    /// paused).
    pub(crate) fn adopt(&mut self, window: super::WindowState) {
        self.shared().adopt_current(window);
        self.use_current_window();
        self.sync_layout(true);
    }

    pub(crate) fn interrupted(&self) -> bool {
        self.shared_ref().interrupted
    }

    pub(crate) fn halting(&mut self) -> bool {
        std::mem::take(&mut self.shared().halt)
    }

    pub(crate) fn next_queued(&mut self) -> Option<(u64, egui::Context, ScriptJob)> {
        self.shared().queued.pop_front()
    }

    pub(crate) fn next_window_transaction(&mut self) -> Option<Trigger> {
        self.shared()
            .file
            .window_transactions
            .pop_front()
            .or_else(|| {
                self.files
                    .peers
                    .values_mut()
                    .find_map(|f| f.state.window_transactions.pop_front())
            })
    }

    pub(crate) fn timer(&self) -> Option<OnTimer> {
        self.shared_ref().file.timer.clone()
    }

    pub(crate) fn timer_interval(&self) -> Option<f64> {
        self.shared_ref().file.timer.as_ref().map(|t| t.interval)
    }

    pub(crate) fn take_callback(&mut self, id: u32) -> Option<Callback> {
        self.shared().file.callbacks.remove(&id)
    }

    pub(crate) fn firing(&mut self, event: TriggerEvent, token: u64) {
        let window = self.shared().file.window.id();
        self.shared().firing.insert(token, (window, event.id()));
    }

    /// After a panic's halt ([`ScriptSession::abandon`]): a script's
    /// transaction a file's provider still holds (the halt's revert
    /// panicked, or the panic came after the runtime let go of it) is
    /// reverted once more, in the file's window. A window's record session
    /// is the window's, not the scripts': it stays. Whether none is left.
    pub(crate) fn revert_abandoned(&mut self) -> bool {
        fn held(provider: &dyn DataProvider) -> bool {
            provider.transaction_open() && !provider.record_session_open()
        }
        let start = self.shared_ref().file.window.id();
        let mut windows: Vec<u32> = self
            .files
            .peers
            .values()
            .filter(|f| held(f.provider.as_ref()))
            .map(|f| f.state.window.id())
            .collect();
        if held(self.p()) {
            windows.insert(0, start);
        }
        for &window in &windows {
            if self.use_window(u64::from(window)).is_ok() {
                let _ = Records::revert_transaction(self);
            }
        }
        if windows.iter().any(|&w| w != start) {
            let _ = self.use_window(u64::from(start));
        }
        !held(self.p()) && !self.files.peers.values().any(|f| held(f.provider.as_ref()))
    }

    /// Every script ended: the UI gets the window and the `$$` variables.
    pub(crate) fn finish(&mut self, runtime: &mut Runtime, message: Option<String>) {
        // A lost connection is told before the window (see `flush`).
        self.connection_lost();
        let s = self.shared();
        s.held.clear();
        s.firing.clear();
        let update = end_window(&mut s.file);
        // A script may finish in a different file/window from the one it
        // froze. Deliver those deferred effects to their owners as well.
        let mut thawed = Vec::new();
        thaw(&mut self.shared().file, false, &mut thawed);
        for file in self.files.peers.values_mut() {
            thaw(&mut file.state, true, &mut thawed);
        }
        for update in thawed {
            self.send_for(update.state.id, ScriptEvent::Window(Box::new(update)));
        }
        for file in std::mem::take(&mut self.shared().closed_files) {
            runtime.close_file(file);
        }
        let ids: Vec<_> = self
            .files
            .peers
            .keys()
            .copied()
            .chain(std::iter::once(self.shared_ref().file.id))
            .collect();
        for file in ids {
            self.send(ScriptEvent::Globals {
                file,
                globals: Box::new(runtime.globals(file).clone()),
            });
        }
        // What the script's commits left partly applied, in any window of
        // any file: each window freezes as the script ends, before it takes
        // any typing (#1006).
        let mut files = vec![self.shared_ref().file.id];
        let mut saves = self.p().unsettled_saves();
        for (id, file) in &mut self.files.peers {
            files.push(*id);
            saves.extend(file.provider.unsettled_saves());
        }
        self.send(ScriptEvent::Unsettled { files, saves });
        self.send(ScriptEvent::Finished {
            window: Box::new(update),
            globals: Box::new(runtime.globals(self.shared_ref().file.id).clone()),
            message,
        });
        self.shared().in_run = false;
    }
}

/// The window a run ends in, thawed, as the UI is to show it.
fn end_window(file: &mut FileState) -> super::WindowUpdate {
    file.window.frozen = false;
    let mut update = file
        .window
        .take_update(true)
        .expect("a forced update is always produced");
    update.file = file.id;
    update
}

/// The frozen windows of `file` (its current one with `include_current`)
/// thawed at a run's end: what they hold, for the UI to show behind.
fn thaw(file: &mut FileState, include_current: bool, updates: &mut Vec<super::WindowUpdate>) {
    for window in file
        .others
        .iter_mut()
        .chain(include_current.then_some(&mut file.window))
    {
        if window.frozen {
            window.frozen = false;
            if let Some(mut update) = window.take_update(true) {
                update.file = file.id;
                update.background = true;
                updates.push(update);
            }
        }
    }
}

#[cfg(test)]
#[path = "databases_tests.rs"]
mod databases_tests;

#[cfg(test)]
pub(crate) fn preview_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::preview_fixture()
}

#[cfg(test)]
pub(crate) fn title_grand_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::title_grand_fixture()
}

#[cfg(test)]
pub(crate) fn picture_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::picture_fixture()
}

#[cfg(test)]
pub(crate) fn button_group_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::button_group_fixture()
}

#[cfg(test)]
pub(crate) fn conditional_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::conditional_fixture()
}

#[cfg(test)]
pub(crate) fn grouped_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::grouped_fixture()
}

#[cfg(test)]
pub(crate) fn list_groups_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::list_groups_fixture()
}

#[cfg(test)]
pub(crate) fn sliding_fixture() -> (Box<dyn DataProvider>, impl Fn() -> Vec<String>) {
    databases_tests::sliding_fixture()
}

/// The sliding fixture's text ascent, line height and per-row lift.
#[cfg(test)]
pub(crate) fn sliding_metrics() -> (f64, f64, fn(i64) -> f64) {
    databases_tests::sliding_metrics()
}
