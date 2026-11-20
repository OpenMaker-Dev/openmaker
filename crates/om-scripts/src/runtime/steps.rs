//! Executing one step (`Execute`, `DispatchStep`; script-runtime.md 2.3,
//! 3, 5, 8, 11): the checks every step goes through, the steps the
//! runtime handles itself, and the decoding of every other step's
//! parameters into a [`ScriptHost`] call.

use std::sync::Arc;

use om_calc::eval::{Repetition, VarScope};
use om_calc::{Ctx, Value, ValueType};

use super::host::{encoding, visibility, Bar, StepArg};
use super::*;
use crate::params::Item;
use crate::step::flag;
use crate::{FieldSpec, ScriptStep, VARIABLE_NAME_SLOT};

/// `Get` selectors the insert steps read.
const CURRENT_DATE: u8 = 2;
const CURRENT_TIME: u8 = 29;
const USER_NAME: u8 = 31;

/// How a step's result reaches its target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Write {
    /// The insert steps: into a field at the insertion point (or over its
    /// contents with "Select entire contents"); a variable gets the text
    /// appended unless "Select entire contents" is on (script-runtime.md
    /// 5.3).
    Insert { select_all: bool },
    /// A result (URL, file, Data API, dialog): replaces a variable; a
    /// field gets it as [`Write::Insert`] does.
    Result { select_all: bool },
}

impl Runtime {
    /// `Execute` (script-runtime.md 2.3).
    pub(super) fn execute(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let id = step.id();
        if id == 0 {
            return Exec::error(ErrorCode::UNKNOWN);
        }
        if is_unknown_step(&self.options, id) {
            return Exec::error(ErrorCode::UNKNOWN_COMMAND);
        }
        if let Some(t) = &self.transaction {
            let failed = t.aborted || t.skip || (t.error.is_error() && self.revert_on_error);
            if failed && id != id::OPEN_TRANSACTION && id != id::COMMIT_TRANSACTION {
                return Exec::control();
            }
        }
        if self.options.client == Client::Server {
            match id {
                // Window steps the server's shared handlers reach although
                // the table has no Server bit (script-runtime.md 10.2,
                // 13.7): no text ruler, zoom or menu bar to change, and no
                // web viewer object to find.
                92 | 97 | 166 => return Exec::ok(),
                146 => return Exec::error(ErrorCode::INVALID),
                // Exit Application closes the session's files: the script
                // ends and its 13 is not stored (13.7).
                44 => {
                    let _ = host.exit_application();
                    self.halt_script(host);
                    return Exec::error(ErrorCode(13));
                }
                _ => {}
            }
        }
        if !client_supports(&self.options, id) {
            return Exec::error(ErrorCode::UNAVAILABLE);
        }
        match id {
            id::IF => self.do_if(host, step),
            id::ELSE_IF => {
                if matches!(self.last_executed, id::IF | id::ELSE_IF) && !self.clause_taken {
                    self.do_if(host, step)
                } else {
                    self.continue_at(step.number(0).unwrap_or(0));
                    Exec::control()
                }
            }
            id::ELSE => {
                if !(matches!(self.last_executed, id::IF | id::ELSE_IF) && !self.clause_taken) {
                    self.continue_at(i64::from(step.value));
                }
                Exec::control()
            }
            id::END_IF => {
                self.clause_taken = false;
                Exec::control()
            }
            id::LOOP => Exec::control(),
            id::EXIT_LOOP_IF => {
                if self.condition(host, step) {
                    self.exit_loop(host, step.value as i16 as i64);
                }
                Exec::control()
            }
            id::END_LOOP => {
                self.continue_after(step.value as i16 as i64);
                Exec::control()
            }
            id::HALT_SCRIPT => {
                self.halt_script(host);
                Exec::control()
            }
            id::EXIT_SCRIPT => self.exit_script(host, step),
            id::ALLOW_USER_ABORT => {
                self.allow_abort = step.has_flag(flag::TURN_ON);
                Exec::ok()
            }
            id::SET_ERROR_CAPTURE => {
                self.capture = step.has_flag(flag::TURN_ON);
                Exec::ok()
            }
            id::SET_LAYOUT_OBJECT_ANIMATION => {
                self.animation = step.has_flag(flag::TURN_ON);
                Exec::ok()
            }
            id::SET_REVERT_TRANSACTION_ON_ERROR => {
                self.revert_on_error = step.has_flag(flag::TURN_ON);
                Exec::ok()
            }
            id::SET_SESSION_IDENTIFIER => {
                self.session_identifier = self.eval_slot(host, step, 0).map(|e| e.value);
                Exec::ok()
            }
            id::SET_VARIABLE => self.set_variable(host, step),
            id::SET_VARIABLE_BY_NAME => self.set_variable_by_name(host, step),
            id::PERFORM_SCRIPT => self.perform_script_step(host, step),
            id::PERFORM_SCRIPT_ON_SERVER => self.perform_script_on_server(host, step),
            210 => self.perform_script_on_server_with_callback(host, step),
            id::PAUSE_RESUME => self.pause_resume(host, step),
            id::SET_ERROR_LOGGING => self.set_error_logging(step),
            id::OPEN_TRANSACTION => self.open_transaction(host, step),
            id::COMMIT_TRANSACTION => self.commit_transaction(host, step),
            id::REVERT_TRANSACTION => self.revert_transaction(host, step),
            148 => self.install_on_timer(host, step),
            87 => self.show_custom_dialog(host, step),
            _ => self.host_step(host, step),
        }
    }

    // ----- control flow (script-runtime.md 3) -------------------------------

    /// If, and Else If when it is evaluated.
    fn do_if(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let taken = self.condition(host, step);
        self.clause_taken = taken;
        if !taken {
            self.continue_at(i64::from(step.value));
        }
        Exec::control()
    }

    /// A Boolean condition in slot 0. A calculation that fails gives `?`,
    /// which is true; its error is not reported (script-runtime.md 3.2,
    /// 13.3).
    fn condition(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> bool {
        let Some(evaluation) = self.eval_slot(host, step, 0) else {
            return false;
        };
        let value = evaluation.value;
        value.is_invalid() || value.is_true(&Ctx::new(host.locale(), host.today()))
    }

    fn frame_count(&self) -> Option<isize> {
        match &self.stack.last()?.kind {
            FrameKind::Script { script, .. } => Some(script.steps.len() as isize),
            FrameKind::Step { .. } => None,
        }
    }

    fn set_index(&mut self, new: isize) {
        if let Some(Frame {
            kind: FrameKind::Script { index, .. },
            ..
        }) = self.stack.last_mut()
        {
            *index = new;
        }
    }

    /// "Continue at step v": the next step executed is `v` (index `v - 1`
    /// after the fetch increments); 0 restarts at the first step, values
    /// past the count or above 0x8000 end the script.
    fn continue_at(&mut self, v: i64) {
        let Some(count) = self.frame_count() else {
            return;
        };
        let index = match v {
            0 => -1,
            1..=0x8000 => (v as isize - 1).min(count),
            _ => count,
        };
        self.set_index(index);
    }

    /// "Continue after step v": the next step executed is `v + 1`.
    fn continue_after(&mut self, v: i64) {
        let Some(count) = self.frame_count() else {
            return;
        };
        let index = if v < -1 || v >= count as i64 {
            count
        } else {
            v as isize
        };
        self.set_index(index);
    }

    /// Leave a loop after End Loop `target` (Exit Loop If, Exit after
    /// last); a transaction opened in this frame whose Commit Transaction
    /// lies inside the skipped part is reverted (script-runtime.md 3.3).
    fn exit_loop(&mut self, host: &mut dyn ScriptHost, target: i64) {
        let frame = self.stack.last().map(|f| f.id);
        let crosses = self
            .transaction
            .as_ref()
            .is_some_and(|t| Some(t.frame) == frame && i64::from(t.end_index) < target);
        if crosses {
            self.transaction = None;
            let _ = host.revert_transaction();
        }
        self.continue_after(target);
    }

    /// Go to Record / Go to Portal Row found no further record with Exit
    /// after last on: leave the enclosing loop, or the script when the
    /// step is outside a loop (`ProcessExitLoop`).
    fn exit_after_last(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) {
        match step.number(0) {
            Some(end) if end >= 0 => self.exit_loop(host, end),
            _ => self.exit_frame(host, true),
        }
    }

    /// Exit Script (script-runtime.md 3.4).
    fn exit_script(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let result = if step.has_flag(flag::CALCULATION) {
            self.eval_slot(host, step, 0).map(|e| e.value)
        } else {
            None
        };
        if matches!(
            self.stack.last().map(|f| &f.kind),
            Some(FrameKind::Step { .. })
        ) {
            let frame = self.stack.pop().expect("frame");
            self.unstarted.remove(&frame.id);
            if self.stack.is_empty() {
                self.halt_script(host);
                return Exec::control();
            }
        }
        let cx = Ctx::new(host.locale(), host.today());
        let boolean = result.as_ref().map_or(true, |v| v.is_true(&cx));
        let is_trigger = self.stack.last().is_some_and(|f| f.trigger.is_some());
        let is_final = self.stack.last().map(|f| f.id) == self.final_frame;
        if let (Some(value), false) = (&result, is_trigger) {
            // Results keep their data type (script-runtime.md 13.5); the
            // final frame's goes to the request instead of a caller (11.3).
            let below = self.stack.len().saturating_sub(1);
            let caller = self.stack[..below]
                .iter()
                .rposition(|f| matches!(f.kind, FrameKind::Script { .. }));
            match caller {
                _ if is_final => self.final_result = Some(value.clone()),
                Some(i) => self.stack[i].result = Some(Arc::new(value.clone())),
                None => {}
            }
        }
        self.exit_frame(host, boolean);
        Exec::control()
    }

    // ----- variables (script-runtime.md 5) ----------------------------------

    /// Set Variable (141).
    fn set_variable(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let Some(name) = step.text(0).filter(|n| valid_variable_name(n)) else {
            return Exec::error(ErrorCode::INVALID);
        };
        self.assign_variable(host, step, name)
    }

    /// Set Variable by Name (250): Set Variable with the name calculated
    /// in slot 3 (script-steps.md 12.4). The name is evaluated first, and
    /// one that does not name a variable fails as Set Variable's stored
    /// name does, with error 5 [I: FileMaker Pro 26.1.1 not run].
    fn set_variable_by_name(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let Some(name) = (self.slot_text(host, step, 3)).filter(|n| valid_variable_name(n)) else {
            return Exec::error(ErrorCode::INVALID);
        };
        self.assign_variable(host, step, name)
    }

    /// Set Variable's assignment of the repetition (slot 2) and value
    /// (slot 1) to the variable `name`.
    fn assign_variable(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        name: String,
    ) -> Exec {
        let repetition = match self.eval_slot(host, step, 2) {
            None => Repetition::from(1),
            Some(e) if e.value.is_empty() => Repetition::from(1),
            Some(e) => {
                let cx = Ctx::new(host.locale(), host.today());
                // Any size of integer; `?` is error 5 and assigns nothing
                // (script-runtime.md 13.6).
                match Repetition::from_number(&e.value.to_number(&cx)) {
                    Some(repetition) => repetition,
                    None => return Exec::error(ErrorCode::INVALID),
                }
            }
        };
        let value = if step.has_flag(flag::CALCULATION) {
            self.eval_slot(host, step, 1)
                .map_or_else(Value::empty_text, |e| e.value)
        } else {
            Value::empty_text()
        };
        self.variables(host, &name).set(&name, repetition, value);
        Exec::ok()
    }

    /// The map a variable name selects for the current frame.
    fn variables(
        &mut self,
        host: &mut dyn ScriptHost,
        name: &str,
    ) -> &mut om_calc::eval::Variables {
        match VarScope::of(name) {
            VarScope::Global => {
                let file = self
                    .stack
                    .last()
                    .map_or_else(|| host.current_file(), |f| f.file);
                self.globals.entry(file).or_default()
            }
            VarScope::Local => match self.script_frame_index() {
                Some(i) => match &mut self.stack[i].kind {
                    FrameKind::Script { vars, .. } => vars,
                    FrameKind::Step { .. } => unreachable!("script frame"),
                },
                None => &mut self.level0,
            },
        }
    }

    /// Write a step's result to its target: a variable (here) or a field
    /// (the host).
    fn write_target(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        value: Value,
        write: Write,
    ) -> Exec {
        let (select_all, insert) = match write {
            Write::Insert { select_all } => (select_all, true),
            Write::Result { select_all } => (select_all, false),
        };
        match step.field_spec() {
            Some(spec) if spec.is_variable() => {
                let Some(name) = step.variable_name(0).filter(|n| valid_variable_name(n)) else {
                    return Exec::error(ErrorCode::INVALID);
                };
                let repetition = i64::from(spec.repetition().max(1));
                let value = if insert && !select_all {
                    self.appended(host, &name, repetition, value)
                } else {
                    value
                };
                self.variables(host, &name).set(&name, repetition, value);
                Exec::ok()
            }
            Some(spec) => match field_ref(&spec, 1) {
                Some(field) => Exec::from_host(host.insert(
                    InsertTarget {
                        field: Some(field),
                        select_all,
                    },
                    value,
                )),
                None => Exec::error(ErrorCode::FIELD_MISSING),
            },
            None => Exec::from_host(host.insert(
                InsertTarget {
                    field: None,
                    select_all,
                },
                value,
            )),
        }
    }

    /// A variable's text with `value` appended (a container is replaced).
    fn appended(
        &mut self,
        host: &mut dyn ScriptHost,
        name: &str,
        repetition: i64,
        value: Value,
    ) -> Value {
        let cx_locale = host.locale().clone();
        let today = host.today();
        let current = self.variables(host, name).get(name, repetition).cloned();
        match current {
            Some(old) if old.value_type() != ValueType::Container => {
                let cx = Ctx::new(&cx_locale, today);
                let mut text = old.to_text(&cx).into_string();
                text.push_str(value.to_text(&cx).as_str());
                Value::text(text)
            }
            _ => value,
        }
    }

    /// The value of a step's data source (Write to Data File): a variable
    /// or a field.
    fn read_target(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
    ) -> Result<Value, HostError> {
        match step.field_spec() {
            Some(spec) if spec.is_variable() => {
                let name = step
                    .variable_name(0)
                    .ok_or(HostError::Error(ErrorCode::INVALID))?;
                let repetition = i64::from(spec.repetition().max(1));
                Ok(self
                    .variables(host, &name)
                    .get(&name, repetition)
                    .cloned()
                    .unwrap_or_else(Value::empty_text))
            }
            Some(spec) => host.field_value_of(
                field_ref(&spec, 1).ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?,
            ),
            None => Err(HostError::Error(ErrorCode::FIELD_MISSING)),
        }
    }

    // ----- script calls (script-runtime.md 3.6, 3.7, 11) --------------------

    /// The file and script a Perform Script-like step names: by name (a
    /// calculation, `script` or `source::script`) or by stored key.
    fn resolve_script(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
    ) -> Result<(FileId, u64), ErrorCode> {
        let file = self.current_file();
        let name_slot = if step.list_value(1) == Some(1) && step.calculation(2).is_some() {
            Some(2)
        } else if step.script_key().is_none() && step.calculation(1).is_some() {
            Some(1)
        } else {
            None
        };
        if let Some(slot) = name_slot {
            let name = self.slot_text(host, step, slot).unwrap_or_default();
            // A data source the file does not have: no such script
            // (script-runtime.md 13.16).
            let (file, name, exact) = match name.split_once("::") {
                Some((source, name)) => (
                    host.file_named_from(file, source)
                        .ok_or(ErrorCode::SCRIPT_MISSING)?,
                    name.to_owned(),
                    true,
                ),
                None => (file, name, false),
            };
            let id = host
                .script_named(file, &name)
                .filter(|id| !exact || host.script(file, *id).is_some_and(|s| s.name == name))
                .ok_or(ErrorCode::SCRIPT_MISSING)?;
            return Ok((file, id));
        }
        let in_button = matches!(
            self.stack.last().map(|f| &f.kind),
            Some(FrameKind::Step { .. })
        );
        let key = step.script_id();
        // A layout button's step names its script whatever its flags say
        // (the old prototype relied on it; layouts store kScriptKey).
        let key = key.or_else(|| in_button.then(|| step.target_script_id()).flatten());
        let id = key.ok_or(ErrorCode::SCRIPT_MISSING)?;
        let file = match step.data_leaf(crate::StepData::FILE_REFERENCES) {
            Some(reference) if step.has_flag(flag::FILE_REFERENCE) => {
                match host.resolve_file(file, reference) {
                    Ok(file) => file,
                    Err(HostError::Error(code)) => return Err(code),
                    Err(_) => return Err(ErrorCode::FILE_MISSING),
                }
            }
            _ => file,
        };
        Ok((file, id))
    }

    /// Perform Script (1).
    fn perform_script_step(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let (file, script) = match self.resolve_script(host, step) {
            Ok(target) => target,
            Err(code) => return Exec::error(code),
        };
        let button = match self.stack.last().map(|f| &f.kind) {
            Some(FrameKind::Step { parameter, .. }) => Some(parameter.clone()),
            _ => None,
        };
        let parameter = match self.eval_slot(host, step, 0) {
            Some(e) => Some(e.value),
            None => button.clone().flatten(),
        };
        let parent_full_access = self.stack.last().is_some_and(|f| f.full_access);
        let frame = match self.script_frame(host, file, script, parameter, parent_full_access) {
            Ok(frame) => frame,
            Err(code) => return Exec::error(code),
        };
        if button.is_some() {
            // A button's "current script" option (script-runtime.md 3.6
            // step 4): the stored value, 0 meaning Pause.
            let mode = match step.value {
                0 => QueueMode::Pause,
                v => QueueMode::from_value(v),
            };
            self.button_queue_mode(host, mode);
        }
        self.push(frame, true);
        Exec::ok()
    }

    /// Apply a button's queue mode to the script running below its frame.
    fn button_queue_mode(&mut self, host: &mut dyn ScriptHost, mode: QueueMode) {
        if self.stack.len() < 2 {
            return;
        }
        match mode {
            QueueMode::Halt => {
                let button = self.stack.pop().expect("button frame");
                self.halt_script(host);
                self.stack.push(button);
            }
            QueueMode::Exit => {
                let button = self.stack.pop().expect("button frame");
                self.exit_frame(host, true);
                if self.stack.is_empty() {
                    self.reset_for_new_script();
                }
                self.stack.push(button);
            }
            QueueMode::Resume => self.resume(),
            QueueMode::Pause | QueueMode::Continue => {}
        }
    }

    /// Perform Script on Server (164).
    fn perform_script_on_server(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let call = match self.server_call(host, step) {
            Ok(call) => call,
            Err(code) => return Exec::error(code),
        };
        match host.perform_script_on_server(&call) {
            Ok(reply) => {
                let result = reply.result.map(|v| Arc::new(self.as_text(host, &v)));
                if let Some(frame) = self.stack.last_mut() {
                    frame.result = result;
                }
                Exec::error(reply.error)
            }
            Err(e) => Exec::from_host(Err(e)),
        }
    }

    fn server_call(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
    ) -> Result<ServerScriptCall, ErrorCode> {
        let (file, script) = self.resolve_script(host, step)?;
        let parameter = self
            .eval_slot(host, step, 0)
            .map(|e| self.as_text(host, &e.value));
        if parameter
            .as_ref()
            .is_some_and(|p| self.text_of(host, p).chars().count() > PSOS_LIMIT)
        {
            return Err(ErrorCode::PARAMETER_TOO_LONG);
        }
        Ok(ServerScriptCall {
            file,
            script,
            parameter,
            wait: !step.has_flag(flag::OPTION),
        })
    }

    /// Perform Script on Server with Callback (210).
    fn perform_script_on_server_with_callback(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
    ) -> Exec {
        let mut call = match self.server_call(host, step) {
            Ok(call) => call,
            Err(code) => return Exec::error(code),
        };
        call.wait = false;
        let file = self.current_file();
        let callback = match self.psos_callback(host, step, file) {
            Ok(script) => script.map(|id| (file, id)),
            Err(code) => return Exec::error(code),
        };
        Exec::from_host(
            host.perform_script_on_server_with_callback(&ServerCallbackCall {
                call,
                callback,
                callback_parameter: step.calculation(4),
                state: step.value,
            }),
        )
    }

    /// The callback script of step 210 in `file`, the calling script's
    /// file (script-steps.md 12.2): by name when `[00 6]` = 1, the name
    /// calculation in slot 7; from the list when `kOption` marks the key
    /// that slot 3 holds as a hex literal. Pro keeps that slot when the
    /// choice changes to by name, so the choice decides. A script that is
    /// not there is 104 before the server is called; a callback in another
    /// file (`kOption3` with its data source in slot 5, or a by-name
    /// `source::script`) is 3, not supported. Both are OpenMaker's choices:
    /// FileMaker's handling of either is unmeasured.
    fn psos_callback(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        file: FileId,
    ) -> Result<Option<u64>, ErrorCode> {
        if step.list_value(6) == Some(1) {
            let name = self.slot_text(host, step, 7).unwrap_or_default();
            if name.contains("::") {
                return Err(ErrorCode::UNAVAILABLE);
            }
            return host
                .script_named(file, &name)
                .map(Some)
                .ok_or(ErrorCode::SCRIPT_MISSING);
        }
        if !step.has_flag(flag::OPTION) {
            return Ok(None);
        }
        if step.has_flag(flag::OPTION3) {
            return Err(ErrorCode::UNAVAILABLE);
        }
        step.positioned_id(3)
            .filter(|&id| host.script(file, id).is_some())
            .map(Some)
            .ok_or(ErrorCode::SCRIPT_MISSING)
    }

    /// Pause/Resume Script (62, script-runtime.md 6.2).
    fn pause_resume(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        if matches!(
            self.stack.last().map(|f| &f.kind),
            Some(FrameKind::Step { .. })
        ) {
            // From a button: resume the paused script.
            let frame = self.stack.pop().expect("frame");
            self.unstarted.remove(&frame.id);
            self.resume();
            if self.stack.is_empty() {
                self.halt_script(host);
            }
            return Exec::control();
        }
        let seconds = if step.has_flag(flag::CALCULATION) {
            self.slot_float(host, step, 0)
        } else {
            None
        };
        self.pause(host, seconds)
    }

    /// Pause the current script: the host waits, or the runtime returns
    /// [`RunState::Paused`] until [`Runtime::resume`].
    fn pause(&mut self, host: &mut dyn ScriptHost, seconds: Option<f64>) -> Exec {
        let seconds = match seconds {
            None if self.options.is_server() => Some(0.1),
            other => other,
        };
        match host.pause(seconds) {
            Ok(PauseEnd::Resumed) => Exec::ok(),
            Ok(PauseEnd::Canceled) => {
                if self.allow_abort {
                    self.set_last_error(ErrorCode::CANCELED, false);
                    self.halt_script(host);
                    Exec::control()
                } else {
                    Exec::ok()
                }
            }
            Err(HostError::Pending) => {
                if let Some(frame) = self.stack.last_mut() {
                    frame.paused = Some(seconds);
                }
                Exec::ok()
            }
            Err(e) => Exec::from_host(Err(e)),
        }
    }

    /// Install OnTimer Script (148).
    fn install_on_timer(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let script = step.script_id();
        let interval = self.slot_float(host, step, 1).unwrap_or(0.0);
        let timer = script.filter(|_| interval > 0.0).map(|script| OnTimer {
            file: host.current_file(),
            window: host.current_window(),
            script,
            parameter: step.calculation(0),
            interval,
        });
        Exec::from_host(host.install_on_timer(timer))
    }

    /// Set Error Logging (200, script-runtime.md 4.8).
    fn set_error_logging(&mut self, step: &ScriptStep) -> Exec {
        if self.options.is_server() && !self.options.server_error_logging {
            return Exec::error(ErrorCode::UNAVAILABLE);
        }
        let file = self.current_file();
        if step.has_flag(flag::OPTION) {
            self.error_logging.insert(file, step.calculation(0));
        } else {
            self.error_logging.remove(&file);
        }
        Exec::ok()
    }

    // ----- transactions (script-runtime.md 8) -------------------------------

    /// Open Transaction (205). While one is open, in this frame or a
    /// caller's, it is 3 and the open one goes on (script-runtime.md 13.10).
    fn open_transaction(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        if self.transaction.is_some() {
            return Exec::error(ErrorCode::UNAVAILABLE);
        }
        let options = TransactionOptions {
            skip_validation: step.has_flag(flag::OPTION),
            override_ess_locking: step.has_flag(flag::OPTION2),
            skip_auto_enter: step.has_flag(flag::SELECT_ALL),
        };
        self.transaction_error_location = None;
        let exec = Exec::from_host(host.begin_transaction(options));
        if exec.pending {
            return exec;
        }
        self.start_transaction(host, step, exec.error);
        exec
    }

    /// The runtime's side of Open Transaction once the host has started
    /// (or failed to start) it, also when the start completed later
    /// (script-runtime.md 1.7, 8.1): a start error skips the transaction's
    /// steps.
    pub(super) fn start_transaction(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        error: ErrorCode,
    ) {
        let frame = self.stack.last().map_or(0, |f| f.id);
        self.transaction = Some(Transaction {
            file: host.current_file(),
            window: host.current_window(),
            frame,
            end_index: step.value,
            error,
            aborted: false,
            skip: error.is_error(),
            options: TransactionOptions {
                skip_validation: step.has_flag(flag::OPTION),
                override_ess_locking: step.has_flag(flag::OPTION2),
                skip_auto_enter: step.has_flag(flag::SELECT_ALL),
            },
        });
        if error.is_error() {
            self.transaction_error_location = Some(self.location(step));
        }
    }

    /// Commit Transaction (206). Without an open transaction, or in
    /// another frame than the one that opened it, it is 3 and an open one
    /// goes on (script-runtime.md 13.10).
    fn commit_transaction(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let frame = self.stack.last().map_or(0, |f| f.id);
        if self.transaction.as_ref().map_or(true, |t| t.frame != frame) {
            return self.commit_failed(ErrorCode::UNAVAILABLE);
        }
        let t = self.transaction.as_ref().expect("open transaction");
        if host.current_file() != t.file {
            return self.commit_failed(ErrorCode::UNAVAILABLE);
        }
        let error = if t.skip || t.aborted {
            t.error
        } else if t.error.is_error() && self.revert_on_error {
            let error = t.error;
            let _ = host.revert_transaction();
            error
        } else {
            match host.commit_transaction(t.options.skip_validation) {
                Ok(()) => ErrorCode::NONE,
                Err(HostError::Error(code)) => code,
                Err(HostError::Unsupported) => {
                    self.transaction = None;
                    return Exec::from_host(Err(HostError::Unsupported));
                }
                // The transaction stays open until the commit completes.
                Err(HostError::Pending) => return Exec::from_host(Err(HostError::Pending)),
            }
        };
        self.finish_commit(step, error)
    }

    /// Closes the transaction with the commit's outcome, also when a
    /// pending commit completed later (script-runtime.md 1.7, 8.2). A
    /// failure is located at Commit Transaction; `Get ( LastErrorDetail )`
    /// names the step that failed the transaction, with a custom revert's
    /// message (13.10).
    pub(super) fn finish_commit(&mut self, step: &ScriptStep, error: ErrorCode) -> Exec {
        let Some(t) = self.transaction.take() else {
            return Exec::error(error);
        };
        if error.is_error() && !t.skip && !t.aborted && error != t.error {
            // A failure of the commit itself.
            self.transaction_error_location = Some(self.location(step));
        }
        let message = std::mem::take(&mut self.transaction_detail);
        if !error.is_error() || error.is_internal() {
            return Exec::error(error);
        }
        let mut detail = self
            .transaction_error_location
            .as_ref()
            .map(StepLocation::text)
            .unwrap_or_default();
        if error.is_custom() && !message.is_empty() {
            detail.push('\r');
            detail.push_str(&message);
        }
        let exec = self.commit_failed(error);
        self.last_error_detail = detail;
        exec
    }

    /// `Execute`'s handling of a failed Commit Transaction (2.3): the code is
    /// stored at once; with a code other than a custom one the step is then
    /// a control step, neither stored again nor reported (13.2).
    fn commit_failed(&mut self, error: ErrorCode) -> Exec {
        self.set_last_error(error, false);
        Exec {
            error,
            handled: true,
            control: !error.is_custom(),
            ..Exec::default()
        }
    }

    /// Revert Transaction (207).
    fn revert_transaction(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        if step.has_flag(flag::OPTION) && !self.condition(host, step) {
            return Exec::ok();
        }
        if self
            .transaction
            .as_ref()
            .map_or(true, |t| t.file != host.current_file())
        {
            return Exec::error(ErrorCode::UNAVAILABLE);
        }
        let custom = if step.has_flag(flag::OPTION2) {
            self.slot_integer(host, step, 1)
                .map(|n| ErrorCode(n as i32))
                .filter(|c| c.is_custom())
        } else {
            None
        };
        let message = custom
            .and_then(|_| self.slot_text(host, step, 2))
            .unwrap_or_default();
        let result = Self::revert_owned_transaction(
            host,
            self.transaction.as_ref().expect("open transaction"),
        );
        let location = self.location(step);
        let t = self.transaction.as_mut().expect("open transaction");
        t.aborted = true;
        if let Some(code) = custom {
            // Revert Transaction itself succeeds; Commit Transaction reports
            // the code (script-runtime.md 13.10).
            t.error = code;
            self.transaction_error_location = Some(location);
            self.transaction_detail = message;
        }
        match result {
            Err(HostError::Unsupported) => Exec::from_host(result),
            _ => Exec::ok(),
        }
    }

    /// `SetTransactionError`: the first error inside an open transaction.
    /// Only the data layer records one (script-runtime.md 8.4, 13.10): a
    /// step that writes records, or a user cancel. Errors of other steps
    /// (Perform Script's 104, Go to Record's 101, an unsupported step's 3,
    /// a sub-script's own Open or Commit Transaction) do not.
    pub(super) fn record_transaction_error(&mut self, step: &ScriptStep, error: ErrorCode) {
        if error == ErrorCode::RETRY || error == ErrorCode::MULTI_STAGE || self.is_paused() {
            return;
        }
        // A step the runtime does not know (on the server, 249 among them)
        // never reaches the data layer.
        if error != ErrorCode::CANCELED
            && (!writes_records(step.id()) || is_unknown_step(&self.options, step.id()))
        {
            return;
        }
        let location = self.location(step);
        if let Some(t) = &mut self.transaction {
            if !t.error.is_error() {
                t.error = map_error(error);
                self.transaction_error_location = Some(location);
            }
        }
    }

    // ----- Show Custom Dialog (87) ------------------------------------------

    fn show_custom_dialog(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let title = self.slot_text(host, step, 0).unwrap_or_default();
        let message = self.slot_text(host, step, 1).unwrap_or_default();
        let mut buttons = Vec::new();
        for n in 0..3u16 {
            let label = match self.slot_text(host, step, 5 + n) {
                Some(label) => label,
                None => step.text(n).unwrap_or_default(),
            };
            if !label.is_empty() {
                let commit = step.number(3 + n).map_or(n == 0, |c| c != 0);
                buttons.push((n, DialogButton { label, commit }));
            }
        }
        if buttons.is_empty() {
            buttons.push((
                0,
                DialogButton {
                    label: "OK".into(),
                    commit: true,
                },
            ));
        }
        let mut inputs = Vec::new();
        let mut targets = Vec::new();
        for n in 0..3u16 {
            let Some(spec) = step.slot_field_spec(n) else {
                continue;
            };
            // FileMaker Pro keeps each unused input as a cleared spec
            // (`FieldSpec::names_field`): no input, nothing written (#940).
            let target = if spec.is_variable() {
                match step.text(VARIABLE_NAME_SLOT + n) {
                    Some(name) if valid_variable_name(&name) => {
                        DialogTarget::Variable(name, i64::from(spec.repetition().max(1)))
                    }
                    _ => continue,
                }
            } else if spec.names_field() {
                match field_ref(&spec, 1) {
                    Some(field) => DialogTarget::Field(field),
                    None => continue,
                }
            } else {
                continue;
            };
            let value = match &target {
                DialogTarget::Variable(name, rep) => self
                    .variables(host, name)
                    .get(name, *rep)
                    .cloned()
                    .unwrap_or_else(Value::empty_text),
                DialogTarget::Field(field) => host
                    .field_value_of(*field)
                    .unwrap_or_else(|_| Value::empty_text()),
            };
            inputs.push(DialogInput {
                label: self.slot_text(host, step, 2 + n).unwrap_or_default(),
                password: step.number(n).is_some_and(|p| p != 0),
                value,
            });
            targets.push(target);
        }
        // "Close after (seconds)" (26.1.1): on in numeric slot 6, the
        // seconds in calculation slot 13 (script-steps.md 12.4, from the
        // binaries). The result is taken as a number as it is, not through
        // its text. A time that is not a positive number leaves the dialog
        // to the user: OpenMaker's choice, FileMaker's is unmeasured.
        let close_after = step
            .number(6)
            .is_some_and(|on| on != 0)
            .then(|| self.eval_slot(host, step, 13))
            .flatten()
            .and_then(|seconds| {
                let cx = Ctx::new(host.locale(), host.today());
                seconds.value.to_number(&cx).to_f64()
            })
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0);
        let dialog = CustomDialog {
            title,
            message,
            buttons: buttons.iter().map(|(_, b)| b.clone()).collect(),
            inputs,
            close_after,
        };
        let response = match host.show_custom_dialog(&dialog) {
            Ok(response) => response,
            Err(e) => return Exec::from_host(Err(e)),
        };
        if response.button == 0 && close_after.is_some() {
            // Closed by its timer: `Get ( LastMessageChoice )` is 0
            // (Claris), and no button stored the inputs [I].
            self.last_message_choice = Some(0);
            return Exec::ok();
        }
        let chosen = usize::from(response.button.max(1)) - 1;
        let Some((slot, button)) = buttons.get(chosen) else {
            return Exec::error(ErrorCode::INVALID);
        };
        // Get ( LastMessageChoice ) numbers the stored buttons 1-3.
        self.last_message_choice = Some(i64::from(*slot) + 1);
        if button.commit {
            for (target, value) in targets.into_iter().zip(response.inputs) {
                match target {
                    DialogTarget::Variable(name, rep) => {
                        self.variables(host, &name).set(&name, rep, value)
                    }
                    DialogTarget::Field(field) => {
                        if let Err(e) = host.set_field(field, value) {
                            return Exec::from_host(Err(e));
                        }
                    }
                }
            }
        }
        Exec::ok()
    }

    // ----- host steps ---------------------------------------------------------

    /// Steps that go to the host, with their parameters decoded.
    fn host_step(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let id = step.id();
        let dialog = !step.has_flag(flag::NO_INTERACT);
        let select_all = step.has_flag(flag::SELECT_ALL);
        let field = match step.field_spec().filter(|s| !s.is_variable()) {
            Some(spec) => match self.field_target(host, step, &spec) {
                Some(field) => Some(field),
                None => return Exec::error(ErrorCode::FIELD_MISSING),
            },
            None => None,
        };
        let insert = InsertTarget { field, select_all };
        let r = match id {
            // Navigation
            6 => {
                let layout = match step.value {
                    1 => Some(LayoutTarget::Original),
                    3 => self.layout_calc(host, step).map(LayoutTarget::Name),
                    4 => self.layout_calc(host, step).map(|n| {
                        let cx = Ctx::new(host.locale(), host.today());
                        LayoutTarget::Number(Value::text(n).to_number(&cx).to_i64().unwrap_or(0))
                    }),
                    _ => step
                        .member_key()
                        .and_then(|k| k.id())
                        .map(LayoutTarget::Key),
                };
                let Some(layout) = layout else {
                    return Exec::error(ErrorCode::LAYOUT_MISSING);
                };
                host.go_to_layout(&layout, step.list_value(6))
            }
            id::GO_TO_RECORD | id::GO_TO_PORTAL_ROW => {
                let movement = match step.value {
                    1 => RecordMove::First,
                    2 => RecordMove::Last,
                    3 => RecordMove::Previous,
                    4 => RecordMove::Next,
                    5 => RecordMove::Number(self.slot_integer(host, step, 0).unwrap_or(0)),
                    6 => RecordMove::Number(step.target_id().map_or(0, |n| n as i64)),
                    _ => return Exec::error(ErrorCode::INVALID),
                };
                let moved = if id == id::GO_TO_RECORD {
                    host.go_to_record(movement, dialog)
                } else {
                    host.go_to_portal_row(movement, select_all, dialog)
                };
                return match moved {
                    Ok(true) => Exec::ok(),
                    Ok(false) => {
                        // No further record: 101, after leaving the loop (or
                        // the script, where nothing stores it) with Exit
                        // after last (script-runtime.md 13.16).
                        let exits = step.has_flag(flag::EXIT)
                            && matches!(movement, RecordMove::Previous | RecordMove::Next);
                        if exits {
                            self.exit_after_last(host, step);
                        }
                        Exec::error(ErrorCode::RECORD_MISSING)
                    }
                    Err(e) => Exec::from_host(Err(e)),
                };
            }
            17 => host.go_to_field(field, select_all),
            4 | 5 => host.go_to_next_field(id == 4),
            145 => {
                let name = self.slot_text(host, step, 0).unwrap_or_default();
                let repetition = self.slot_integer(host, step, 1).unwrap_or(1).max(1) as u32;
                host.go_to_object(&name, repetition)
            }
            74 => {
                // Slot 0 the new window's name, 1-4 its bounds (11.8).
                let window = std::array::from_fn(|slot| {
                    step.has_flag(flag::TURN_ON)
                        .then(|| self.eval_slot(host, step, slot as u16).map(|e| e.value))
                        .flatten()
                });
                let layout = match step.value {
                    1 => Some(LayoutTarget::Original),
                    3 => self.slot_text(host, step, 5).map(LayoutTarget::Name),
                    4 => self.slot_integer(host, step, 5).map(LayoutTarget::Number),
                    _ => step
                        .member_key()
                        .and_then(|k| k.id())
                        .map(LayoutTarget::Key),
                };
                let occurrence = if step.has_flag(flag::OPTION3) {
                    // "Table by Calculation" (FileMaker Pro 26.1): `0x400`
                    // with the occurrence's name in calculation slot 8 in
                    // place of `[00 0, 11]` (script-steps.md 12.4, from the
                    // binaries). A name no occurrence of the file has is
                    // 106, OpenMaker's choice: FileMaker's is unmeasured.
                    let name = self.slot_text(host, step, 8).unwrap_or_default();
                    match host.names().find_occurrence(&name) {
                        Some(occurrence) => Some(occurrence),
                        None => return Exec::error(ErrorCode::TABLE_MISSING),
                    }
                } else {
                    step.data
                        .as_ref()
                        .and_then(|d| d.slot(0))
                        .and_then(|s| s.child(0x11))
                        .and_then(crate::node_bytes)
                        .and_then(|b| key_id(&b))
                };
                host.go_to_related_record(&RelatedRecords {
                    occurrence,
                    layout,
                    external_layout: step.has_flag(flag::OPTION),
                    options: step.number(0),
                    show_only_related: step.has_flag(flag::RESTORE),
                    match_found_set: step.has_flag(flag::OPTION2),
                    new_window: step.has_flag(flag::TURN_ON),
                    animation: step.list_value(6),
                    window,
                    step,
                })
            }
            228 => {
                // The ids are slot 7. The flag, not slot 0, asks for a new
                // window: Pro keeps the name there when it is turned off
                // (script-steps.md 12.2).
                let ids = self.slot_text(host, step, 7).unwrap_or_default();
                let layout = step
                    .member_key()
                    .and_then(|k| k.id())
                    .map(LayoutTarget::Key);
                let new_window = step.has_flag(flag::TURN_ON).then(|| NewWindow {
                    name: self.slot_text(host, step, 0),
                    bounds: self.bounds(host, step),
                    layout: None,
                    style: 0,
                    options: step.number(0),
                });
                host.go_to_list_of_records(&RecordList {
                    ids,
                    layout,
                    animation: step.list_value(6),
                    new_window,
                })
            }
            22 | 41 | 55 => {
                let mode = match id {
                    22 => Mode::Find,
                    41 => Mode::Preview,
                    _ => Mode::Browse,
                };
                let requests = (id == 22 && step.has_flag(flag::RESTORE))
                    .then(|| step.data_leaf(crate::StepData::REQUESTS))
                    .flatten()
                    .map(|node| self.expand_requests(host, node));
                let pause = step.has_flag(flag::PAUSE_SCRIPT);
                let exec = Exec::from_host(host.enter_mode(mode, pause, requests.as_ref()));
                if pause && !exec.error.is_error() && !exec.pending {
                    return self.pause(host, None);
                }
                return exec;
            }
            169 => host.close_popover(),
            // Records
            7 => host.new_record(),
            8 => host.duplicate_record(),
            9 => host.delete_record(dialog),
            104 => host.delete_portal_row(dialog),
            10 => host.delete_all_records(dialog),
            51 => host.revert_record(dialog),
            75 => host.commit_records(CommitOptions {
                with_dialog: dialog,
                skip_validation: step.has_flag(flag::OPTION),
                override_ess_locking: step.has_flag(flag::OPTION2),
            }),
            133 => host.open_record(),
            101 => host.copy_records(false),
            98 => host.copy_records(true),
            182 => host.truncate_table(step.member_key().and_then(|k| k.id()), dialog),
            // Found sets
            23 => host.show_all_records(),
            27 => host.show_omitted_only(),
            // Omit Record has no dialog option.
            25 => host.omit_records(1, false),
            26 => {
                let count = self.slot_integer(host, step, 0).unwrap_or(1);
                host.omit_records(count, dialog)
            }
            21 => host.unsort_records(),
            24 => host.modify_last_find(),
            28 | 126 | 127 => {
                let kind = match id {
                    126 => FindKind::Constrain,
                    127 => FindKind::Extend,
                    _ => FindKind::Replace,
                };
                let requests = step
                    .has_flag(flag::RESTORE)
                    .then(|| step.data_leaf(crate::StepData::REQUESTS))
                    .flatten()
                    .map(|node| self.expand_requests(host, node));
                if self.in_script() {
                    host.perform_find(kind, requests.as_ref())
                } else {
                    host.perform_find_command(kind, requests.as_ref())
                }
            }
            150 => {
                let text = self.slot_text(host, step, 0).unwrap_or_default();
                host.perform_quick_find(&text)
            }
            155 => {
                let kind = match step.value {
                    2 => FindKind::Constrain,
                    3 => FindKind::Extend,
                    _ => FindKind::Replace,
                };
                host.find_matching(field, kind)
            }
            39 => {
                let order = step
                    .has_flag(flag::RESTORE)
                    .then(|| step.data_leaf(crate::StepData::SORT_ORDER))
                    .flatten();
                host.sort_records(order, dialog)
            }
            154 => host.sort_by_field(field, step.value),
            // Fields
            76 => {
                let Some(value) = self.eval_slot(host, step, 0).map(|e| e.value) else {
                    return Exec::error(ErrorCode::INVALID);
                };
                match field {
                    Some(field) => host.set_field(field, value),
                    None => host.insert(
                        InsertTarget {
                            field: None,
                            select_all: true,
                        },
                        value,
                    ),
                }
            }
            147 => {
                let Some(name) = self.slot_text(host, step, 1) else {
                    return Exec::error(ErrorCode::INVALID);
                };
                let value = self
                    .eval_slot(host, step, 0)
                    .map_or_else(Value::empty_text, |e| e.value);
                host.set_field_by_name(&name, value)
            }
            13 | 14 | 60 => {
                let selector = match id {
                    13 => CURRENT_DATE,
                    14 => CURRENT_TIME,
                    _ => USER_NAME,
                };
                let context = host.calc_context();
                let value = self
                    .calc(host)
                    .get(selector, &context)
                    .unwrap_or_else(|_| Value::empty_text());
                return self.write_target(host, step, value, Write::Insert { select_all });
            }
            61 => {
                let text = step.text(1).or_else(|| step.text(0)).unwrap_or_default();
                return self.write_target(
                    host,
                    step,
                    Value::text(text),
                    Write::Insert { select_all },
                );
            }
            77 => {
                let value = self
                    .eval_slot(host, step, 0)
                    .map_or_else(Value::empty_text, |e| e.value);
                return self.write_target(host, step, value, Write::Insert { select_all });
            }
            11 | 12 => host.insert_from(insert, id == 12),
            46 => host.clipboard(Clipboard::Cut, insert),
            47 => host.clipboard(Clipboard::Copy, insert),
            48 => host.clipboard(
                Clipboard::Paste {
                    no_style: step.has_flag(flag::OPTION2),
                },
                insert,
            ),
            49 => host.clipboard(Clipboard::Clear, insert),
            50 => host.select_all(),
            130 => {
                let start = self.slot_integer(host, step, 0).unwrap_or(0);
                let end = self.slot_integer(host, step, 1).unwrap_or(start);
                host.set_selection(field, start, end)
            }
            45 => host.undo_redo(step.value),
            40 => host.relookup(field, dialog),
            116 => {
                let (Some(field), Some(value)) =
                    (field, self.eval_slot(host, step, 0).map(|e| e.value))
                else {
                    return Exec::error(ErrorCode::INVALID);
                };
                host.set_next_serial_value(field, value)
            }
            132 => {
                let paths = self.paths(host, step);
                host.export_field_contents(field, &paths, step.has_flag(flag::OPTION2))
            }
            56 | 131 | 158 | 159 => {
                let kind = match id {
                    56 => InsertKind::Picture,
                    158 => InsertKind::Pdf,
                    159 => InsertKind::AudioVideo,
                    _ => InsertKind::File,
                };
                // Only Insert File has a target; a variable gets the value.
                let variable = step.field_spec().filter(|s| id == 131 && s.is_variable());
                let request = InsertFile {
                    kind,
                    paths: self.paths(host, step),
                    reference: step.has_flag(flag::STORE_REFERENCE),
                    with_dialog: dialog,
                    target: variable.is_none().then_some(InsertTarget {
                        field,
                        select_all: true,
                    }),
                };
                let value = match host.insert_file(&request) {
                    Ok(value) => value,
                    Err(e) => return Exec::from_host(Err(e)),
                };
                if variable.is_some() {
                    let Some(name) = step.variable_name(0).filter(|n| valid_variable_name(n))
                    else {
                        return Exec::error(ErrorCode::INVALID);
                    };
                    let repetition = i64::from(variable.map_or(1, |s| s.repetition().max(1)));
                    self.variables(host, &name).set(&name, repetition, value);
                }
                Ok(())
            }
            35 | 36 | 143 => {
                let call = self.transfer_call(host, step, dialog);
                if id == 35 {
                    host.import_records(&call)
                } else {
                    host.export_records(&call)
                }
            }
            // Windows
            122 => {
                let name = self.slot_text(host, step, 0);
                let bounds = self.bounds(host, step);
                host.new_window(&NewWindow {
                    name,
                    bounds,
                    layout: step.member_key().and_then(|k| k.id()),
                    style: step.value,
                    options: step.number(0),
                })
            }
            119 | 121 | 123 | 124 => {
                let target = match self.slot_text(host, step, 0) {
                    Some(name) if step.value != 0 || id == 123 && step.value == 1 => {
                        WindowTarget::Named(name)
                    }
                    _ => WindowTarget::Current,
                };
                let current_file = step.has_flag(flag::LIMIT_TO_CURRENT_FILE);
                match id {
                    119 => {
                        let bounds = self.bounds(host, step);
                        host.move_resize_window(&target, bounds, current_file)
                    }
                    121 => host.close_window(&target, current_file),
                    123 => host.select_window(&target, current_file),
                    _ => {
                        let title = self.slot_text(host, step, 1).unwrap_or_default();
                        host.set_window_title(&target, &title, current_file)
                    }
                }
            }
            31 => host.adjust_window(step.value),
            120 => host.arrange_windows(step.value),
            79 => host.freeze_window(),
            80 => host.refresh_window(step.has_flag(flag::OPTION), step.has_flag(flag::OPTION2)),
            81 => host.scroll_window(step.value),
            97 => host.set_zoom_level(step.value, step.has_flag(flag::LOCK)),
            30 => host.view_as(step.value),
            29 | 166 | 92 | 174 => {
                let Some(choice) = visibility(step.value) else {
                    return Exec::error(ErrorCode::INVALID);
                };
                let bar = match id {
                    29 => Bar::Toolbars {
                        edit: step.has_flag(flag::OPTION),
                    },
                    166 => Bar::Menubar,
                    92 => Bar::TextRuler,
                    _ => Bar::TouchKeyboard,
                };
                host.show_hide(bar, choice, step.has_flag(flag::LOCK))
            }
            167 | 180 => {
                let name = self.slot_text(host, step, 0).unwrap_or_default();
                // Unlike Go to Object, zero or no repetition expression
                // refreshes all repetitions. The editor may store an explicit 1.
                let repetition = self.slot_integer(host, step, 1).unwrap_or(0);
                let Ok(repetition) = u32::try_from(repetition) else {
                    return Exec::error(ErrorCode::INVALID);
                };
                host.refresh_object(&name, repetition, id == 180)
            }
            146 => {
                let name = self.slot_text(host, step, 0).unwrap_or_default();
                let url = self.slot_text(host, step, 1);
                let action = step.number(0).map_or(step.value, |n| n as u16);
                host.set_web_viewer(&name, action, url.as_deref())
            }
            175 => {
                let object = self.slot_text(host, step, 0).unwrap_or_default();
                let function = self.slot_text(host, step, 1).unwrap_or_default();
                let count = step.number(0).unwrap_or(0).clamp(0, 64) as u16;
                let args: Vec<Value> = (0..count)
                    .map(|i| {
                        self.eval_slot(host, step, 10 + i)
                            .map_or_else(Value::empty_text, |e| e.value)
                    })
                    .collect();
                host.perform_javascript(&object, &function, &args)
            }
            // Files
            33 | 34 => {
                let path = self.slot_text(host, step, 0);
                let source = match (step.data_leaf(crate::StepData::FILE_REFERENCES), &path) {
                    (Some(reference), _) if step.has_flag(flag::FILE_REFERENCE) => {
                        FileSource::Reference(reference)
                    }
                    (_, Some(path)) => FileSource::Path(path),
                    _ => FileSource::Current,
                };
                if id == 33 {
                    host.open_file_from(self.current_file(), source, step.has_flag(flag::OPTION))
                } else {
                    host.close_file_from(self.current_file(), source)
                }
            }
            37 => {
                let paths = self.paths(host, step);
                host.save_a_copy_as(&paths, step.value, step.has_flag(flag::OPTION2))
            }
            84 => host.set_multi_user(step.value),
            94 => host.set_use_system_formats(step.has_flag(flag::TURN_ON)),
            188 | 189 | 191 => {
                let paths = self.paths(host, step);
                let value = match id {
                    188 => host.file_exists(&paths).map(Value::boolean),
                    189 => host.file_size(&paths).map(Value::number),
                    _ => host.open_data_file(&paths).map(Value::number),
                };
                return self.result_to_target(host, step, value, false);
            }
            190 => {
                let paths = self.paths(host, step);
                host.create_data_file(&paths, step.has_flag(flag::OPTION2))
            }
            192 => {
                let file = self.slot_integer(host, step, 0).unwrap_or(0);
                match self.read_target(host, step) {
                    Ok(data) => host.write_data_file(
                        file,
                        &data,
                        encoding(step.value),
                        step.has_flag(flag::OPTION2),
                    ),
                    Err(e) => Err(e),
                }
            }
            193 => {
                let file = self.slot_integer(host, step, 0).unwrap_or(0);
                let amount = self.slot_integer(host, step, 1);
                let value = host.read_data_file(file, amount, encoding(step.value));
                return self.result_to_target(host, step, value, false);
            }
            194 => {
                let file = self.slot_integer(host, step, 0).unwrap_or(0);
                let value = host.data_file_position(file).map(Value::number);
                return self.result_to_target(host, step, value, false);
            }
            195 => {
                let file = self.slot_integer(host, step, 0).unwrap_or(0);
                let position = self.slot_integer(host, step, 1).unwrap_or(0);
                host.set_data_file_position(file, position)
            }
            196 => {
                let file = self.slot_integer(host, step, 0).unwrap_or(0);
                host.close_data_file(file)
            }
            197 => {
                let paths = self.paths(host, step);
                host.delete_file(&paths)
            }
            199 => {
                let paths = self.paths(host, step);
                let name = self.slot_text(host, step, 0).unwrap_or_default();
                host.rename_file(&paths, &name)
            }
            181 => {
                let title = self.slot_text(host, step, 3).unwrap_or_default();
                let default = self.slot_text(host, step, 4).unwrap_or_default();
                let value =
                    match host.get_folder_path(&title, &default, step.has_flag(flag::OPTION)) {
                        Ok(value) => value,
                        Err(e) => return Exec::from_host(Err(e)),
                    };
                let Some(name) = step.text(0).filter(|n| valid_variable_name(n)) else {
                    return Exec::error(ErrorCode::INVALID);
                };
                let repetition = self.slot_integer(host, step, 2).unwrap_or(1);
                self.variables(host, &name).set(&name, repetition, value);
                Ok(())
            }
            // Network
            111 => {
                let url = self.slot_text(host, step, 0).unwrap_or_default();
                host.open_url(&url, dialog, step.has_flag(flag::OPTION))
            }
            160 => {
                let curl_options = self.slot_text(host, step, 1);
                let variables = curl_options
                    .as_deref()
                    .map(option_variables)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|name| {
                        let value = self
                            .variables(host, &name)
                            .get(&name, 1)
                            .cloned()
                            .unwrap_or_else(Value::empty_text);
                        (name, value)
                    })
                    .collect();
                let request = UrlRequest {
                    url: self.slot_text(host, step, 0).unwrap_or_default(),
                    curl_options,
                    verify_ssl: step.has_flag(flag::VERIFY_SSL_CERTIFICATES),
                    with_dialog: dialog,
                    encode_url: !step.has_flag(flag::OPTION),
                    variables,
                    target: match step.field_spec() {
                        Some(spec) if spec.is_variable() => None,
                        _ => Some(insert),
                    },
                };
                let response = match host.fetch_url(&request) {
                    Ok(response) => response,
                    Err(e) => return Exec::from_host(Err(e)),
                };
                for (name, value) in response.variables {
                    if valid_variable_name(&name) {
                        self.variables(host, &name).set(&name, 1, value);
                    }
                }
                return self.result_to_target(host, step, Ok(response.body), select_all);
            }
            91 | id::REPLACE_FIELD_CONTENTS_BY_NAME => {
                // 249 names its field with the calculation in slot 1, found
                // as Set Field By Name finds its target (script-steps.md
                // 12.4); a name that finds none is error 102 [I].
                let field = if id == id::REPLACE_FIELD_CONTENTS_BY_NAME {
                    let Some(name) = self.slot_text(host, step, 1) else {
                        return Exec::error(ErrorCode::INVALID);
                    };
                    match host.field_by_name(&name) {
                        Ok(field) => Some(field),
                        Err(e) => return Exec::from_host(Err(e)),
                    }
                } else {
                    field
                };
                let mut evaluate;
                let mut with = match step.number(1) {
                    Some(3) => {
                        let Some(calc) = step.calculation(0) else {
                            return Exec::error(ErrorCode::INVALID);
                        };
                        evaluate = move |h: &mut dyn ScriptHost| self.eval(h, &calc).value;
                        Replacement::Calculation(&mut evaluate)
                    }
                    // "Use Entry Options" (`[00 5, 03]`) takes the field's
                    // own serial settings, which are not read here: refused
                    // before any record is written (script-steps.md 11.8).
                    Some(2) if step.number(5).is_some_and(|n| n != 0) => {
                        return Exec::error(ErrorCode::UNAVAILABLE);
                    }
                    Some(2) => Replacement::Serial {
                        initial: step.text(3).unwrap_or_else(|| "1".into()),
                        increment: step.number(2).unwrap_or(1),
                        update_serial: step.number(4).is_some_and(|n| n != 0),
                    },
                    _ => Replacement::CurrentContents,
                };
                host.replace_field_contents(field, &mut with, dialog)
            }
            203 => {
                let request = self.slot_text(host, step, 0).unwrap_or_default();
                let value = host.execute_data_api(&request);
                return self.result_to_target(host, step, value, select_all);
            }
            63 => {
                // Slots of script-steps.md 12.3: To, CC, BCC, Subject and
                // Message calculations 0-4, Collect `[00 0..2, 03]`, one
                // per record `[00 3, 03]`, SMTP `[00 4, 03]`, OAuth
                // `[00 7, 03]`, attachments `[00 0, 82]`.
                // A stored option or attachment list OpenMaker cannot read
                // is 3: read as absent, it could turn SMTP into the client
                // or drop an attachment, a different message than the step
                // describes.
                let stored = |slot: u16, key: u8| {
                    step.data
                        .as_ref()
                        .and_then(|d| d.slot(slot))
                        .and_then(|s| s.child(key))
                        .is_some()
                };
                let mut on = [false; 8];
                for slot in [0u16, 1, 2, 3, 4, 7] {
                    match step.number(slot) {
                        Some(n) => on[usize::from(slot)] = n != 0,
                        None if stored(slot, 0x03) => return Exec::error(ErrorCode::UNAVAILABLE),
                        None => {}
                    }
                }
                let attached =
                    step.has_flag(flag::FILE_PATH) || stored(0, crate::StepData::PATH_STRING);
                let attachments = match path_list(step) {
                    Some(list) => list,
                    None if attached => return Exec::error(ErrorCode::UNAVAILABLE),
                    None => String::new(),
                };
                let via = if on[7] {
                    MailVia::OAuth
                } else if on[4] {
                    MailVia::Smtp
                } else {
                    MailVia::Client
                };
                let mut text = |slot| self.slot_text(host, step, slot).unwrap_or_default();
                let (to, cc, bcc, subject, message) = (text(0), text(1), text(2), text(3), text(4));
                host.send_mail(&Mail {
                    via,
                    to,
                    cc,
                    bcc,
                    subject,
                    message,
                    collect: [on[0], on[1], on[2]],
                    one_per_record: on[3],
                    attachments,
                    with_dialog: dialog,
                })
            }
            // Accounts
            134 | 135 | 136 | 137 | 83 | 138 => {
                let first = self.slot_text(host, step, 0).unwrap_or_default();
                let second = self.slot_text(host, step, 1).unwrap_or_default();
                let option = step.has_flag(flag::OPTION);
                match id {
                    134 => host.add_account(&first, &second, &step.reserved, option),
                    135 => host.delete_account(&first),
                    136 => host.reset_account_password(&first, &second, option),
                    137 => host.enable_account(&first, !option),
                    83 => host.change_password(&first, &second, dialog),
                    _ => host.re_login(&first, &second, dialog),
                }
            }
            // Platform
            93 => host.beep(),
            44 => host.exit_application(),
            102 => host.flush_cache(),
            65 => {
                let number = self.slot_text(host, step, 0).unwrap_or_default();
                host.dial_phone(&number, dialog)
            }
            142 => host.install_menu_set(
                step.member_key().and_then(|k| k.id()),
                step.has_flag(flag::OPTION2),
            ),
            115 => host.allow_formatting_bar(step.has_flag(flag::TURN_ON)),
            157 => host.install_plugin_file(field),
            _ if is_open_menu_item(id) => host.open_menu_item(id),
            _ => return self.other_step(host, step),
        };
        Exec::from_host(r)
    }

    /// A step without a dedicated host method: its stored parameters,
    /// calculations evaluated, through [`ScriptHost::other_step`].
    fn other_step(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Exec {
        let Some(schema) = step.schema() else {
            // Named nowhere: FileMaker's dispatcher has no case (error 3).
            return Exec::error(ErrorCode::UNAVAILABLE);
        };
        let mut args = Vec::new();
        for parameter in step.parameters() {
            // PDF output uses the same path-list variable expansion as data
            // file and transfer steps, before crossing the generic host API.
            if step.id() == 144 && parameter.schema.name == "File paths" {
                args.push(StepArg {
                    parameter: "File paths",
                    location: crate::Location::Key(0x82),
                    value: Arg::Text(self.paths(host, step).join("\n")),
                });
                continue;
            }
            for (location, item) in parameter.parts {
                let Some(item) = item else {
                    continue;
                };
                if let (Item::Calculation(_), crate::Location::Slot { slot, .. }) =
                    (&item, location)
                {
                    if crate::save_records::unused_pdf_calculation(step, slot) {
                        continue;
                    }
                }
                let value = match item {
                    Item::Bool(b) => Arg::Bool(b),
                    Item::Value(v) => Arg::Choice(v),
                    Item::Field(spec) if spec.is_variable() => match step.variable_name(0) {
                        Some(name) => Arg::Variable(name),
                        None => continue,
                    },
                    Item::Field(spec) => match field_ref(&spec, 1) {
                        Some(field) => Arg::Field(field),
                        None => continue,
                    },
                    Item::Key(key) => match key.id() {
                        Some(id) => Arg::Key(id),
                        None => continue,
                    },
                    Item::Reserved(bytes) => Arg::Reserved(*bytes),
                    Item::Calculation(calc) => Arg::Value(self.eval(host, &calc).value),
                    Item::Text(text) => Arg::Text(text),
                    Item::Number(n) => Arg::Number(n),
                    Item::Node(node) => Arg::Node(node),
                };
                args.push(StepArg {
                    parameter: parameter.schema.name.as_str(),
                    location,
                    value,
                });
            }
        }
        let call = StepCall {
            id: step.id(),
            name: schema.name.as_str(),
            step,
            args,
        };
        let has_target = schema
            .params
            .iter()
            .any(|p| p.kind == crate::ParamKind::Target);
        match host.other_step(&call) {
            Ok(Some(value)) if has_target => {
                let select_all = step.has_flag(flag::SELECT_ALL);
                self.write_target(host, step, value, Write::Result { select_all })
            }
            Ok(_) => Exec::ok(),
            Err(e) => Exec::from_host(Err(e)),
        }
    }

    /// Stored find requests as the window gets them: every `$name` and
    /// `$$name` in a criterion replaced by the variable's text, a variable
    /// that does not exist left as written (script-steps.md 11.2.3).
    fn expand_requests(&mut self, host: &mut dyn ScriptHost, node: &OwnedNode) -> OwnedNode {
        crate::specs::expand_request_node(node, |name| {
            let value = self.variables(host, name).get(name, 1).cloned()?;
            Some(self.text_of(host, &value))
        })
    }

    /// A host result to the step's target.
    fn result_to_target(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        value: HostResult<Value>,
        select_all: bool,
    ) -> Exec {
        match value {
            Ok(value) => self.write_target(host, step, value, Write::Result { select_all }),
            Err(e) => Exec::from_host(Err(e)),
        }
    }

    /// The field of a step's inline field spec; a repetition of 0 is
    /// calculated in slot 10 (Set Field, Go to Field; corpus).
    fn field_target(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        spec: &FieldSpec,
    ) -> Option<FieldRef> {
        let repetition = if spec.repetition() > 0 {
            spec.repetition() as u32
        } else {
            self.slot_integer(host, step, 10).unwrap_or(1).max(1) as u32
        };
        field_ref(spec, repetition)
    }

    /// Go to Layout's name or number calculation: slot 5 (corpus), else 0.
    fn layout_calc(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Option<String> {
        self.slot_text(host, step, 5)
            .or_else(|| self.slot_text(host, step, 0))
    }

    /// A window step's bounds: height, width, top and left in slots 1-4.
    fn bounds(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Bounds {
        Bounds {
            height: self.slot_float(host, step, 1),
            width: self.slot_float(host, step, 2),
            top: self.slot_float(host, step, 3),
            left: self.slot_float(host, step, 4),
        }
    }

    /// A step's file path list (`[00 0, 82]`, or a step-level `82`), one
    /// path per line. `$name` and `$$name` anywhere in a line (a name ends
    /// at `/`, `\`, `:` or the end of the line) are replaced by the
    /// variable's value, empty when it does not exist, without rescanning;
    /// a value of several lines is several paths (Claris "Creating file
    /// paths": `file:testing/$fileName`, `$chosenType:/$driveLetter/...`,
    /// `$searchList`).
    /// The parameters of Import Records, Export Records and Save Records as
    /// Excel ([`TransferCall`]).
    fn transfer_call<'s>(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &'s ScriptStep,
        with_dialog: bool,
    ) -> TransferCall<'s> {
        let leaf = |key: u8| {
            step.data_leaf(key)
                .and_then(crate::node_bytes)
                .map(|b| b.into_owned())
        };
        let slots = if step.id() == 143 {
            (0..6)
                .map(|slot| self.eval_slot(host, step, slot).map(|e| e.value))
                .collect()
        } else {
            Vec::new()
        };
        TransferCall {
            id: step.id(),
            step,
            paths: self.paths(host, step),
            with_dialog,
            restore: step.has_flag(flag::RESTORE),
            create_folders: step.has_flag(flag::OPTION2),
            open_automatically: step.has_flag(flag::OPEN_AUTOMATICALLY),
            create_email: step.has_flag(flag::CREATE_EMAIL),
            verify_ssl: step.has_flag(flag::VERIFY_SSL_CERTIFICATES),
            source_kind: step.value,
            import_order: leaf(0x84),
            export_order: leaf(0x85),
            profile: leaf(0x86),
            profile_ex: if step.id() == 35 { leaf(0x87) } else { None },
            field_flags: leaf(0xf0),
            slots,
        }
    }

    fn paths(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) -> Vec<String> {
        let text = path_list(step).unwrap_or_default();
        let mut paths = Vec::new();
        for line in text.split(['\r', '\n']).filter(|l| !l.trim().is_empty()) {
            let mut expanded = String::new();
            let mut rest = line;
            while let Some(at) = rest.find('$') {
                expanded.push_str(&rest[..at]);
                let after = &rest[at..];
                let sigil = if after.starts_with("$$") { 2 } else { 1 };
                let starts_name = after[sigil..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_');
                if !starts_name {
                    expanded.push('$');
                    rest = &after[1..];
                    continue;
                }
                let end = after[sigil..]
                    .find(['/', '\\', ':'])
                    .map_or(after.len(), |i| i + sigil);
                let name = after[..end].trim_end();
                let value = self
                    .variables(host, name)
                    .get(name, 1)
                    .cloned()
                    .unwrap_or_else(Value::empty_text);
                expanded.push_str(&self.text_of(host, &value));
                expanded.push_str(&after[name.len()..end]);
                rest = &after[end..];
            }
            expanded.push_str(rest);
            paths.extend(
                expanded
                    .split(['\r', '\n'])
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned),
            );
        }
        paths
    }
}

/// A step's file path list as text, one path per line. FileMaker stores the
/// list as a path stream (script-steps.md 11.0); a list written as text
/// reads as text.
pub(crate) fn path_list(step: &ScriptStep) -> Option<String> {
    let stream = step
        .data
        .as_ref()
        .and_then(|d| d.slot(0))
        .and_then(|slot| slot.child(crate::StepData::PATH_STRING))
        .and_then(crate::node_bytes)
        .and_then(|bytes| path_stream(&bytes));
    stream
        .or_else(|| {
            step.item(&crate::Location::Slot {
                slot: 0,
                kind: crate::SlotKind::Path,
            })
            .and_then(|item| match item {
                Item::Text(text) => Some(text),
                _ => None,
            })
        })
        .or_else(|| {
            step.data_leaf(crate::StepData::PATH_STRING)
                .and_then(crate::node_text)
        })
}

/// A path list stream (`UniversalPathListToStream`, script-steps.md 11.0):
/// a u32 count, then per path its kind (`file`, `filemac`, ...), a volume
/// and the path, as stream strings; the lines `kind:path` (`kind:/volume
/// path` with a volume) joined by returns. `None` when the bytes are not
/// one.
fn path_stream(bytes: &[u8]) -> Option<String> {
    let count = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    if count > bytes.len() {
        return None;
    }
    let mut at = 4;
    let mut string = || -> Option<String> {
        let first = *bytes.get(at)?;
        at += 1;
        let n = match first {
            0..=0x7f => usize::from(first),
            0x80..=0xbf => {
                let low = *bytes.get(at)?;
                at += 1;
                (usize::from(first & 0x3f) << 8) | usize::from(low)
            }
            _ => return None,
        };
        let text = om_scsu::decode_scsu(bytes.get(at..at + n)?).ok()?;
        at += n;
        Some(text)
    };
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        let (kind, volume, path) = (string()?, string()?, string()?);
        lines.push(if volume.is_empty() {
            format!("{kind}:{path}")
        } else {
            format!("{kind}:/{volume}{path}")
        });
    }
    (at == bytes.len()).then(|| lines.join("\r"))
}

/// The variables a cURL option string names (`$name`, `$$name`, also
/// after `@` or `name@`), in order, without repeats.
fn option_variables(options: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let chars: Vec<char> = options.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let start = i;
            i += 1;
            if chars.get(i) == Some(&'$') {
                i += 1;
            }
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            if valid_variable_name(&name) && !names.contains(&name) {
                names.push(name);
            }
        } else {
            i += 1;
        }
    }
    names
}

/// A dialog input's target.
enum DialogTarget {
    Variable(String, i64),
    Field(FieldRef),
}

/// A field spec as a [`FieldRef`]; `repetition` replaces a stored 0.
fn field_ref(spec: &FieldSpec, repetition: u32) -> Option<FieldRef> {
    Some(FieldRef {
        occurrence: spec.table().id()?,
        field: spec.field().id()?,
        repetition: if spec.repetition() > 0 {
            spec.repetition() as u32
        } else {
            repetition.max(1)
        },
    })
}

/// A key stored as bytes: `[len][seglen][segment]` or `[seglen][segment]`.
fn key_id(bytes: &[u8]) -> Option<u64> {
    let mut padded = bytes.to_vec();
    padded.resize(6.max(bytes.len()), 0);
    crate::StoredKey::from_bytes(&padded)
        .and_then(|k| k.id())
        .or_else(|| {
            let (&len, segment) = bytes.split_first()?;
            (usize::from(len) == segment.len() && !segment.is_empty())
                .then(|| om_hbam::slot::be_uint(segment))
                .flatten()
        })
}

/// Steps whose errors come from the data layer and so fail an open
/// transaction (`SetTransactionError`'s callers, script-runtime.md 8.4): the
/// record steps, field writes, inserts into fields, imports, replaces and
/// relookups.
fn writes_records(id: u16) -> bool {
    matches!(
        id,
        7..=14
            | 35
            | 40
            | 46
            | 48
            | 49
            | 56
            | 60
            | 61
            | 75
            | 76
            | 77
            | 91
            | 104
            | 128
            | 131
            | 133
            | 147
            | 158..=161
            | 215
            | 216
            | 240
            | 241
            | 249
    )
}

/// The "Open Menu Item" steps (catalogs/script-steps.csv category).
fn is_open_menu_item(id: u16) -> bool {
    crate::step_info(id).is_some_and(|info| info.category == "Open Menu Item")
}

/// `DBVarMap::ValidVariableName`: `$` or `$$` and a calculation name
/// (script-runtime.md 5.2).
pub(crate) fn valid_variable_name(name: &str) -> bool {
    let base = name
        .strip_prefix("$$")
        .or_else(|| name.strip_prefix('$'))
        .unwrap_or("");
    let delimiter = |c: char| {
        matches!(
            c,
            '"' | '&'
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | '-'
                | '/'
                | ';'
                | '<'
                | '='
                | '>'
                | '['
                | '\\'
                | ']'
                | '^'
                | '{'
                | '}'
                | '≠'
                | '≤'
                | '≥'
                | '¶'
                | '$'
                | ':'
        )
    };
    !base.is_empty()
        && !base.starts_with(|c: char| c.is_whitespace() || c.is_ascii_digit())
        && !base.ends_with(char::is_whitespace)
        && !base.chars().any(delimiter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_streams_read_as_path_lists() {
        // Export Records of probe battery x01, as FileMaker's XML importer
        // stored it: two paths, `file:$p0` and `file:$p1`.
        let masked = |s: &str| -> Vec<u8> {
            let mut out = vec![s.len() as u8];
            out.extend(s.bytes().map(|b| b ^ 0x5a));
            out
        };
        let mut bytes = 2u32.to_be_bytes().to_vec();
        for p in ["$p0", "$p1"] {
            bytes.extend(masked("file"));
            bytes.extend(masked(""));
            bytes.extend(masked(p));
        }
        assert_eq!(path_stream(&bytes).as_deref(), Some("file:$p0\rfile:$p1"));
        let mut mac = 1u32.to_be_bytes().to_vec();
        mac.extend(masked("filemac"));
        mac.extend(masked("Macintosh HD"));
        mac.extend(masked("/Users/a.tab"));
        assert_eq!(
            path_stream(&mac).as_deref(),
            Some("filemac:/Macintosh HD/Users/a.tab")
        );
        // Masked text is not a stream.
        assert_eq!(path_stream(&masked("file:a.tab")[1..]), None);
        assert_eq!(path_stream(&bytes[..bytes.len() - 1]), None);
    }

    #[test]
    fn variable_names_follow_the_calculation_name_rules() {
        assert!(valid_variable_name("$x"));
        assert!(valid_variable_name("$$Count 2"));
        assert!(valid_variable_name("$a b"));
        for bad in [
            "x", "$", "$$", "$1x", "$$2", "$a+b", "$x[2]", "$ x", "$a::b", "$$$x",
        ] {
            assert!(!valid_variable_name(bad), "{bad}");
        }
    }

    #[test]
    fn curl_options_name_their_variables() {
        assert_eq!(
            option_variables("-X POST -d @$body -D $$headers --data-urlencode q@$q -d $body"),
            ["$body", "$$headers", "$q"]
        );
        assert!(option_variables("--data price=5$").is_empty());
    }

    #[test]
    fn keys_read_with_or_without_the_length_byte() {
        assert_eq!(key_id(&[4, 3, 0xd0, 0, 5]), Some(0xd00005));
        assert_eq!(key_id(&[3, 0xd0, 0, 5]), Some(0xd00005));
    }
}
