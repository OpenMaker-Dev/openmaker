//! The calculation host the evaluator sees during a script: the host's file
//! and session, with the running frame's `$` variables, the file's `$$`
//! variables and the script `Get` selectors answered by the runtime
//! (script-runtime.md 3.8, 4.6, 5; calc-values.md 8.3).

use std::sync::Arc;

use om_calc::eval::{
    CalcHost, CustomFunction, FieldId, FieldInfo, HostFunction, LookupDirection, VarScope,
    Variables,
};
use om_calc::{CalcError, Date, Locale, Value};
use om_formula::Names;

use super::host::ScriptHost;
use super::trigger::TriggerInfo;

/// `Get` selectors the runtime answers (catalogs/get-selectors.csv).
pub mod selector {
    pub const LAST_ERROR: u8 = 3;
    pub const SCRIPT_NAME: u8 = 26;
    pub const LAST_MESSAGE_CHOICE: u8 = 33;
    pub const LAST_ERROR_DETAIL: u8 = 41;
    pub const SCRIPT_PARAMETER: u8 = 62;
    pub const ALLOW_ABORT_STATE: u8 = 70;
    pub const ERROR_CAPTURE_STATE: u8 = 71;
    pub const SCRIPT_RESULT: u8 = 80;
    pub const TRIGGER_MODIFIER_KEYS: u8 = 91;
    pub const TRIGGER_KEYSTROKE: u8 = 92;
    pub const TRIGGER_CURRENT_PANEL: u8 = 97;
    pub const TRIGGER_TARGET_PANEL: u8 = 98;
    pub const TRIGGER_GESTURE_INFO: u8 = 107;
    pub const SCRIPT_ANIMATION_STATE: u8 = 109;
    pub const TRIGGER_EXTERNAL_EVENT: u8 = 116;
    pub const SESSION_IDENTIFIER: u8 = 129;
    pub const TRANSACTION_OPEN_STATE: u8 = 132;
    pub const LAST_ERROR_LOCATION: u8 = 134;
    pub const REVERT_TRANSACTION_ON_ERROR_STATE: u8 = 136;
}

/// The script state `Get` reads, captured for one evaluation.
#[derive(Clone, Debug, Default)]
pub(crate) struct ScriptState {
    pub script_name: Option<Arc<str>>,
    pub parameter: Option<Arc<Value>>,
    pub result: Option<Arc<Value>>,
    pub last_error: i32,
    pub last_error_detail: String,
    pub last_error_location: String,
    pub capture: bool,
    pub allow_abort: bool,
    pub revert_on_error: bool,
    pub animation: bool,
    pub transaction_open: bool,
    pub last_message_choice: Option<i64>,
    pub session_identifier: Option<Value>,
    pub trigger: Option<Arc<TriggerInfo>>,
}

impl ScriptState {
    /// The runtime's answer to `Get ( selector )`, or `None` for the host.
    fn get(&self, selector: u8) -> Option<Value> {
        use selector::*;
        let flag = |b: bool| Some(Value::boolean(b));
        let text = |t: Option<&str>| Some(Value::text(t.unwrap_or_default()));
        let shared =
            |v: &Option<Arc<Value>>| Some(v.as_deref().cloned().unwrap_or_else(Value::empty_text));
        let trigger = self.trigger.as_deref();
        match selector {
            LAST_ERROR => Some(Value::number(i64::from(self.last_error))),
            LAST_ERROR_DETAIL => text(Some(&self.last_error_detail)),
            LAST_ERROR_LOCATION => text(Some(&self.last_error_location)),
            SCRIPT_NAME => text(self.script_name.as_deref()),
            SCRIPT_PARAMETER => shared(&self.parameter),
            SCRIPT_RESULT => shared(&self.result),
            ALLOW_ABORT_STATE => flag(self.allow_abort),
            ERROR_CAPTURE_STATE => flag(self.capture),
            REVERT_TRANSACTION_ON_ERROR_STATE => flag(self.revert_on_error),
            SCRIPT_ANIMATION_STATE => flag(self.animation),
            TRANSACTION_OPEN_STATE => flag(self.transaction_open),
            // 0 until a dialog is answered (script-runtime.md 13.1).
            LAST_MESSAGE_CHOICE => Some(Value::number(self.last_message_choice.unwrap_or(0))),
            SESSION_IDENTIFIER => self.session_identifier.clone(),
            TRIGGER_KEYSTROKE => {
                trigger.map(|t| Value::text(t.keystroke.clone().unwrap_or_default()))
            }
            TRIGGER_MODIFIER_KEYS => trigger.and_then(|t| t.modifier_keys).map(Value::number),
            TRIGGER_CURRENT_PANEL => {
                trigger.map(|t| Value::text(t.current_panel.clone().unwrap_or_default()))
            }
            TRIGGER_TARGET_PANEL => {
                trigger.map(|t| Value::text(t.target_panel.clone().unwrap_or_default()))
            }
            TRIGGER_GESTURE_INFO => {
                trigger.map(|t| Value::text(t.gesture.clone().unwrap_or_default()))
            }
            TRIGGER_EXTERNAL_EVENT => trigger.and_then(|t| t.external_event).map(Value::number),
            _ => None,
        }
    }
}

/// [`CalcHost`] over a [`ScriptHost`] with the runtime's variables and
/// script state.
pub(crate) struct ScriptCalc<'a> {
    pub host: &'a mut dyn ScriptHost,
    pub locals: &'a mut Variables,
    pub globals: &'a mut Variables,
    pub state: ScriptState,
    /// A script step's calculation, which may act on files through the
    /// plug-in subset; false outside a step (`Runtime::calc_host`).
    pub effects: bool,
}

impl ScriptCalc<'_> {
    /// A command of the MBS plug-in subset (`super::plugins`), where the
    /// host keeps plug-in state; `None` for any other call.
    fn plugin(
        &mut self,
        function: &HostFunction<'_>,
        args: &[Value],
    ) -> Option<Result<Value, CalcError>> {
        let HostFunction::Plugin(id) = function else {
            return None;
        };
        let locale = self.host.locale().clone();
        let cx = om_calc::Ctx::new(&locale, self.host.today());
        super::plugins::call(&mut *self.host, &cx, *id, args, self.effects)
    }
}

impl CalcHost for ScriptCalc<'_> {
    fn locale(&self) -> &Locale {
        self.host.locale()
    }

    fn today(&self) -> Date {
        self.host.today()
    }

    fn names(&self) -> &dyn Names {
        self.host.names()
    }

    /// The script's abort and time limit stop its calculations too.
    fn interrupted(&self) -> bool {
        self.host.interrupted()
    }

    fn limits(&self) -> om_calc::eval::CalcLimits {
        self.host.limits()
    }

    fn names_for(&self, context: &om_calc::eval::Context) -> &dyn Names {
        self.host.names_for(context)
    }

    fn locale_for(&self, context: &om_calc::eval::Context) -> &Locale {
        self.host.locale_for(context)
    }

    fn table_file(&self, occurrence: u64) -> Option<u64> {
        self.host.table_file(occurrence)
    }

    fn variable_name<'n>(
        &self,
        name: &'n str,
        context: &om_calc::eval::Context,
    ) -> std::borrow::Cow<'n, str> {
        self.host.variable_name(name, context)
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        self.host.field(occurrence, field)
    }

    fn evaluate_always(&self, occurrence: u64, field: u64) -> bool {
        self.host.evaluate_always(occurrence, field)
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        self.host.field_value(occurrence, record, field, repetition)
    }

    fn field_value_in_context(
        &self,
        context: &om_calc::eval::Context,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        self.host
            .field_value_in_context(context, occurrence, record, field, repetition)
    }

    fn relationship_origin(
        &self,
        context: &om_calc::eval::Context,
        related: u64,
    ) -> Option<(u64, u64)> {
        self.host.relationship_origin(context, related)
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        self.host.related_records(occurrence, record, related)
    }

    fn related_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        index: usize,
    ) -> Result<Option<u64>, CalcError> {
        self.host.related_record(occurrence, record, related, index)
    }

    fn referenced_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Option<u64>, CalcError> {
        self.host.referenced_record(occurrence, record, related)
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        self.host.found_record(occurrence, index)
    }

    fn lookup_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        direction: Option<LookupDirection>,
    ) -> Result<Option<u64>, CalcError> {
        self.host
            .lookup_record(occurrence, record, related, direction)
    }

    fn summary(
        &self,
        context: &om_calc::eval::Context,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Value, CalcError> {
        self.host.summary(context, summary, break_field)
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        self.host.base_table_name(occurrence)
    }

    fn table_name(&self, occurrence: u64) -> Option<String> {
        self.host.table_name(occurrence)
    }

    fn qualified_field_name(
        &self,
        context: &om_calc::eval::Context,
        occurrence: u64,
        field: u64,
    ) -> Option<String> {
        self.host.qualified_field_name(context, occurrence, field)
    }

    fn get(&mut self, selector: u8, context: &om_calc::eval::Context) -> Result<Value, CalcError> {
        match self.state.get(selector) {
            Some(value) => Ok(value),
            None => self.host.get(selector, context),
        }
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        match scope {
            VarScope::Local => self.locals,
            VarScope::Global => self.globals,
        }
    }

    fn custom_function(&self, id: u64) -> Option<Arc<CustomFunction>> {
        self.host.custom_function(id)
    }

    fn random(&mut self) -> f64 {
        self.host.random()
    }

    fn container_data(
        &mut self,
        container: &om_calc::Container,
        context: &om_calc::eval::Context,
    ) -> Result<om_calc::eval::StoredContainer, CalcError> {
        self.host.container_data(container, context)
    }

    fn call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &om_calc::eval::Context,
    ) -> Result<Value, CalcError> {
        if let Some(result) = self.plugin(&function, args) {
            return result;
        }
        self.host.call(function, args, context)
    }

    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &om_calc::eval::Context,
    ) -> (Value, Option<CalcError>) {
        match self.plugin(&function, args) {
            Some(Ok(value)) => (value, None),
            Some(Err(error)) => (Value::invalid(), Some(error)),
            None => self.host.call_reporting(function, args, context),
        }
    }
}

#[cfg(test)]
mod portal_tests {
    use super::super::host::*;
    use super::*;
    use om_calc::eval::{evaluate, Context, NullHost};
    use om_formula::{catalog::Opcode, Expr, Field};

    struct Host(NullHost);
    impl CalcHost for Host {
        fn locale(&self) -> &Locale {
            self.0.locale()
        }
        fn today(&self) -> Date {
            self.0.today()
        }
        fn variables(&mut self, scope: VarScope) -> &mut Variables {
            self.0.variables(scope)
        }
        fn relationship_origin(&self, context: &Context, _: u64) -> Option<(u64, u64)> {
            context
                .portal
                .or_else(|| context.occurrence.zip(context.record))
        }
        fn field(&self, _: u64, _: u64) -> Option<FieldInfo> {
            Some(FieldInfo::stored(om_calc::ValueType::Number, 1))
        }
        fn field_value(&self, _: u64, _: u64, _: u64, _: u32) -> Result<Value, CalcError> {
            Ok(Value::number(1))
        }
        fn field_value_in_context(
            &self,
            context: &Context,
            _: u64,
            _: u64,
            _: u64,
            _: u32,
        ) -> Result<Value, CalcError> {
            Ok(Value::number(if context.portal == Some((2, 22)) {
                7
            } else {
                99
            }))
        }
        fn related_records(&self, from: u64, record: u64, _: u64) -> Result<Vec<u64>, CalcError> {
            Ok(if (from, record) == (2, 22) {
                vec![20]
            } else {
                vec![10, 20]
            })
        }
    }
    impl Navigation for Host {}
    impl Records for Host {}
    impl Fields for Host {}
    impl FoundSets for Host {}
    impl Windows for Host {}
    impl Dialogs for Host {}
    impl Files for Host {}
    impl Network for Host {}
    impl Accounts for Host {}
    impl Platform for Host {}
    impl ScriptHost for Host {}
    #[test]
    fn runtime_calculation_keeps_the_hosts_portal_relationship_origin() {
        let mut host = Host(NullHost::new(
            Locale::en_us(),
            Date::from_ymd(2026, 1, 2).unwrap(),
        ));
        let mut runtime = super::super::Runtime::default();
        let mut wrapped = runtime.calc_host(&mut host);
        let expression = Expr::Call {
            op: Opcode(128),
            args: vec![Expr::Field(Field::new(3, 1, false))],
        };
        let result = evaluate(
            &expression,
            &mut wrapped,
            &Context::record(1, 1).with_portal(2, 22),
        );
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(om_calc::eval::display(&result.value, &wrapped), "1");
        let result = evaluate(
            &Expr::Field(Field::new(3, 1, false)),
            &mut wrapped,
            &Context::record(1, 1).with_portal(2, 22),
        );
        assert!(result.error.is_none());
        assert_eq!(om_calc::eval::display(&result.value, &wrapped), "7");
    }
}
