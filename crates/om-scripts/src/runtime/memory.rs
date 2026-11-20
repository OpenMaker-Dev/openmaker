//! An in-memory [`ScriptHost`] for tests and tools, built on om-calc's
//! [`MemoryHost`]: files with script catalogs, tables and records, layouts,
//! windows with found sets, layout triggers, record transactions, data
//! files, a scripted dialog responder, calculation functions answered from
//! the session (`ExecuteSQL`), and Perform Script on Server run by a
//! server-type runtime on a copy of the session.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use om_calc::eval::{
    CalcHost, Context, CustomFunction, FieldInfo, HostFunction, MemoryHost, Storage, VarScope,
    Variables,
};
use om_calc::{CalcError, Ctx, Date, Locale, Value};
use om_formula::Names;

use super::host::*;
use super::trigger::{Trigger, TriggerAction, TriggerEvent};
use super::{ErrorCode, Options, RunState, Runtime, ScriptCall};
use crate::{Client, Script, ScriptCatalog};

/// `Get` selectors the memory host answers from its windows and clock.
const CURRENT_DATE: u8 = 2;
const FOUND_COUNT: u8 = 7;
const LAYOUT_NAME: u8 = 10;
const LAYOUT_NUMBER: u8 = 11;
const WINDOW_MODE: u8 = 13;
const RECORD_NUMBER: u8 = 20;
const RECORD_ID: u8 = 37;
const WINDOW_NAME: u8 = 48;
const LAYOUT_TABLE_NAME: u8 = 61;

/// A layout: id, name and the table occurrence it shows.
#[derive(Clone, Debug)]
pub struct MemoryLayout {
    pub id: u64,
    pub name: String,
    pub occurrence: u64,
}

/// A window: its file, layout, mode and found set.
#[derive(Clone, Debug)]
pub struct MemoryWindow {
    pub id: WindowId,
    pub name: String,
    pub file: FileId,
    pub layout: u64,
    pub mode: Mode,
    /// Record ids in found-set order.
    pub found: Vec<u64>,
    /// Index into `found`.
    pub current: usize,
    pub focus: Option<String>,
}

/// A calculation function the session answers from its own state
/// (`ExecuteSQL` over its records): the arguments, then the value and the
/// error to report beside it (`CalcHost::call_reporting`).
pub type SessionFunction =
    Arc<dyn Fn(&mut MemoryScriptHost, &[Value]) -> (Value, Option<CalcError>) + Send + Sync>;

/// An event action waiting for its trigger script.
#[derive(Clone, Debug)]
enum Deferred {
    Layout(LayoutTarget),
    Commit,
}

/// The state a transaction reverts to.
#[derive(Clone)]
struct Snapshot {
    calc: MemoryHost,
    windows: Vec<MemoryWindow>,
    deleted: HashSet<(u64, u64)>,
}

/// A session in memory. Public fields are for tests to set up and inspect.
#[derive(Clone)]
pub struct MemoryScriptHost {
    pub calc: MemoryHost,
    /// Files by id: (name, scripts).
    files: HashMap<FileId, (String, Vec<Arc<Script>>)>,
    occurrences: HashMap<u64, u64>,
    pub layouts: Vec<MemoryLayout>,
    pub windows: Vec<MemoryWindow>,
    pub current_window: usize,
    pub deleted: HashSet<(u64, u64)>,
    /// Every host call, in order (`"go_to_layout Key(2)"`, ...).
    pub calls: Vec<String>,
    pub alerts: Vec<ErrorAlert>,
    /// The answer to every error alert.
    pub alert_choice: AlertChoice,
    pub log: Vec<ErrorLogEntry>,
    pub dialogs: Vec<CustomDialog>,
    /// Answers to custom dialogs in order; when empty the default button
    /// with the inputs unchanged.
    pub dialog_answers: VecDeque<DialogResponse>,
    /// Send Mail's messages, in order; none is sent.
    pub mails: Vec<Mail>,
    /// Layout triggers: (layout, event) -> (file, script).
    pub layout_triggers: HashMap<(u64, TriggerEvent), (FileId, u64)>,
    /// Calculation functions by lowercase name, answered before
    /// [`MemoryHost`]'s.
    pub functions: HashMap<String, SessionFunction>,
    queued: Vec<Trigger>,
    actions: HashMap<u64, Deferred>,
    next_action: u64,
    /// Trigger actions performed, with their outcome.
    pub trigger_actions: Vec<TriggerAction>,
    transaction: Option<Box<Snapshot>>,
    /// Data files by path.
    pub data_files: HashMap<String, Vec<u8>>,
    open_files: HashMap<i64, (String, usize)>,
    next_file: i64,
    /// Pause/Resume Script requests (seconds); every pause resumes at once.
    pub pauses: Vec<Option<f64>>,
    /// Pending answers for [`Self::pause`]: `true` makes a pause wait for
    /// [`Runtime::resume`].
    pub pause_waits: bool,
    /// Steps answered by [`ScriptHost::other_step`]: their ids.
    pub other_steps: Vec<u16>,
    /// The value `other_step` returns for a step id.
    pub other_results: HashMap<u16, Value>,
    /// Perform Script on Server runs the script on a server-type runtime.
    pub server_scripts: bool,
    /// Perform Script on Server with Callback's calls, in order; their
    /// callbacks are never fired.
    pub server_callbacks: Vec<ServerCallbackCall>,
    pub timer: Option<OnTimer>,
    /// The MBS plug-in subset's state; `None` (the default) evaluates its
    /// commands as without the plug-in, as the server does.
    pub plugin: Option<super::plugins::PluginState>,
    /// Each `FM.RunScriptIdle` (`Some`) and `FM.StopScriptIdle` (`None`).
    pub idle: Vec<Option<super::plugins::IdleScript>>,
    /// The alerts `Dialog.Run` showed, and the answers given in turn (a
    /// timeout when none is left).
    pub plugin_dialogs: Vec<super::plugins::PluginDialog>,
    pub plugin_answers: VecDeque<super::plugins::PluginDialogAnswer>,
    /// New Record returns [`HostError::Pending`] this many times (an
    /// asynchronous host).
    pub pending_new_records: u32,
    /// Open Transaction returns [`HostError::Pending`] this many times.
    pub pending_opens: u32,
    /// Commit Transaction returns [`HostError::Pending`] this many times.
    pub pending_commits: u32,
    /// Go to List of Records' requests, in order; the found set stays.
    pub record_lists: Vec<RecordList>,
}

impl MemoryScriptHost {
    pub fn new(locale: Locale, today: Date) -> Self {
        MemoryScriptHost {
            calc: MemoryHost::new(locale, today),
            files: HashMap::from([(0, ("File".to_owned(), Vec::new()))]),
            occurrences: HashMap::new(),
            layouts: Vec::new(),
            windows: Vec::new(),
            current_window: 0,
            deleted: HashSet::new(),
            calls: Vec::new(),
            alerts: Vec::new(),
            alert_choice: AlertChoice::Continue,
            log: Vec::new(),
            dialogs: Vec::new(),
            dialog_answers: VecDeque::new(),
            mails: Vec::new(),
            layout_triggers: HashMap::new(),
            functions: HashMap::new(),
            queued: Vec::new(),
            actions: HashMap::new(),
            next_action: 1,
            trigger_actions: Vec::new(),
            transaction: None,
            data_files: HashMap::new(),
            open_files: HashMap::new(),
            next_file: 1,
            pauses: Vec::new(),
            pause_waits: false,
            other_steps: Vec::new(),
            other_results: HashMap::new(),
            server_scripts: true,
            server_callbacks: Vec::new(),
            timer: None,
            plugin: None,
            idle: Vec::new(),
            plugin_dialogs: Vec::new(),
            plugin_answers: VecDeque::new(),
            pending_new_records: 0,
            pending_opens: 0,
            pending_commits: 0,
            record_lists: Vec::new(),
        }
    }

    /// A host in en-US with a fixed date.
    pub fn en_us() -> Self {
        MemoryScriptHost::new(
            Locale::en_us(),
            Date::from_ymd(2026, 10, 28).expect("valid date"),
        )
    }

    /// Name a file (file 0 exists as "File").
    pub fn add_file(&mut self, file: FileId, name: &str) {
        self.files.entry(file).or_default().0 = name.to_owned();
    }

    /// Add a script to a file's catalog.
    pub fn add_script(&mut self, file: FileId, script: Script) {
        self.files.entry(file).or_default().1.push(Arc::new(script));
    }

    /// Every script of a decoded catalog.
    pub fn add_catalog(&mut self, file: FileId, catalog: &ScriptCatalog) {
        for script in &catalog.scripts {
            self.add_script(file, script.clone());
        }
    }

    pub fn add_table(&mut self, table: u64, name: &str) {
        self.calc.add_table(table, name);
    }

    pub fn add_occurrence(&mut self, occurrence: u64, name: &str, table: u64) {
        self.calc.add_occurrence(occurrence, name, table);
        self.occurrences.insert(occurrence, table);
    }

    pub fn add_field(&mut self, table: u64, field: u64, name: &str, info: FieldInfo) {
        self.calc.add_field(table, field, name, info);
    }

    pub fn add_record(&mut self, table: u64, record: u64) {
        self.calc.add_record(table, record);
    }

    /// A layout of `occurrence`; the first layout gets a window.
    pub fn add_layout(&mut self, id: u64, name: &str, occurrence: u64) {
        self.layouts.push(MemoryLayout {
            id,
            name: name.to_owned(),
            occurrence,
        });
        if self.windows.is_empty() {
            let found = self.records_of(occurrence);
            self.windows.push(MemoryWindow {
                id: 1,
                name: self.files.get(&0).map_or_else(String::new, |f| f.0.clone()),
                file: 0,
                layout: id,
                mode: Mode::Browse,
                found,
                current: 0,
                focus: None,
            });
        }
    }

    /// The current window.
    pub fn window(&self) -> Option<&MemoryWindow> {
        self.windows.get(self.current_window)
    }

    fn window_mut(&mut self) -> Option<&mut MemoryWindow> {
        self.windows.get_mut(self.current_window)
    }

    fn layout(&self, id: u64) -> Option<&MemoryLayout> {
        self.layouts.iter().find(|l| l.id == id)
    }

    /// The current window's occurrence and record.
    pub fn current_record(&self) -> Option<(u64, u64)> {
        let window = self.window()?;
        let occurrence = self.layout(window.layout)?.occurrence;
        Some((occurrence, *window.found.get(window.current)?))
    }

    fn table_of(&self, occurrence: u64) -> Option<u64> {
        self.occurrences.get(&occurrence).copied()
    }

    /// The records of an occurrence's table in creation order, deleted ones
    /// left out.
    pub fn records_of(&self, occurrence: u64) -> Vec<u64> {
        let table = self.table_of(occurrence).unwrap_or(0);
        (0..)
            .map_while(|i| self.calc.found_record(occurrence, i).ok().flatten())
            .filter(|r| !self.deleted.contains(&(table, *r)))
            .collect()
    }

    /// A field's value on the current record, by name.
    pub fn value(&self, field: &str) -> Option<Value> {
        let (occurrence, record) = self.current_record()?;
        let id = self.calc.names().find_field(occurrence, field)?;
        self.calc.field_value(occurrence, record, id, 1).ok()
    }

    fn call(&mut self, call: impl Into<String>) {
        self.calls.push(call.into());
    }

    fn switch_layout(&mut self, target: &LayoutTarget) -> HostResult {
        let layout = match target {
            LayoutTarget::Original => return Ok(()),
            LayoutTarget::Key(id) => self.layout(*id).map(|l| l.id),
            LayoutTarget::Name(name) => self.layouts.iter().find(|l| l.name == *name).map(|l| l.id),
            LayoutTarget::Number(n) => usize::try_from(*n - 1)
                .ok()
                .and_then(|i| self.layouts.get(i))
                .map(|l| l.id),
        }
        .ok_or(HostError::Error(ErrorCode::LAYOUT_MISSING))?;
        let occurrence = self.layout(layout).map(|l| l.occurrence);
        let old = self.window().map(|w| w.layout);
        let same_table = old
            .and_then(|o| self.layout(o))
            .map(|l| self.table_of(l.occurrence))
            == occurrence.map(|o| self.table_of(o));
        let found = occurrence.map(|o| self.records_of(o)).unwrap_or_default();
        let window = self
            .window_mut()
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        if window.layout == layout {
            // The layout the window shows: nothing changes and no trigger
            // fires (script-runtime.md 13.12).
            return Ok(());
        }
        window.layout = layout;
        if !same_table {
            window.found = found;
            window.current = 0;
        }
        // OnRecordLoad fires with or without a record and is called first,
        // so OnLayoutEnter's script runs first (script-runtime.md 13.17).
        self.queue_trigger(layout, TriggerEvent::RecordLoad, None);
        self.queue_trigger(layout, TriggerEvent::LayoutEnter, None);
        Ok(())
    }

    /// Queue the current layout's trigger for `event`, if it has one.
    fn queue_trigger(
        &mut self,
        layout: u64,
        event: TriggerEvent,
        action: Option<Deferred>,
    ) -> bool {
        let Some(&(file, script)) = self.layout_triggers.get(&(layout, event)) else {
            return false;
        };
        let mut trigger = Trigger::new(event, file, script);
        trigger.mode = self.window().map_or(Mode::Browse, |w| w.mode);
        trigger.window = self.window().map(|w| w.id);
        trigger.from_step = true;
        if let Some(action) = action {
            let token = self.next_action;
            self.next_action += 1;
            self.actions.insert(token, action);
            trigger.action = Some(token);
        }
        self.queued.push(trigger);
        true
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            calc: self.calc.clone(),
            windows: self.windows.clone(),
            deleted: self.deleted.clone(),
        }
    }

    fn move_in(&mut self, movement: RecordMove) -> HostResult<bool> {
        let window = self
            .window_mut()
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        let last = window.found.len().saturating_sub(1);
        if window.found.is_empty() {
            // Next and Previous find no further record (Exit after last
            // applies); the others have none to go to.
            return match movement {
                RecordMove::Previous | RecordMove::Next => Ok(false),
                _ => Err(ErrorCode::RECORD_MISSING.into()),
            };
        }
        let target = match movement {
            RecordMove::First => 0,
            RecordMove::Last => last,
            RecordMove::Previous if window.current == 0 => return Ok(false),
            RecordMove::Previous => window.current - 1,
            RecordMove::Next if window.current >= last => return Ok(false),
            RecordMove::Next => window.current + 1,
            RecordMove::Number(n) if n < 1 => return Err(ErrorCode::RECORD_MISSING.into()),
            RecordMove::Number(n) => (n as usize - 1).min(last),
        };
        window.current = target;
        Ok(true)
    }

    fn field_named(&self, name: &str) -> Option<(u64, u64)> {
        let (occurrence, field) = match name.split_once("::") {
            Some((table, field)) => (self.calc.names().find_occurrence(table)?, field),
            None => (self.current_record()?.0, name),
        };
        Some((occurrence, self.calc.names().find_field(occurrence, field)?))
    }

    /// A field write: the field is checked before the record (Set Field
    /// into a calculation field is 201 without a record too,
    /// script-runtime.md 13.10).
    fn set_value(&mut self, field: FieldRef, value: Value) -> HostResult {
        let table = self
            .table_of(field.occurrence)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        let info = self
            .calc
            .field(field.occurrence, field.field)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        if matches!(info.storage, Storage::Unstored(_) | Storage::Summary) {
            return Err(ErrorCode(201).into());
        }
        if field.repetition > info.repetitions {
            return Err(ErrorCode(111).into());
        }
        let (_, record) = self
            .current_record()
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        self.calc
            .set(table, record, field.field, field.repetition, value);
        Ok(())
    }
}

impl CalcHost for MemoryScriptHost {
    fn locale(&self) -> &Locale {
        self.calc.locale()
    }

    fn today(&self) -> Date {
        self.calc.today()
    }

    fn names(&self) -> &dyn Names {
        self.calc.names()
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        self.calc.field(occurrence, field)
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        self.calc.field_value(occurrence, record, field, repetition)
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        self.calc.related_records(occurrence, record, related)
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        match self.window() {
            Some(w)
                if self
                    .layout(w.layout)
                    .is_some_and(|l| l.occurrence == occurrence) =>
            {
                Ok(w.found.get(index).copied())
            }
            _ => self.calc.found_record(occurrence, index),
        }
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        self.calc.base_table_name(occurrence)
    }

    fn get(&mut self, selector: u8, context: &Context) -> Result<Value, CalcError> {
        let window = self.window().cloned();
        let layout = window.as_ref().and_then(|w| self.layout(w.layout)).cloned();
        let answer = match (selector, &window) {
            (CURRENT_DATE, _) => Some(Value::Date(self.calc.today())),
            (FOUND_COUNT, Some(w)) => Some(Value::number(w.found.len() as i64)),
            (RECORD_NUMBER, Some(w)) if !w.found.is_empty() => {
                Some(Value::number(w.current as i64 + 1))
            }
            (RECORD_ID, Some(w)) => w.found.get(w.current).map(|r| Value::number(*r as i64)),
            (LAYOUT_NAME, _) => layout.as_ref().map(|l| Value::text(&l.name)),
            (LAYOUT_NUMBER, _) => layout
                .as_ref()
                .and_then(|l| self.layouts.iter().position(|x| x.id == l.id))
                .map(|i| Value::number(i as i64 + 1)),
            (WINDOW_MODE, Some(w)) => Some(Value::number(match w.mode {
                Mode::Browse => 0,
                Mode::Find => 1,
                Mode::Preview => 2,
                Mode::Layout => 4,
            })),
            (WINDOW_NAME, Some(w)) => Some(Value::text(&w.name)),
            (LAYOUT_TABLE_NAME, _) => layout
                .as_ref()
                .and_then(|l| self.calc.names().occurrence_name(l.occurrence))
                .map(Value::text),
            _ => None,
        };
        match answer {
            Some(value) => Ok(value),
            None => self.calc.get(selector, context),
        }
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        self.calc.variables(scope)
    }

    fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        self.calc.custom_function(id)
    }

    fn random(&mut self) -> f64 {
        self.calc.random()
    }

    fn call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Value, CalcError> {
        self.calc.call(function, args, context)
    }

    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> (Value, Option<CalcError>) {
        let session = match function {
            HostFunction::Named(name) => self.functions.get(&name.to_lowercase()).cloned(),
            _ => None,
        };
        match session {
            Some(f) => f(self, args),
            None => self.calc.call_reporting(function, args, context),
        }
    }
}

impl ScriptHost for MemoryScriptHost {
    fn calc_context(&mut self) -> Context {
        match self.current_record() {
            Some((occurrence, record)) => Context::record(occurrence, record),
            None => Context {
                occurrence: self
                    .window()
                    .and_then(|w| self.layout(w.layout))
                    .map(|l| l.occurrence),
                ..Context::new()
            },
        }
    }

    fn current_file(&mut self) -> FileId {
        self.window().map_or(0, |w| w.file)
    }

    fn current_window(&mut self) -> Option<WindowId> {
        self.window().map(|w| w.id)
    }

    fn script(&mut self, file: FileId, id: u64) -> Option<Arc<Script>> {
        self.files
            .get(&file)?
            .1
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    fn script_named(&mut self, file: FileId, name: &str) -> Option<u64> {
        self.files
            .get(&file)?
            .1
            .iter()
            .find(|s| {
                s.kind == crate::ScriptKind::Script && s.name.to_lowercase() == name.to_lowercase()
            })
            .map(|s| s.id)
    }

    /// The data sources are the other files, by name: a file is not one of
    /// its own (script-runtime.md 13.16).
    fn file_named(&mut self, name: &str) -> Option<FileId> {
        let current = self.window().map_or(0, |w| w.file);
        self.files
            .iter()
            .find(|(id, (n, _))| n == name && **id != current)
            .map(|(id, _)| *id)
    }

    fn use_window(&mut self, window: WindowId) -> HostResult {
        if let Some(i) = self.windows.iter().position(|w| w.id == window) {
            self.current_window = i;
        }
        Ok(())
    }

    fn window_mode(&mut self) -> Mode {
        self.window().map_or(Mode::Browse, |w| w.mode)
    }

    fn take_triggers(&mut self) -> Vec<Trigger> {
        std::mem::take(&mut self.queued)
    }

    fn trigger_action(&mut self, action: &TriggerAction) -> HostResult {
        self.trigger_actions.push(*action);
        let Some(deferred) = self.actions.remove(&action.action) else {
            return Ok(());
        };
        if !action.proceed {
            return Ok(());
        }
        match deferred {
            Deferred::Layout(target) => self.switch_layout(&target),
            Deferred::Commit => Ok(()),
        }
    }

    fn error_alert(&mut self, alert: &ErrorAlert) -> AlertChoice {
        self.alerts.push(alert.clone());
        self.alert_choice
    }

    fn log_error(&mut self, entry: &ErrorLogEntry) {
        self.log.push(entry.clone());
    }

    fn pause(&mut self, seconds: Option<f64>) -> HostResult<PauseEnd> {
        self.pauses.push(seconds);
        if self.pause_waits {
            Err(HostError::Pending)
        } else {
            Ok(PauseEnd::Resumed)
        }
    }

    fn install_on_timer(&mut self, timer: Option<OnTimer>) -> HostResult {
        self.timer = timer;
        Ok(())
    }

    fn plugin_state(&mut self) -> Option<&mut super::plugins::PluginState> {
        self.plugin.as_mut()
    }

    fn schedule_idle_script(&mut self, idle: Option<super::plugins::IdleScript>) -> HostResult {
        self.idle.push(idle);
        Ok(())
    }

    fn other_step(&mut self, call: &StepCall<'_>) -> HostResult<Option<Value>> {
        self.other_steps.push(call.id);
        match self.other_results.get(&call.id) {
            Some(value) => Ok(Some(value.clone())),
            None => Err(HostError::Unsupported),
        }
    }
}

impl Navigation for MemoryScriptHost {
    /// Keeps the request in [`MemoryScriptHost::record_lists`].
    fn go_to_list_of_records(&mut self, request: &RecordList) -> HostResult {
        self.call("go_to_list_of_records");
        self.record_lists.push(request.clone());
        Ok(())
    }

    fn go_to_layout(&mut self, layout: &LayoutTarget, _animation: Option<u32>) -> HostResult {
        self.call(format!("go_to_layout {layout:?}"));
        let current = self.window().map(|w| w.layout);
        if let Some(current) = current {
            if self.queue_trigger(
                current,
                TriggerEvent::LayoutExit,
                Some(Deferred::Layout(layout.clone())),
            ) {
                return Ok(());
            }
        }
        self.switch_layout(layout)
    }

    fn go_to_record(&mut self, movement: RecordMove, _with_dialog: bool) -> HostResult<bool> {
        self.call(format!("go_to_record {movement:?}"));
        let moved = self.move_in(movement)?;
        if moved {
            if let Some(layout) = self.window().map(|w| w.layout) {
                self.queue_trigger(layout, TriggerEvent::RecordLoad, None);
            }
        }
        Ok(moved)
    }

    /// The memory host has no portals: no related rows.
    fn go_to_portal_row(
        &mut self,
        movement: RecordMove,
        _select: bool,
        _with_dialog: bool,
    ) -> HostResult<bool> {
        self.call(format!("go_to_portal_row {movement:?}"));
        Err(ErrorCode::RECORD_MISSING.into())
    }

    fn go_to_next_field(&mut self, forward: bool) -> HostResult {
        self.call(format!("go_to_next_field {forward}"));
        Ok(())
    }

    /// No relationships are followed: no related records.
    fn go_to_related_record(&mut self, _request: &RelatedRecords<'_>) -> HostResult {
        self.call("go_to_related_record");
        Err(ErrorCode::RECORD_MISSING.into())
    }

    fn close_popover(&mut self) -> HostResult {
        self.call("close_popover");
        Ok(())
    }

    fn go_to_field(&mut self, field: Option<FieldRef>, _select: bool) -> HostResult {
        self.call(format!("go_to_field {field:?}"));
        Ok(())
    }

    fn go_to_object(&mut self, name: &str, _repetition: u32) -> HostResult {
        self.call(format!("go_to_object {name}"));
        if let Some(window) = self.window_mut() {
            window.focus = Some(name.to_owned());
        }
        Ok(())
    }

    fn enter_mode(
        &mut self,
        mode: Mode,
        _pause: bool,
        _requests: Option<&om_hbam::OwnedNode>,
    ) -> HostResult {
        self.call(format!("enter_mode {mode:?}"));
        if let Some(window) = self.window_mut() {
            window.mode = mode;
        }
        Ok(())
    }
}

impl Records for MemoryScriptHost {
    fn new_record(&mut self) -> HostResult {
        self.call("new_record");
        if self.pending_new_records > 0 {
            self.pending_new_records -= 1;
            return Err(HostError::Pending);
        }
        let window = self
            .window()
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        let occurrence = self
            .layout(window.layout)
            .map(|l| l.occurrence)
            .unwrap_or(0);
        let table = self
            .table_of(occurrence)
            .ok_or(HostError::Error(ErrorCode(106)))?;
        let id = (0..)
            .map_while(|i| self.calc.found_record(occurrence, i).ok().flatten())
            .max()
            .unwrap_or(0)
            + 1;
        self.calc.add_record(table, id);
        let window = self.window_mut().expect("window");
        window.found.push(id);
        window.current = window.found.len() - 1;
        Ok(())
    }

    fn duplicate_record(&mut self) -> HostResult {
        self.call("duplicate_record");
        self.new_record()
    }

    fn open_record(&mut self) -> HostResult {
        self.call("open_record");
        self.current_record()
            .map(|_| ())
            .ok_or(ErrorCode::RECORD_MISSING.into())
    }

    fn copy_records(&mut self, all: bool) -> HostResult {
        self.call(format!("copy_records {all}"));
        Ok(())
    }

    fn delete_portal_row(&mut self, _with_dialog: bool) -> HostResult {
        self.call("delete_portal_row");
        Err(ErrorCode::RECORD_MISSING.into())
    }

    /// Without a record there is nothing to delete and no error
    /// (script-runtime.md 13.2).
    fn delete_record(&mut self, _with_dialog: bool) -> HostResult {
        self.call("delete_record");
        let Some((occurrence, record)) = self.current_record() else {
            return Ok(());
        };
        let table = self.table_of(occurrence).unwrap_or(0);
        self.deleted.insert((table, record));
        let window = self.window_mut().expect("window");
        window.found.retain(|r| *r != record);
        window.current = window.current.min(window.found.len().saturating_sub(1));
        Ok(())
    }

    fn delete_all_records(&mut self, _with_dialog: bool) -> HostResult {
        self.call("delete_all_records");
        let Some((occurrence, _)) = self.current_record() else {
            return Ok(());
        };
        let table = self.table_of(occurrence).unwrap_or(0);
        let found = std::mem::take(&mut self.window_mut().expect("window").found);
        self.deleted.extend(found.into_iter().map(|r| (table, r)));
        Ok(())
    }

    fn commit_records(&mut self, _options: CommitOptions) -> HostResult {
        self.call("commit_records");
        if let Some(layout) = self.window().map(|w| w.layout) {
            self.queue_trigger(layout, TriggerEvent::RecordCommit, Some(Deferred::Commit));
        }
        Ok(())
    }

    fn revert_record(&mut self, _with_dialog: bool) -> HostResult {
        self.call("revert_record");
        Ok(())
    }

    fn begin_transaction(&mut self, _options: TransactionOptions) -> HostResult {
        self.call("begin_transaction");
        self.transaction = Some(Box::new(self.snapshot()));
        if self.pending_opens > 0 {
            self.pending_opens -= 1;
            return Err(HostError::Pending);
        }
        Ok(())
    }

    fn commit_transaction(&mut self, _skip_validation: bool) -> HostResult {
        self.call("commit_transaction");
        if self.pending_commits > 0 {
            self.pending_commits -= 1;
            return Err(HostError::Pending);
        }
        self.transaction = None;
        Ok(())
    }

    fn revert_transaction(&mut self) -> HostResult {
        self.call("revert_transaction");
        if let Some(snapshot) = self.transaction.take() {
            self.calc = snapshot.calc;
            self.windows = snapshot.windows;
            self.deleted = snapshot.deleted;
        }
        Ok(())
    }
}

impl Fields for MemoryScriptHost {
    fn set_field(&mut self, field: FieldRef, value: Value) -> HostResult {
        self.call(format!("set_field {}:{}", field.occurrence, field.field));
        self.set_value(field, value)
    }

    fn set_field_by_name(&mut self, name: &str, value: Value) -> HostResult {
        self.call(format!("set_field_by_name {name}"));
        let field = self.field_by_name(name)?;
        self.set_value(field, value)
    }

    /// Every record of the found set, each made current in turn; the
    /// active field is not modelled, so a field is needed.
    fn replace_field_contents(
        &mut self,
        field: Option<FieldRef>,
        with: &mut Replacement<'_>,
        _with_dialog: bool,
    ) -> HostResult {
        let field = field.ok_or(HostError::Unsupported)?;
        self.call(format!(
            "replace_field_contents {}:{}",
            field.occurrence, field.field
        ));
        let (count, current) = (self.window())
            .map(|w| (w.found.len(), w.current))
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        let contents = self.field_value_of(field)?;
        // Serial numbers that cannot all be counted write none of them.
        if let Replacement::Serial {
            initial, increment, ..
        } = with
        {
            let last = u64::try_from(count.saturating_sub(1)).unwrap_or(u64::MAX);
            if serial_text(initial, *increment, last).is_none() {
                return Err(HostError::Error(ErrorCode::UNAVAILABLE));
            }
        }
        let mut result = Ok(());
        for i in 0..count {
            if let Some(window) = self.window_mut() {
                window.current = i;
            }
            let value = match with {
                Replacement::CurrentContents => contents.clone(),
                Replacement::Calculation(evaluate) => evaluate(self),
                Replacement::Serial {
                    initial, increment, ..
                } => match serial_text(initial, *increment, i as u64) {
                    Some(text) => Value::text(text),
                    None => return Err(HostError::Error(ErrorCode::UNAVAILABLE)),
                },
            };
            result = self.set_value(field, value);
            if result.is_err() {
                break;
            }
        }
        if let Some(window) = self.window_mut() {
            window.current = current;
        }
        result
    }

    fn field_by_name(&mut self, name: &str) -> HostResult<FieldRef> {
        let (occurrence, field) = self
            .field_named(name)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
        Ok(FieldRef {
            occurrence,
            field,
            repetition: 1,
        })
    }

    fn field_value_of(&mut self, field: FieldRef) -> HostResult<Value> {
        let (_, record) = self
            .current_record()
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        self.calc
            .field_value(field.occurrence, record, field.field, field.repetition)
            .map_err(|e| HostError::Error(ErrorCode(e.code())))
    }

    fn insert(&mut self, target: InsertTarget, value: Value) -> HostResult {
        self.call("insert");
        let Some(field) = target.field else {
            return Err(ErrorCode::FIELD_MISSING.into());
        };
        let value = if target.select_all {
            value
        } else {
            let old = self
                .field_value_of(field)
                .unwrap_or_else(|_| Value::empty_text());
            let cx = Ctx::new(self.calc.locale(), self.calc.today());
            let mut text = old.to_text(&cx).into_string();
            text.push_str(value.to_text(&cx).as_str());
            Value::text(text)
        };
        self.set_value(field, value)
    }

    fn select_all(&mut self) -> HostResult {
        self.call("select_all");
        Ok(())
    }

    fn clipboard(&mut self, op: Clipboard, _target: InsertTarget) -> HostResult {
        self.call(format!("clipboard {op:?}"));
        Ok(())
    }

    fn set_selection(&mut self, _field: Option<FieldRef>, start: i64, end: i64) -> HostResult {
        self.call(format!("set_selection {start} {end}"));
        Ok(())
    }

    fn undo_redo(&mut self, choice: u16) -> HostResult {
        self.call(format!("undo_redo {choice}"));
        Ok(())
    }
}

impl FoundSets for MemoryScriptHost {
    fn show_all_records(&mut self) -> HostResult {
        self.call("show_all_records");
        let occurrence = self
            .window()
            .and_then(|w| self.layout(w.layout))
            .map(|l| l.occurrence)
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        let found = self.records_of(occurrence);
        let window = self.window_mut().expect("window");
        window.found = found;
        window.current = 0;
        Ok(())
    }

    fn show_omitted_only(&mut self) -> HostResult {
        self.call("show_omitted_only");
        let occurrence = self
            .window()
            .and_then(|w| self.layout(w.layout))
            .map(|l| l.occurrence)
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        let all = self.records_of(occurrence);
        let window = self.window_mut().expect("window");
        let omitted = all
            .into_iter()
            .filter(|r| !window.found.contains(r))
            .collect();
        window.found = omitted;
        window.current = 0;
        Ok(())
    }

    fn unsort_records(&mut self) -> HostResult {
        self.call("unsort_records");
        Ok(())
    }

    fn modify_last_find(&mut self) -> HostResult {
        self.call("modify_last_find");
        if let Some(window) = self.window_mut() {
            window.mode = Mode::Find;
        }
        Ok(())
    }

    /// Records keep their order: the memory host has no sort keys.
    fn sort_records(
        &mut self,
        _order: Option<&om_hbam::OwnedNode>,
        _with_dialog: bool,
    ) -> HostResult {
        self.call("sort_records");
        Ok(())
    }

    fn sort_by_field(&mut self, _field: Option<FieldRef>, direction: u16) -> HostResult {
        self.call(format!("sort_by_field {direction}"));
        Ok(())
    }

    fn omit_records(&mut self, count: i64, _with_dialog: bool) -> HostResult {
        self.call(format!("omit_records {count}"));
        let window = self
            .window_mut()
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        let start = window.current.min(window.found.len());
        let end = (start + count.max(0) as usize).min(window.found.len());
        window.found.drain(start..end);
        window.current = window.current.min(window.found.len().saturating_sub(1));
        Ok(())
    }

    fn perform_find(
        &mut self,
        kind: FindKind,
        _requests: Option<&om_hbam::OwnedNode>,
    ) -> HostResult {
        self.call(format!("perform_find {kind:?}"));
        Err(ErrorCode(401).into())
    }
}

impl Windows for MemoryScriptHost {
    fn new_window(&mut self, window: &NewWindow) -> HostResult {
        self.call("new_window");
        let mut copy = self
            .window()
            .cloned()
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        copy.id = self.windows.iter().map(|w| w.id).max().unwrap_or(0) + 1;
        if let Some(name) = &window.name {
            copy.name = name.clone();
        }
        self.windows.push(copy);
        self.current_window = self.windows.len() - 1;
        if let Some(layout) = window.layout {
            self.switch_layout(&LayoutTarget::Key(layout))?;
        }
        Ok(())
    }

    fn close_window(&mut self, target: &WindowTarget, _current_file_only: bool) -> HostResult {
        self.call(format!("close_window {target:?}"));
        let index = match target {
            WindowTarget::Current => Some(self.current_window),
            WindowTarget::Named(name) => self.windows.iter().position(|w| w.name == *name),
        }
        .filter(|i| *i < self.windows.len())
        .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        self.windows.remove(index);
        self.current_window = self
            .current_window
            .min(self.windows.len().saturating_sub(1));
        Ok(())
    }

    fn select_window(&mut self, target: &WindowTarget, _current_file_only: bool) -> HostResult {
        self.call(format!("select_window {target:?}"));
        if let WindowTarget::Named(name) = target {
            self.current_window = self
                .windows
                .iter()
                .position(|w| w.name == *name)
                .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        }
        Ok(())
    }

    fn set_window_title(
        &mut self,
        target: &WindowTarget,
        title: &str,
        _current_file_only: bool,
    ) -> HostResult {
        self.call(format!("set_window_title {title}"));
        let window = match target {
            WindowTarget::Current => self.window_mut(),
            WindowTarget::Named(name) => self.windows.iter_mut().find(|w| w.name == *name),
        }
        .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))?;
        window.name = title.to_owned();
        Ok(())
    }

    fn freeze_window(&mut self) -> HostResult {
        self.call("freeze_window");
        Ok(())
    }

    fn refresh_window(&mut self, _flush_joins: bool, _flush_external: bool) -> HostResult {
        self.call("refresh_window");
        Ok(())
    }

    fn adjust_window(&mut self, choice: u16) -> HostResult {
        self.call(format!("adjust_window {choice}"));
        Ok(())
    }

    fn scroll_window(&mut self, choice: u16) -> HostResult {
        self.call(format!("scroll_window {choice}"));
        Ok(())
    }

    fn show_hide(&mut self, what: Bar, visibility: Visibility, _lock: bool) -> HostResult {
        self.call(format!("show_hide {what:?} {visibility:?}"));
        Ok(())
    }

    fn view_as(&mut self, choice: u16) -> HostResult {
        self.call(format!("view_as {choice}"));
        Ok(())
    }

    fn refresh_object(&mut self, name: &str, _repetition: u32, _portal: bool) -> HostResult {
        self.call(format!("refresh_object {name}"));
        Ok(())
    }
}

impl Dialogs for MemoryScriptHost {
    fn open_menu_item(&mut self, step: u16) -> HostResult {
        self.call(format!("open_menu_item {step}"));
        Ok(())
    }

    fn show_plugin_dialog(
        &mut self,
        dialog: &super::plugins::PluginDialog,
    ) -> HostResult<super::plugins::PluginDialogAnswer> {
        self.plugin_dialogs.push(dialog.clone());
        Ok(self
            .plugin_answers
            .pop_front()
            .unwrap_or(super::plugins::PluginDialogAnswer::Timeout))
    }

    fn show_custom_dialog(&mut self, dialog: &CustomDialog) -> HostResult<DialogResponse> {
        self.dialogs.push(dialog.clone());
        Ok(self
            .dialog_answers
            .pop_front()
            .unwrap_or_else(|| DialogResponse {
                button: 1,
                inputs: dialog.inputs.iter().map(|i| i.value.clone()).collect(),
            }))
    }
}

impl Files for MemoryScriptHost {
    fn file_exists(&mut self, path: &[String]) -> HostResult<bool> {
        Ok(path.iter().any(|p| self.data_files.contains_key(p)))
    }

    fn create_data_file(&mut self, path: &[String], _create_folders: bool) -> HostResult {
        let path = path.first().ok_or(HostError::Error(ErrorCode(800)))?;
        self.data_files.insert(path.clone(), Vec::new());
        Ok(())
    }

    fn open_data_file(&mut self, path: &[String]) -> HostResult<i64> {
        let path = path
            .iter()
            .find(|p| self.data_files.contains_key(*p))
            .ok_or(HostError::Error(ErrorCode::FILE_MISSING))?;
        let id = self.next_file;
        self.next_file += 1;
        self.open_files.insert(id, (path.clone(), 0));
        Ok(id)
    }

    fn write_data_file(
        &mut self,
        id: i64,
        data: &Value,
        _encoding: Encoding,
        line_feed: bool,
    ) -> HostResult {
        let (path, _) = self
            .open_files
            .get(&id)
            .ok_or(HostError::Error(ErrorCode(300)))?;
        let cx = Ctx::new(self.calc.locale(), self.calc.today());
        let mut text = data.to_text(&cx).into_string();
        if line_feed {
            text.push('\n');
        }
        self.data_files
            .entry(path.clone())
            .or_default()
            .extend_from_slice(text.as_bytes());
        Ok(())
    }

    fn read_data_file(
        &mut self,
        id: i64,
        amount: Option<i64>,
        _encoding: Encoding,
    ) -> HostResult<Value> {
        let (path, position) = self
            .open_files
            .get_mut(&id)
            .ok_or(HostError::Error(ErrorCode(300)))?;
        let data = self
            .data_files
            .get(path.as_str())
            .cloned()
            .unwrap_or_default();
        let start = (*position).min(data.len());
        let end = amount.map_or(data.len(), |n| (start + n.max(0) as usize).min(data.len()));
        *position = end;
        Ok(Value::text(String::from_utf8_lossy(&data[start..end])))
    }

    fn close_data_file(&mut self, id: i64) -> HostResult {
        self.open_files
            .remove(&id)
            .map(|_| ())
            .ok_or(HostError::Error(ErrorCode(300)))
    }

    fn delete_file(&mut self, path: &[String]) -> HostResult {
        let removed = path.iter().any(|p| self.data_files.remove(p).is_some());
        if removed {
            Ok(())
        } else {
            Err(ErrorCode::FILE_MISSING.into())
        }
    }
}

impl Network for MemoryScriptHost {
    fn open_url(&mut self, url: &str, _with_dialog: bool, _external: bool) -> HostResult {
        self.call(format!("open_url {url}"));
        Ok(())
    }

    /// Keeps the message in [`MemoryScriptHost::mails`].
    fn send_mail(&mut self, mail: &Mail) -> HostResult {
        self.call("send_mail");
        self.mails.push(mail.clone());
        Ok(())
    }

    /// Runs the script on a server-type runtime over a copy of this
    /// session (its own `$$` variables and windows), then takes the
    /// copy's records back, as a hosted file would show them.
    fn perform_script_on_server(
        &mut self,
        call: &ServerScriptCall,
    ) -> HostResult<ServerScriptResult> {
        self.call(format!("perform_script_on_server {}", call.script));
        if !self.server_scripts {
            return Err(HostError::Unsupported);
        }
        let mut server = self.clone();
        server.calls.clear();
        server.calc.globals = Variables::new();
        let mut runtime = Runtime::new(Options::new(Client::Server));
        let mut script = ScriptCall::new(call.file, call.script);
        script.parameter = call.parameter.clone();
        runtime
            .perform_script(&mut server, script)
            .map_err(HostError::Error)?;
        let state = runtime.run(&mut server);
        debug_assert!(matches!(state, RunState::Idle), "{state:?}");
        let globals = std::mem::take(&mut self.calc.globals);
        self.calc = server.calc;
        self.calc.globals = globals;
        self.deleted = server.deleted;
        self.log.extend(server.log);
        Ok(ServerScriptResult {
            result: call.wait.then(|| runtime.final_result().cloned()).flatten(),
            error: runtime.final_error(),
        })
    }

    /// Keeps the call in [`MemoryScriptHost::server_callbacks`]; the
    /// server script does not run.
    fn perform_script_on_server_with_callback(&mut self, call: &ServerCallbackCall) -> HostResult {
        self.call(format!(
            "perform_script_on_server_with_callback {}",
            call.call.script
        ));
        if !self.server_scripts {
            return Err(HostError::Unsupported);
        }
        self.server_callbacks.push(call.clone());
        Ok(())
    }
}

impl Accounts for MemoryScriptHost {}

impl Platform for MemoryScriptHost {
    fn beep(&mut self) -> HostResult {
        self.call("beep");
        Ok(())
    }
}
