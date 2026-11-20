//! Perform Script on Server (script-runtime.md 11): OpenMaker Server's
//! script engine.
//!
//! An op30 request runs the named script with `om_scripts::runtime` as a
//! server client (steps a server does not support are skipped with error
//! 3, errors are captured and logged) in a server-side session of its own
//! with the requesting account and its privileges ([`ServerHost`], 9, 11.3,
//! 11.5). A request that waits gets the script's final `Get ( LastError )`
//! and Exit Script result in its reply; one that does not is answered at
//! once and, for Perform Script on Server with Callback, the result goes
//! back to the client as an op105 on its back-channel when the script
//! finishes (11.4). A schedule (`crate::schedules`) runs a script it names
//! by name the same way ([`Slot::take`], [`run`]).
//!
//! Every executing script is registered with an abort flag. At most
//! [`ScriptLimits::concurrent`] execute at a time (more are refused with
//! 812); each is aborted when it runs past its step or time limit, or when
//! the administrator, a file close or the server's shutdown aborts it
//! ([`crate::ServerHandle`]). An aborted script stops before its next step
//! as an administrator's disconnect stops it on FileMaker Server (error 1,
//! 2.3, 6.1): its open transaction and uncommitted record are reverted and
//! its locks released.

mod find;
mod host;
mod values;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use log::{info, warn};
use om_calc::eval::CalcHost;
use om_calc::{Ctx, Value};
use om_protocol::script::{
    self, PerformScript, PerformScriptReply, PsosResult, RESULT_TOO_LONG, TEXT_LIMIT,
};
use om_scripts::runtime::{ErrorCode, Options, RunState, Runtime, ScriptCall, ScriptHost, Stop};
use om_scripts::Client;

use crate::config::{Privileges, ScriptLimits, SCRIPT_CALC_LIMITS};
use crate::connection::Shared;
use crate::eventlog::Event;
use crate::hosting::Hosted;
pub(crate) use host::ServerHost;
use om_calc::eval::CalcLimits;

/// How many scripts may execute at once unless configured otherwise
/// (`sLimitOfScriptsExecuting`; OpenMaker's default, FileMaker Server's
/// own is an Admin Console setting).
pub const DEFAULT_SCRIPT_LIMIT: usize = ScriptLimits::DEFAULT.concurrent;

/// FileMaker errors of op30 itself (script-runtime.md 11.3).
const FILE_MISSING: u16 = 100;
const SCRIPT_MISSING: u16 = 104;
const WINDOW_MISSING: u16 = 112;
const HOST_CAPACITY: u16 = 812;
/// "File is being closed": a request while the server shuts down.
const CLOSING: u16 = 820;

/// Why a script was aborted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AbortReason {
    /// An administrator aborted it.
    Admin = 1,
    /// Its file is being closed.
    FileClosed = 2,
    /// The server is shutting down.
    Shutdown = 3,
    /// It executed [`ScriptLimits::steps`] steps.
    StepLimit = 4,
    /// It ran for [`ScriptLimits::duration`].
    TimeLimit = 5,
}

impl AbortReason {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => AbortReason::Admin,
            2 => AbortReason::FileClosed,
            3 => AbortReason::Shutdown,
            4 => AbortReason::StepLimit,
            5 => AbortReason::TimeLimit,
            _ => return None,
        })
    }
}

/// A script's abort flag and time limit, which its session checks before
/// every step (`IsServerAbort`, script-runtime.md 2.3) and while it pauses.
#[derive(Clone, Debug)]
pub(crate) struct Control {
    abort: Arc<AtomicU8>,
    deadline: Option<Instant>,
}

impl Control {
    /// A control nothing aborts (tests, sessions outside the engine).
    pub fn unlimited() -> Self {
        Control {
            abort: Arc::new(AtomicU8::new(0)),
            deadline: None,
        }
    }

    /// Abort the script, keeping the first reason given.
    pub fn abort(&self, reason: AbortReason) {
        let _ = self
            .abort
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Why the script was aborted, if it was; a script past its time
    /// limit is aborted here.
    pub fn aborted(&self) -> Option<AbortReason> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.abort(AbortReason::TimeLimit);
        }
        AbortReason::from_u8(self.abort.load(Ordering::Acquire))
    }
}

/// An executing script, as the administrator sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningScript {
    /// The engine's id for the script, for [`crate::ServerHandle::abort_script`].
    pub id: u64,
    /// The hosted file.
    pub file: String,
    /// The script's name (`#` and its id while its session starts).
    pub script: String,
    /// The account it runs as.
    pub account: String,
    /// How long it has run.
    pub running_for: Duration,
}

/// A registered script.
#[derive(Debug)]
struct Running {
    file: String,
    script: String,
    account: String,
    started: Instant,
    control: Control,
}

/// The script engine's shared state.
#[derive(Debug)]
pub(crate) struct ScriptEngine {
    /// Only ever copied in or out while locked: nothing panics holding
    /// it, so it is never poisoned.
    limits: Mutex<ScriptLimits>,
    /// What one calculation of a script may build, copied in and out as
    /// `limits` is, so never poisoned either.
    calc_limits: Mutex<CalcLimits>,
    running: Mutex<HashMap<u64, Running>>,
    /// Signaled when a script ends.
    ended: Condvar,
    next_id: AtomicU64,
    /// The server is shutting down: no new scripts.
    closing: AtomicBool,
    /// op105 transactions waiting for each connection's back-channel.
    calls: Mutex<HashMap<u32, Vec<Vec<u8>>>>,
    next_transaction: AtomicU32,
    /// The stack size of the threads of scripts that do not wait, if not
    /// the default (0).
    stack_size: AtomicUsize,
}

impl ScriptEngine {
    pub fn new(limits: ScriptLimits) -> Self {
        ScriptEngine {
            limits: Mutex::new(limits),
            calc_limits: Mutex::new(SCRIPT_CALC_LIMITS),
            running: Mutex::new(HashMap::new()),
            ended: Condvar::new(),
            next_id: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            calls: Mutex::new(HashMap::new()),
            next_transaction: AtomicU32::new(1),
            stack_size: AtomicUsize::new(0),
        }
    }

    /// Give the threads of scripts that do not wait a stack of `bytes`.
    pub fn set_stack_size(&self, bytes: usize) {
        self.stack_size.store(bytes, Ordering::Relaxed);
    }

    /// Change how many scripts may execute at once.
    pub fn set_limit(&self, limit: usize) {
        self.limits.lock().expect("script limits lock").concurrent = limit;
    }

    /// Change every limit.
    pub fn set_limits(&self, limits: ScriptLimits) {
        *self.limits.lock().expect("script limits lock") = limits;
    }

    pub fn limits(&self) -> ScriptLimits {
        *self.limits.lock().expect("script limits lock")
    }

    /// Change what one calculation of a script may build.
    pub fn set_calc_limits(&self, limits: CalcLimits) {
        *self.calc_limits.lock().expect("calc limits lock") = limits;
    }

    /// The scripts executing now.
    pub fn running(&self) -> Vec<RunningScript> {
        let running = self.registry();
        let mut scripts: Vec<RunningScript> = running
            .iter()
            .map(|(id, r)| RunningScript {
                id: *id,
                file: r.file.clone(),
                script: r.script.clone(),
                account: r.account.clone(),
                running_for: r.started.elapsed(),
            })
            .collect();
        scripts.sort_by_key(|s| s.id);
        scripts
    }

    /// Abort the scripts `select` picks; returns how many.
    pub fn abort(&self, reason: AbortReason, select: impl Fn(u64, &str) -> bool) -> usize {
        let running = self.registry();
        let mut count = 0;
        for (id, script) in running.iter().filter(|(id, r)| select(**id, &r.file)) {
            info!(
                "{:?}: aborting script {:?} ({id}) of {:?}: {reason:?}",
                script.file, script.script, script.account
            );
            script.control.abort(reason);
            count += 1;
        }
        count
    }

    /// The registry of executing scripts. A thread that panicked while it
    /// held the lock left the map whole (every change is one insert or
    /// remove), so a poisoned lock is used as it is: shutdown must still
    /// see and wait for the others.
    fn registry(&self) -> MutexGuard<'_, HashMap<u64, Running>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Refuse new scripts from now on. Set under the registry's lock, which
    /// [`Slot::take`] checks it under: a script either was registered
    /// before (and a following [`ScriptEngine::abort`] sees it) or is
    /// refused.
    pub fn close(&self) {
        let _running = self.registry();
        self.closing.store(true, Ordering::Release);
    }

    /// Wait until no script executes, at most `timeout`; returns whether
    /// none does. Those still executing then are logged.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let running = self.registry();
        let (running, _) = self
            .ended
            .wait_timeout_while(running, timeout, |r| !r.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        for (id, script) in running.iter() {
            warn!(
                "{:?}: script {:?} ({id}) of {:?} still executes after {timeout:?}",
                script.file, script.script, script.account
            );
        }
        running.is_empty()
    }

    /// The back-channel calls waiting for each connection, also after a
    /// thread panicked holding them: each change is one push or remove,
    /// which leaves them whole, and a session's end must still drop its
    /// own.
    pub(crate) fn queued_calls(&self) -> MutexGuard<'_, HashMap<u32, Vec<Vec<u8>>>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The back-channel calls (op105 transactions) waiting for
    /// `connection`, oldest first.
    pub fn take_calls(&self, connection: u32) -> Vec<Vec<u8>> {
        self.queued_calls().remove(&connection).unwrap_or_default()
    }

    /// Forget a connection that ended (FileMaker keeps a callback result
    /// ten minutes for a reconnect, 11.4; OpenMaker Server has no
    /// Reconnect). Its scripts that do not wait go on: FileMaker runs them
    /// to the end without the client; their limits still apply.
    pub fn forget(&self, connection: u32) {
        let lost = self.queued_calls().remove(&connection);
        if let Some(lost) = lost {
            info!(
                "dropping {} script callback(s) for closed connection 0x{connection:08x}",
                lost.len()
            );
        }
    }

    fn post(&self, connection: u32, transaction: Vec<u8>) {
        self.queued_calls()
            .entry(connection)
            .or_default()
            .push(transaction);
    }
}

/// One executing script, registered and counted against the limit until
/// dropped.
pub(crate) struct Slot {
    shared: Arc<Shared>,
    id: u64,
    control: Control,
    limits: ScriptLimits,
    calc_limits: CalcLimits,
}

impl Slot {
    /// A slot for a script of `file` run as `account` (`script` names it
    /// until its session has read its name), aborted after the server's
    /// time limit or `time_limit` if that is shorter; or the error that
    /// refuses the script: 812 at the limit
    /// (`SASEPerformScriptOnHost::StartScript`), 820 while the server shuts
    /// down.
    pub(crate) fn take(
        shared: &Arc<Shared>,
        file: &str,
        script: String,
        account: &str,
        time_limit: Option<Duration>,
    ) -> Result<Slot, u16> {
        let engine = &shared.scripts;
        let limits = engine.limits();
        let mut running = engine.registry();
        if engine.closing.load(Ordering::Acquire) {
            return Err(CLOSING);
        }
        if running.len() >= limits.concurrent {
            info!(
                "{file:?}: refusing a script: {} already executing",
                running.len()
            );
            return Err(HOST_CAPACITY);
        }
        let id = engine.next_id.fetch_add(1, Ordering::Relaxed);
        let duration = time_limit.map_or(limits.duration, |t| t.min(limits.duration));
        let control = Control {
            abort: Arc::new(AtomicU8::new(0)),
            deadline: Instant::now().checked_add(duration),
        };
        running.insert(
            id,
            Running {
                file: file.to_string(),
                script,
                account: account.to_string(),
                started: Instant::now(),
                control: control.clone(),
            },
        );
        Ok(Slot {
            shared: shared.clone(),
            id,
            control,
            limits,
            calc_limits: *engine.calc_limits.lock().expect("calc limits lock"),
        })
    }

    /// Why the script was aborted, if it was.
    pub(crate) fn aborted(&self) -> Option<AbortReason> {
        self.control.aborted()
    }

    /// The script's name, or its id until its definition was read.
    fn script_name(&self) -> String {
        self.shared
            .scripts
            .registry()
            .get(&self.id)
            .map(|entry| entry.script.clone())
            .unwrap_or_default()
    }

    /// Name the script in the registry once its definition is known.
    fn name(&self, name: &str) {
        if let Some(entry) = self.shared.scripts.registry().get_mut(&self.id) {
            entry.script = name.to_string();
        }
    }
}

/// Unregisters the script, also when its thread unwinds from a panic, so
/// that the limit and shutdown's wait never count a script that is gone.
impl Drop for Slot {
    fn drop(&mut self) {
        let engine = &self.shared.scripts;
        engine.registry().remove(&self.id);
        engine.ended.notify_all();
    }
}

/// An op30 request with what the requesting session knows.
pub(crate) struct Request {
    pub hosted: Arc<Hosted>,
    /// The account the client logged in with; the script session uses it.
    pub account: String,
    /// What the account may do.
    pub privileges: Privileges,
    /// The requesting connection, which gets the op105.
    pub requester: u32,
    pub script: PerformScript,
}

/// How a server script ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// The runtime's `Get ( LastError )` at the end of the script.
    pub error: u16,
    /// The top script's Exit Script result as text.
    pub result: String,
}

impl Outcome {
    fn failed(error: u16) -> Self {
        Outcome {
            error,
            result: String::new(),
        }
    }
}

/// The op30 reply when the file is not open in the requesting session.
pub(crate) fn not_open() -> PerformScriptReply {
    PerformScriptReply {
        error: FILE_MISSING,
        ..PerformScriptReply::default()
    }
}

/// Answer an op30 request: run the script and reply with its outcome, or
/// start it on its own thread and reply at once (11.3); 812, as past the
/// script limit, when no thread can be started for it.
pub(crate) fn perform(shared: &Arc<Shared>, request: Request) -> PerformScriptReply {
    // The id until the session has read the script's name.
    let label = om_protocol::script::script_id(&request.script.script)
        .map_or_else(String::new, |id| format!("#{id}"));
    let slot = match Slot::take(shared, &request.hosted.name, label, &request.account, None) {
        Ok(slot) => slot,
        Err(error) => {
            return PerformScriptReply {
                error,
                ..PerformScriptReply::default()
            }
        }
    };
    if request.script.waits() {
        let outcome = run_request(shared, &request, &slot);
        drop(slot);
        return PerformScriptReply {
            error: outcome.error,
            result: outcome.result,
            extended_error: String::new(),
        };
    }
    let file = request.hosted.name.clone();
    let mut thread = std::thread::Builder::new();
    let stack_size = shared.scripts.stack_size.load(Ordering::Relaxed);
    if stack_size > 0 {
        thread = thread.stack_size(stack_size);
    }
    let shared = shared.clone();
    let started = thread.spawn(move || {
        // Locals, dropped in reverse order (a closure's captures are dropped
        // in no set order), also as a panic unwinds: the request, and the
        // file it holds, go before the server's state, for which alone
        // closing a local file waits (`LocalHost::close`).
        let shared = shared;
        let request = request;
        let slot = slot;
        let outcome = run_request(&shared, &request, &slot);
        drop(slot);
        if let Some(ids) = request.script.callback.filter(|ids| ids.wants_callback()) {
            callback(&shared, &request, ids.client, outcome);
        }
    });
    if let Err(e) = started {
        // The request and its slot went with the closure: the slot is
        // free again, and a callback request gets no op105, as past the
        // limit. The host has no capacity for the script: 812.
        warn!("{file:?}: refusing a script: no thread to run it: {e}");
        return PerformScriptReply {
            error: HOST_CAPACITY,
            ..PerformScriptReply::default()
        };
    }
    PerformScriptReply::default()
}

/// Queue the op105 that tells the requester its callback script's result
/// (11.4).
fn callback(shared: &Shared, request: &Request, client: u32, outcome: Outcome) {
    let (error, result) = if outcome.result.chars().count() > TEXT_LIMIT {
        (RESULT_TOO_LONG, String::new())
    } else {
        (outcome.error, outcome.result)
    };
    let transaction = script::build_psos_result(
        shared
            .scripts
            .next_transaction
            .fetch_add(1, Ordering::Relaxed),
        request.hosted.id,
        &PsosResult {
            callback: client,
            error,
            result,
            extended_error: String::new(),
        },
    );
    if !shared.connection_alive(request.requester) {
        warn!(
            "{:?}: the client that asked for script callback {client} is gone",
            request.hosted.name
        );
        return;
    }
    shared.scripts.post(request.requester, transaction);
}

/// The script a session runs.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ScriptRef<'a> {
    /// By its catalog key, as op30 names it.
    Key(&'a [u8]),
    /// By its name, as a schedule names it: matched without case, the
    /// first in catalog order (script oracle s36).
    Name(&'a str),
}

impl std::fmt::Display for ScriptRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptRef::Key(key) => write!(f, "{key:02x?}"),
            ScriptRef::Name(name) => write!(f, "{name:?}"),
        }
    }
}

/// Run one request's script in a session of its own; an abort is recorded
/// in the event log.
fn run_request(shared: &Arc<Shared>, request: &Request, slot: &Slot) -> Outcome {
    let outcome = run(
        shared,
        &request.hosted,
        &request.account,
        request.privileges,
        ScriptRef::Key(&request.script.script),
        &request.script.parameter,
        slot,
    );
    if let Some(reason) = slot.aborted() {
        shared.events.record(Event::script_aborted(
            &request.hosted.name,
            &slot.script_name(),
            &request.account,
            reason,
            outcome.error,
        ));
    }
    outcome
}

/// Run `script` of `hosted` with `parameter` in a session of its own, as
/// `account` with `privileges`, under `slot`'s abort flag and limits.
pub(crate) fn run(
    shared: &Arc<Shared>,
    hosted: &Arc<Hosted>,
    account: &str,
    privileges: Privileges,
    script: ScriptRef<'_>,
    parameter: &str,
    slot: &Slot,
) -> Outcome {
    let session = SessionId(shared, shared.new_connection_id());
    let mut host = ServerHost::new(
        shared.clone(),
        hosted.clone(),
        session.1,
        account,
        privileges,
    );
    host.set_control(slot.control.clone());
    host.set_calc_limits(slot.calc_limits);
    let outcome = execute(&mut host, script, parameter, slot);
    host.close();
    // (The session's locks go with it, before another can get its id.)
    drop(host);
    drop(session);
    match slot.control.aborted() {
        Some(reason) => warn!(
            "{:?}: script {script} for {account:?} was aborted ({reason:?}) with error {}",
            hosted.name, outcome.error
        ),
        None => info!(
            "{:?}: script {script} for {account:?} ended with error {}",
            hosted.name, outcome.error
        ),
    }
    outcome
}

/// A script session's connection id, released when it is dropped: also
/// when the script's thread unwinds from a panic, after the session's
/// [`ServerHost`] (declared after it, so dropped first) has released the
/// session's locks.
struct SessionId<'a>(&'a Shared, u32);

impl Drop for SessionId<'_> {
    fn drop(&mut self) {
        self.0.release_connection_id(self.1);
    }
}

/// `SMScriptRuntime::ExecuteScript` (11.3): 112 without a window, 104 for a
/// missing script, else the script run to completion or until it is
/// aborted.
fn execute(host: &mut ServerHost, script: ScriptRef<'_>, parameter: &str, slot: &Slot) -> Outcome {
    if !host.has_window() {
        return Outcome::failed(WINDOW_MISSING);
    }
    let id = match script {
        ScriptRef::Key(key) => om_protocol::script::script_id(key),
        ScriptRef::Name(name) => host.script_named(0, name),
    };
    let Some(id) = id else {
        return Outcome::failed(SCRIPT_MISSING);
    };
    let Some(script) = host.script(0, id) else {
        return Outcome::failed(SCRIPT_MISSING);
    };
    slot.name(&script.name);
    let mut options = Options::new(Client::Server);
    options.step_budget = Some(slot.limits.steps);
    let mut runtime = Runtime::new(options);
    let call = ScriptCall {
        parameter: Some(Value::text(parameter)),
        ..ScriptCall::new(0, id)
    };
    if let Err(code) = runtime.perform_script(host, call) {
        return Outcome::failed(error_code(code));
    }
    loop {
        match runtime.run(host) {
            RunState::Idle => break,
            RunState::Stopped(Stop::Budget(_)) => {
                // The runtime halted (reverting the transaction); the rest
                // is an abort's.
                slot.control.abort(AbortReason::StepLimit);
                break;
            }
            RunState::Stopped(_) => break,
            // The server host waits out pauses itself and never leaves a
            // step pending; neither state should come back.
            RunState::Paused { .. } => runtime.resume(),
            RunState::Waiting => runtime.complete_step(ErrorCode::UNAVAILABLE),
            RunState::Yielded => {}
        }
    }
    let cx = Ctx::new(host.locale(), host.today());
    // `Get ( LastError )`, or 4 when an unknown step aborted the script
    // (script-runtime.md 13.7).
    let mut error = runtime.final_error();
    if host.closed_last_window() {
        // The script stopped when it closed its last window, and FileMaker
        // Server's run ends with 0 (s35-windows "close last": the step
        // returned 13, the script's LastError and result were 0 and empty).
        error = ErrorCode::NONE;
    }
    if slot.control.aborted().is_some() && !error.is_error() {
        // An abort is error 1, which never hides an earlier error
        // (script-runtime.md 4.2).
        error = ErrorCode::CANCELED;
    }
    Outcome {
        error: error_code(error),
        result: runtime
            .final_result()
            .map(|v| v.to_text(&cx).into_string())
            .unwrap_or_default(),
    }
}

/// A runtime error as op30's u16 (custom codes 5000-5499 included).
fn error_code(code: ErrorCode) -> u16 {
    code.0 as u16
}

#[cfg(test)]
#[path = "../../tests/fixtures/tasks.rs"]
pub(crate) mod fixture;
#[cfg(test)]
mod oracle_tests;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use om_calc::eval::CalcHost;
    use om_calc::{Ctx, Value, ValueType};
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, RidList};
    use om_protocol::script::{script_key, CallbackIds, PerformScript, ASYNC};
    use om_protocol::write::attention;
    use om_scripts::runtime::{
        FieldRef, Fields, FindKind, FoundSets, HostError, Mode, Navigation, Records, Stop,
    };
    use ring::rand::SystemRandom;

    use super::fixture::{self, TempFile};
    use super::*;
    use crate::config::{HostedFile, ServerIdentity};
    use crate::hosting::{Hosting, Registration};
    use crate::locks::LockUser;

    const CLIENT: u32 = 0x77;

    pub(super) fn shared(file: &TempFile) -> Arc<Shared> {
        let hosting = Hosting::load(
            &[HostedFile {
                name: fixture::FILE.into(),
                path: file.0.clone(),
            }],
            false,
            &Default::default(),
        )
        .unwrap();
        let shared = Arc::new(Shared {
            identity: ServerIdentity {
                name: "openmaker-test".into(),
                version_string: "OpenMaker Server test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            hosting,
            local: false,
            login_limits: Default::default(),
            custom_sets: Default::default(),
            rng: SystemRandom::new(),
            connection_ids: Default::default(),
            recordings: None,
            mailboxes: Default::default(),
            scripts: ScriptEngine::new(ScriptLimits::DEFAULT),
            refused_downloads: Default::default(),
            omitted_list_paths: Default::default(),
            download_overrides: Default::default(),
            shutdown: Default::default(),
            late_index_builds: Default::default(),
            unterminated_query_replies: Default::default(),
            started: std::time::SystemTime::now(),
            block_new_users: Default::default(),
            clients: Default::default(),
            events: Default::default(),
            open_delays: Default::default(),
            sockets: Default::default(),
            listed: Default::default(),
            file_list_override: Default::default(),
            no_file_list: Default::default(),
            no_get_file_list: Default::default(),
            no_authoring: Default::default(),
            upload_memory: crate::uploads::UploadMemory::new(usize::MAX),
            download_memory: crate::transfer::DownloadMemory::new(usize::MAX),
            #[cfg(debug_assertions)]
            test_panic: Default::default(),
            #[cfg(debug_assertions)]
            test_refusal: Default::default(),
            #[cfg(debug_assertions)]
            script_gate: Default::default(),
            #[cfg(debug_assertions)]
            failing_timestamps: Default::default(),
        });
        // A client registered for the file's notifications.
        shared.connection_ids.lock().unwrap().insert(CLIENT);
        hosted(&shared)
            .registrations
            .lock()
            .unwrap()
            .push(Registration {
                connection: CLIENT,
                address: 1,
                mask: u32::MAX,
                client_file: -4,
            });
        shared
    }

    pub(super) fn hosted(shared: &Shared) -> Arc<Hosted> {
        shared.hosting.by_name(fixture::FILE).unwrap().clone()
    }

    fn request(shared: &Shared, script: u64, parameter: &str, flags: u16) -> Request {
        Request {
            hosted: hosted(shared),
            account: fixture::ACCOUNT.into(),
            privileges: Privileges::FullAccess,
            requester: CLIENT,
            script: PerformScript {
                flags,
                script: script_key(script).unwrap(),
                parameter: parameter.into(),
                callback: Some(CallbackIds::default()),
            },
        }
    }

    /// A stored cell of a record as text.
    fn stored(shared: &Shared, record: u64, field: u64) -> Option<String> {
        let hosted = hosted(shared);
        let store = hosted.read_store();
        let (t, r, f) = (
            uint_segment(fixture::TABLE),
            serial::encode(record),
            uint_segment(field),
        );
        store.node(&[&t, &[5], &r, &f]).and_then(values::cell_text)
    }

    fn record_ids(shared: &Shared) -> Vec<u64> {
        let hosted = hosted(shared);
        let store = hosted.read_store();
        let t = uint_segment(fixture::TABLE);
        RidList::from_owned(store.node(&[&t, &[0x0d]]).unwrap())
            .unwrap()
            .iter()
            .collect()
    }

    /// The review's P7: when Manage Database deletes the window's layout
    /// while a script runs, the window moves to another layout (OpenMaker's
    /// choice: the first of its table, else the file's first, with that
    /// table's records), instead of keeping an index into the new list that
    /// names another layout or none.
    #[test]
    fn the_window_leaves_a_layout_that_was_deleted() {
        let file = TempFile::new("deleted-layout", &[], true);
        let shared = shared(&file);
        let mut host = session(&shared);
        let get = |host: &mut ServerHost, selector: u8| {
            let value = CalcHost::get(host, selector, &om_calc::eval::Context::new()).unwrap();
            value
                .to_text(&Ctx::new(host.locale(), host.today()))
                .into_string()
        };
        host.go_to_layout(&om_scripts::runtime::LayoutTarget::Key(2), None)
            .unwrap();
        assert_eq!(get(&mut host, 10), "Notes");
        // Manage Database replaces the layout catalog without layout 2.
        {
            let hosted = hosted(&shared);
            let mut store = hosted.write_store();
            let mut layouts = store.node(&[&[4], &[5]]).unwrap().clone();
            layouts.remove(&[&[2]]);
            store.put_subtree(&[vec![4], vec![5]], layouts);
        }
        host.refresh_schema();
        // Layout name, number, table, found count.
        let shown: Vec<String> = [10, 11, 61, 7].iter().map(|s| get(&mut host, *s)).collect();
        assert_eq!(shown, ["Tasks", "1", "Tasks", "2"]);
        host.close();
    }

    /// The review's P8: a request that panics holding the store does not
    /// take the file down for every later request (its lock is poisoned,
    /// and every `expect` on it panicked), and a commit that panics puts
    /// back what it had written first.
    #[test]
    fn a_panic_holding_the_store_leaves_it_usable_and_unchanged() {
        let file = TempFile::new("poisoned-store", &[], true);
        let shared = shared(&file);
        let hosted = hosted(&shared);
        let t = uint_segment(fixture::TABLE);
        let panicked = {
            let hosted = hosted.clone();
            let t = t.clone();
            std::thread::spawn(move || {
                let mut store = hosted.write_store();
                let mut journal = store.begin_commit();
                store.guarded(&mut journal, |store, journal| {
                    store.write_record_in(journal, &t, 1, None);
                    panic!("a bug in the middle of a commit");
                });
            })
            .join()
        };
        assert!(panicked.is_err() && hosted.store_is_poisoned());
        {
            let store = hosted.read_store();
            assert!(store.has_record(&t, 1));
            let ids = store.node(&[&t, &[0x0d]]).unwrap();
            assert!(RidList::from_owned(ids).unwrap().contains(1));
        }
        // Later requests still read and write the file.
        let mut host = session(&shared);
        host.go_to_record(om_scripts::runtime::RecordMove::First, false)
            .unwrap();
        host.set_field(field(fixture::NAME), Value::text("After the panic"))
            .unwrap();
        host.commit_records(Default::default()).unwrap();
        host.close();
        let store = hosted.read_store();
        let name = store
            .node(&[&t, &[5], &serial::encode(1), &uint_segment(fixture::NAME)])
            .and_then(|n| om_hbam::Cell::from_owned(n).text())
            .and_then(Result::ok);
        assert_eq!(name.as_deref(), Some("After the panic"));
    }

    pub(super) fn session(shared: &Arc<Shared>) -> ServerHost {
        ServerHost::new(
            shared.clone(),
            hosted(shared),
            0x55,
            fixture::ACCOUNT,
            Privileges::FullAccess,
        )
    }

    fn field(id: u64) -> FieldRef {
        FieldRef {
            occurrence: fixture::OCCURRENCE,
            field: id,
            repetition: 1,
        }
    }

    fn text(host: &ServerHost, value: &Value) -> String {
        value
            .to_text(&Ctx::new(host.locale(), host.today()))
            .into_string()
    }

    #[test]
    fn a_waiting_request_edits_records_and_returns_the_result() {
        let file = TempFile::new("edits", &[fixture::server_work()], true);
        let shared = shared(&file);
        let reply = perform(&shared, request(&shared, 1, "unit", 0));
        assert_eq!(reply.error, 0, "{reply:?}");
        let parts: Vec<&str> = reply.result.split('|').collect();
        assert_eq!(parts[..4], ["3", "3", "From unit", "Admin"]);
        assert!(parts[4].starts_with("Server "), "{reply:?}");

        // The file: record 1 changed, record 3 created with its values.
        assert_eq!(stored(&shared, 1, fixture::COUNT).as_deref(), Some("13"));
        assert_eq!(
            stored(&shared, 3, fixture::NAME).as_deref(),
            Some("From unit")
        );
        assert_eq!(
            stored(&shared, 3, fixture::DUE).as_deref(),
            Some("10/28/2026")
        );
        assert_eq!(record_ids(&shared), [1, 2, 3]);
        // Written back to disk, as every commit is.
        let reopened = om_hbam::HbamFile::open(&file.0).unwrap();
        assert_eq!(reopened.records(fixture::TABLE).unwrap().len(), 3);

        // The registered client is told of both commits, as for a client's.
        let codes: Vec<u32> = shared
            .mailboxes
            .take(CLIENT, usize::MAX)
            .iter()
            .map(|n| n.code)
            .collect();
        assert_eq!(
            codes,
            [
                attention::MODIFY | attention::RECORD,
                attention::ADD | attention::RECORD
            ]
        );
        // Every lock of the session is released, and its id with it.
        assert!(!hosted(&shared).locks.lock().unwrap().held_by_other(
            &[uint_segment(fixture::TABLE), vec![5], serial::encode(1)],
            LockUser {
                connection: CLIENT,
                txn: 1
            }
        ));
        assert_eq!(shared.connection_ids.lock().unwrap().len(), 1);
    }

    #[test]
    fn request_errors_are_filemakers() {
        let file = TempFile::new(
            "errors",
            &[fixture::server_work(), fixture::failing()],
            true,
        );
        let shared = shared(&file);
        // 104: no such script.
        assert_eq!(perform(&shared, request(&shared, 9, "", 0)).error, 104);
        // The script's own last error comes back with its result.
        let failed = perform(&shared, request(&shared, 2, "", 0));
        assert_eq!((failed.error, failed.result.as_str()), (201, "failed"));
        // 100: not open in the session.
        assert_eq!(not_open().error, 100);
        // 812: at the limit nothing runs.
        shared.scripts.set_limit(0);
        assert_eq!(perform(&shared, request(&shared, 1, "", 0)).error, 812);
        assert_eq!(perform(&shared, request(&shared, 1, "", ASYNC)).error, 812);
        assert_eq!(stored(&shared, 1, fixture::COUNT).as_deref(), Some("3"));

        // 112: a file without a layout gives the session no window.
        let bare = TempFile::new("bare", &[fixture::server_work()], false);
        let shared = super::tests::shared(&bare);
        assert_eq!(perform(&shared, request(&shared, 1, "", 0)).error, 112);
    }

    #[test]
    fn the_limit_counts_executing_scripts() {
        let file = TempFile::new("limit", &[fixture::slow(1.5), fixture::server_work()], true);
        let shared = shared(&file);
        shared.scripts.set_limit(1);
        let mut slow = request(&shared, 3, "", ASYNC);
        slow.script.callback = Some(CallbackIds {
            file_user: 0x1234,
            client: 9,
        });
        // Not waiting: answered at once, while the script runs.
        let started = Instant::now();
        assert_eq!(perform(&shared, slow), PerformScriptReply::default());
        // (The script pauses 1.5 s.)
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(perform(&shared, request(&shared, 1, "", 0)).error, 812);
        // The callback result arrives on the requester's back-channel.
        let deadline = Instant::now() + Duration::from_secs(10);
        let call = loop {
            if let Some(call) = shared.scripts.take_calls(CLIENT).pop() {
                break call;
            }
            assert!(Instant::now() < deadline, "no callback");
            std::thread::sleep(Duration::from_millis(20));
        };
        let result = script::parse_psos_result(&call).unwrap();
        assert_eq!(
            (result.callback, result.error, result.result.as_str()),
            (9, 0, "slow done")
        );
        assert_eq!(call[0], 105);
        // The slot is free again.
        assert_eq!(perform(&shared, request(&shared, 1, "", 0)).error, 0);
    }

    #[test]
    fn stored_values_decode_per_field_type() {
        let file = TempFile::new("values", &[], true);
        let shared = shared(&file);
        let mut host = session(&shared);
        let value = |host: &ServerHost, record, id| {
            host.field_value(fixture::OCCURRENCE, record, id, 1)
                .unwrap()
        };
        let name = value(&host, 1, fixture::NAME);
        assert_eq!(name.value_type(), ValueType::Text);
        assert_eq!(text(&host, &name), "Write tests");
        let count = value(&host, 1, fixture::COUNT);
        assert_eq!(count.value_type(), ValueType::Number);
        let cx = Ctx::new(host.locale(), host.today());
        assert_eq!(count.to_number(&cx).to_i64(), Some(3));
        let due = value(&host, 1, fixture::DUE);
        assert_eq!(
            due.to_date(&cx).unwrap(),
            Some(om_calc::Date::from_ymd(2026, 10, 1).unwrap())
        );
        // Not stored: the type's empty value.
        let empty = value(&host, 2, fixture::DUE);
        assert!(empty.is_empty() && empty.value_type() == ValueType::Date);
        assert!(host.field(fixture::OCCURRENCE, 99).is_none());

        // A global field is the session's own, set without a record lock.
        host.set_field(field(fixture::SETTING), Value::text("on"))
            .unwrap();
        assert_eq!(text(&host, &value(&host, 2, fixture::SETTING)), "on");
        assert_eq!(stored(&shared, 1, fixture::SETTING), None);
        // A calculation cannot be set; a repetition must exist.
        let calc = host.set_field(field(fixture::DOUBLED), Value::number(1));
        assert_eq!(calc, Err(HostError::Error(ErrorCode(201))));
        let rep = host.set_field(
            FieldRef {
                repetition: 2,
                ..field(fixture::NAME)
            },
            Value::number(1),
        );
        assert_eq!(rep, Err(HostError::Error(ErrorCode(111))));
        host.close();
    }

    #[test]
    fn get_selectors_describe_the_session_and_the_server() {
        let file = TempFile::new("selectors", &[], true);
        let shared = shared(&file);
        let mut host = session(&shared);
        let context = host.calc_context_for_test();
        let mut get = |selector| {
            let value = CalcHost::get(&mut host, selector, &context).unwrap();
            value
                .to_text(&Ctx::new(
                    &om_calc::Locale::en_us(),
                    om_calc::Date::from_ymd(2026, 1, 1).unwrap(),
                ))
                .into_string()
        };
        assert_eq!(get(7), "2"); // FoundCount
        assert_eq!(get(19), "2"); // TotalRecordCount
        assert_eq!(get(20), "1"); // RecordNumber
        assert_eq!(get(37), "1"); // RecordID
        assert_eq!(get(10), "Tasks"); // LayoutName
        assert_eq!(get(11), "1"); // LayoutNumber
        assert_eq!(get(61), "Tasks"); // LayoutTableName
        assert_eq!(get(5), "Tasks"); // FileName
        assert_eq!(get(32), "Admin"); // AccountName
        assert_eq!(get(31), "Admin"); // UserName: the account on the server
        assert_eq!(get(8), "openmaker-test"); // HostName
        assert_eq!(get(13), "0"); // WindowMode: Browse
        assert_eq!(get(79), "0"); // RecordOpenState
        assert!(get(1).starts_with("Server ")); // ApplicationVersion
        assert!(get(40).starts_with("file:/")); // FilePath
        let uuid = get(101);
        assert_eq!((uuid.len(), &uuid[14..15]), (36, "4"));
        // WindowName: the file's name, as FileMaker Server's engine answers.
        assert_eq!(get(48), "Tasks");
    }

    #[test]
    fn finds_run_through_the_query_engine() {
        let file = TempFile::new("finds", &[], true);
        let shared = shared(&file);
        let mut host = session(&shared);
        let find = |host: &mut ServerHost, criteria: &[(u64, &str)], kind| {
            host.enter_mode(Mode::Find, false, None).unwrap();
            for (id, text) in criteria {
                host.set_field(field(*id), Value::text(*text)).unwrap();
            }
            let outcome = host.perform_find(kind, None);
            let found: Vec<u64> = (0..)
                .map_while(|i| host.found_record(fixture::OCCURRENCE, i).unwrap())
                .collect();
            (outcome, found)
        };
        assert_eq!(
            find(&mut host, &[(fixture::NAME, "shi")], FindKind::Replace),
            (Ok(()), vec![2])
        );
        assert_eq!(
            find(
                &mut host,
                &[(fixture::NAME, "==write tests")],
                FindKind::Replace
            ),
            (Ok(()), vec![1])
        );
        assert_eq!(
            find(&mut host, &[(fixture::COUNT, ">4")], FindKind::Replace),
            (Ok(()), vec![2])
        );
        assert_eq!(
            find(&mut host, &[(fixture::COUNT, "1...3")], FindKind::Replace),
            (Ok(()), vec![1])
        );
        assert_eq!(
            find(&mut host, &[(fixture::DUE, "=")], FindKind::Replace),
            (Ok(()), vec![2])
        );
        // Extend adds to the found set of the last find, {2}.
        assert_eq!(
            find(&mut host, &[(fixture::COUNT, "3")], FindKind::Extend),
            (Ok(()), vec![1, 2])
        );
        assert_eq!(
            find(&mut host, &[(fixture::NAME, "write")], FindKind::Constrain),
            (Ok(()), vec![1])
        );
        // No match: 401 and an empty found set; no criteria: 400.
        let none = find(&mut host, &[(fixture::NAME, "nothing")], FindKind::Replace);
        assert_eq!(none, (Err(HostError::Error(ErrorCode(401))), vec![]));
        assert_eq!(
            find(&mut host, &[], FindKind::Replace).0,
            Err(HostError::Error(ErrorCode(400)))
        );
        host.show_all_records().unwrap();
        assert_eq!(host.found_record(fixture::OCCURRENCE, 1).unwrap(), Some(2));
        host.close();
    }

    #[test]
    fn a_runaway_script_keeps_a_bounded_error_log_and_reverts() {
        let file = TempFile::new("log", &[fixture::runaway(4)], true);
        let shared = shared(&file);
        let mut host = session(&shared);
        let mut options = Options::new(Client::Server);
        options.step_budget = Some(3_000);
        let mut runtime = Runtime::new(options);
        runtime
            .perform_script(&mut host, ScriptCall::new(0, 4))
            .unwrap();
        assert_eq!(
            runtime.run(&mut host),
            RunState::Stopped(Stop::Budget(3_000))
        );
        // About one failing Set Field in three steps: all counted, the
        // last 64 kept.
        assert!(host.log.total() > 900, "{}", host.log.total());
        assert_eq!(host.log.entries().count(), 64);
        assert!(host.log.entries().all(|e| e.code == ErrorCode(201)));
        host.close();
        // The halt reverted the transaction and released record 1.
        assert_eq!(
            stored(&shared, 1, fixture::NAME).as_deref(),
            Some("Write tests")
        );
        assert!(!hosted(&shared).locks.lock().unwrap().held_by_other(
            &[uint_segment(fixture::TABLE), vec![5], serial::encode(1)],
            LockUser {
                connection: CLIENT,
                txn: 1
            }
        ));
    }

    #[test]
    fn unique_values_count_the_transactions_records() {
        // Code: unique, validated Always, with an English value index.
        let mut tree = fixture::tree(&[], true);
        fixture::set_field_options(&mut tree, fixture::TABLE, fixture::CODE, 0x8000_0000);
        fixture::set_field_validation(&mut tree, fixture::TABLE, fixture::CODE, 0x410);
        let file = TempFile::from_tree("unique", &tree);
        let shared = shared(&file);
        let mut host = session(&shared);
        let first = om_scripts::runtime::RecordMove::First;
        let last = om_scripts::runtime::RecordMove::Last;
        host.set_field(field(fixture::CODE), Value::text("A"))
            .unwrap();
        host.commit_records(Default::default()).unwrap();
        host.go_to_record(last, false).unwrap();
        host.set_field(field(fixture::CODE), Value::text("a"))
            .unwrap();
        // 504: record 1 holds it (keys compare without case).
        let refused = host.commit_records(Default::default());
        assert_eq!(refused, Err(HostError::Error(ErrorCode(504))));
        host.revert_record(false).unwrap();

        // In a transaction: record 1 takes "B" and record 2 frees "A"; the
        // file's index alone would say otherwise for both.
        host.begin_transaction(Default::default()).unwrap();
        host.go_to_record(first, false).unwrap();
        host.set_field(field(fixture::CODE), Value::text("B"))
            .unwrap();
        host.go_to_record(last, false).unwrap();
        host.set_field(field(fixture::CODE), Value::text("A"))
            .unwrap();
        host.commit_transaction(false).unwrap();
        assert_eq!(stored(&shared, 1, fixture::CODE).as_deref(), Some("B"));
        assert_eq!(stored(&shared, 2, fixture::CODE).as_deref(), Some("A"));
        host.begin_transaction(Default::default()).unwrap();
        host.set_field(field(fixture::CODE), Value::text("C"))
            .unwrap();
        host.go_to_record(first, false).unwrap();
        host.set_field(field(fixture::CODE), Value::text("C"))
            .unwrap();
        assert_eq!(
            host.commit_transaction(false),
            Err(HostError::Error(ErrorCode(504)))
        );
        // Nothing of the failed transaction was written.
        assert_eq!(stored(&shared, 1, fixture::CODE).as_deref(), Some("B"));
        assert_eq!(stored(&shared, 2, fixture::CODE).as_deref(), Some("A"));
        host.close();
    }

    #[test]
    fn indexes_key_text_in_their_fields_language() {
        // Code indexed in Unicode raw (2), unique and validated Always, as
        // PrimaryKey is in FileMaker Pro 26's blank file (#725); then in 0,
        // the file's own language, English here (query-program.md 7.2).
        use om_scripts::runtime::RecordMove::{First, Last};
        for (language, keys) in [
            (2, vec![vec![1, 0, b'A'], vec![1, 0, b'a']]),
            (0, vec![om_collate::text_index_key("a")]),
        ] {
            let mut tree = fixture::tree(&[], true);
            let (table, code) = (fixture::TABLE, fixture::CODE);
            fixture::set_field_options(&mut tree, table, code, 0x8000_0000);
            fixture::set_field_language(&mut tree, table, code, language);
            if language == 2 {
                fixture::set_field_validation(&mut tree, table, code, 0x410);
            }
            let (t, f) = (uint_segment(table), uint_segment(code));
            let index: [&[u8]; 4] = [&t, &[1], &f, &[0]];
            tree.put(&index, om_hbam::OwnedNode::list(vec![0], Vec::new()));
            let file = TempFile::from_tree(&format!("keys-{language}"), &tree);
            let shared = shared(&file);
            let mut host = session(&shared);
            for (record, text) in [(First, "A"), (Last, "a")] {
                host.go_to_record(record, false).unwrap();
                host.set_field(field(code), Value::text(text)).unwrap();
                host.commit_records(Default::default()).unwrap();
            }
            let held: Vec<Vec<u8>> = hosted(&shared)
                .read_store()
                .node(&index)
                .map(|n| n.children.iter().map(|k| k.segment.clone()).collect())
                .unwrap_or_default();
            assert_eq!(held, keys, "language {language}");
            if language == 2 {
                // Raw keys keep case: "a" was another value, "A" is not.
                host.set_field(field(code), Value::text("A")).unwrap();
                let refused = host.commit_records(Default::default());
                assert_eq!(refused, Err(HostError::Error(ErrorCode(504))));
            }
            host.close();
        }
    }

    #[test]
    fn unknown_privileges_fail_closed_unless_elevated() {
        let file = TempFile::new("privileges", &[], true);
        let shared = shared(&file);
        let mut host = ServerHost::new(
            shared.clone(),
            hosted(&shared),
            0x56,
            fixture::CLERK,
            Privileges::Unknown,
        );
        let denied = Err(HostError::Error(ErrorCode::INSUFFICIENT_PRIVILEGES));
        assert_eq!(
            host.set_field(field(fixture::NAME), Value::text("x")),
            denied
        );
        assert_eq!(host.new_record(), denied);
        assert_eq!(host.delete_record(false), denied);
        assert_eq!(host.truncate_table(None, false), denied);
        assert_eq!(
            host.set_field(field(fixture::SETTING), Value::text("x")),
            denied
        );
        let read = CalcHost::field_value(&host, fixture::OCCURRENCE, 1, fixture::NAME, 1);
        assert_eq!(read.unwrap_err().code(), 9);
        let query = om_calc::eval::evaluate_text(
            "ExecuteSQL ( \"SELECT Name FROM Tasks\" ; \"\" ; \"\" )",
            &mut host,
            &om_calc::eval::Context::new(),
        );
        assert_eq!(query.code(), 9, "{:?}", query.value);
        host.enter_mode(Mode::Find, false, None).unwrap();
        host.set_field(field(fixture::NAME), Value::text("Ship"))
            .unwrap();
        assert_eq!(host.perform_find(FindKind::Replace, None), denied);
        host.enter_mode(Mode::Browse, false, None).unwrap();
        // Scripts: only those run with full access privileges.
        assert_eq!(
            host.script_access(0, 1),
            om_scripts::runtime::ScriptAccess {
                executable: false,
                full_access: false
            }
        );
        // A full-access script's step may write.
        host.set_full_access(0, true);
        host.set_field(field(fixture::NAME), Value::text("Elevated"))
            .unwrap();
        host.commit_records(Default::default()).unwrap();
        host.set_full_access(0, false);
        assert_eq!(
            stored(&shared, 1, fixture::NAME).as_deref(),
            Some("Elevated")
        );
        host.close();
    }

    #[test]
    fn transactions_revert_and_locks_refuse() {
        let file = TempFile::new("transactions", &[], true);
        let shared = shared(&file);
        let mut host = session(&shared);
        host.begin_transaction(Default::default()).unwrap();
        host.set_field(field(fixture::NAME), Value::text("Changed"))
            .unwrap();
        host.new_record().unwrap();
        host.set_field(field(fixture::NAME), Value::text("New"))
            .unwrap();
        host.go_to_record(om_scripts::runtime::RecordMove::First, false)
            .unwrap();
        host.revert_transaction().unwrap();
        assert_eq!(
            stored(&shared, 1, fixture::NAME).as_deref(),
            Some("Write tests")
        );
        assert_eq!(record_ids(&shared), [1, 2]);
        assert!(shared.mailboxes.take(CLIENT, usize::MAX).is_empty());
        // A record deleted in a reverted transaction is still there.
        host.begin_transaction(Default::default()).unwrap();
        host.go_to_record(om_scripts::runtime::RecordMove::Last, false)
            .unwrap();
        host.delete_record(false).unwrap();
        assert_eq!(host.found_record(fixture::OCCURRENCE, 1).unwrap(), None);
        host.revert_transaction().unwrap();
        assert_eq!(record_ids(&shared), [1, 2]);
        assert_eq!(host.found_record(fixture::OCCURRENCE, 1).unwrap(), Some(2));

        // A record another client is editing is refused with 301.
        let other = LockUser {
            connection: CLIENT,
            txn: 5,
        };
        hosted(&shared)
            .locks
            .lock()
            .unwrap()
            .lock(
                &[uint_segment(fixture::TABLE), vec![5], serial::encode(1)],
                other,
                9,
                "client (Admin)",
            )
            .unwrap();
        host.go_to_record(om_scripts::runtime::RecordMove::First, false)
            .unwrap();
        let refused = host.set_field(field(fixture::NAME), Value::text("Mine"));
        assert_eq!(refused, Err(HostError::Error(ErrorCode(301))));
        // Deleting record 2 is fine and notifies.
        host.go_to_record(om_scripts::runtime::RecordMove::Last, false)
            .unwrap();
        host.delete_record(false).unwrap();
        assert_eq!(record_ids(&shared), [1]);
        let codes: Vec<u32> = shared
            .mailboxes
            .take(CLIENT, usize::MAX)
            .iter()
            .map(|n| n.code)
            .collect();
        assert_eq!(codes, [attention::DELETE | attention::RECORD]);
        host.close();
    }
}
