//! Script triggers (script-runtime.md 7): event ids, which run before their
//! event and can cancel it, and what the host passes when an event happens.

use om_calc::Value;

use super::host::{FileId, Mode, WindowId};
use crate::Calculation;

/// `TriggerEventID` (script-runtime.md 7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TriggerEvent {
    ObjectEnter,
    ObjectExit,
    ObjectModify,
    ObjectSave,
    ObjectKeystroke,
    ObjectValidate,
    PanelSwitch,
    ObjectAvPlayerChange,
    LayoutEnter,
    LayoutKeystroke,
    RecordLoad,
    RecordCommit,
    RecordRevert,
    ModeEnter,
    ModeExit,
    /// 108, an old view-change event.
    OldViewChange,
    LayoutExit,
    ViewChange,
    LayoutSizeChange,
    GestureTap,
    ExternalCommandReceived,
    FirstWindowOpen,
    LastWindowClose,
    /// Install OnTimer Script's timer.
    Timer,
    WindowOpen,
    WindowClose,
    FileAvPlayerChange,
    /// A region monitor (location) event.
    Location,
    /// The callback script of Perform Script on Server with Callback.
    PsosCallback,
    WindowTransaction,
}

impl TriggerEvent {
    pub const ALL: [TriggerEvent; 30] = [
        TriggerEvent::ObjectEnter,
        TriggerEvent::ObjectExit,
        TriggerEvent::ObjectModify,
        TriggerEvent::ObjectSave,
        TriggerEvent::ObjectKeystroke,
        TriggerEvent::ObjectValidate,
        TriggerEvent::PanelSwitch,
        TriggerEvent::ObjectAvPlayerChange,
        TriggerEvent::LayoutEnter,
        TriggerEvent::LayoutKeystroke,
        TriggerEvent::RecordLoad,
        TriggerEvent::RecordCommit,
        TriggerEvent::RecordRevert,
        TriggerEvent::ModeEnter,
        TriggerEvent::ModeExit,
        TriggerEvent::OldViewChange,
        TriggerEvent::LayoutExit,
        TriggerEvent::ViewChange,
        TriggerEvent::LayoutSizeChange,
        TriggerEvent::GestureTap,
        TriggerEvent::ExternalCommandReceived,
        TriggerEvent::FirstWindowOpen,
        TriggerEvent::LastWindowClose,
        TriggerEvent::Timer,
        TriggerEvent::WindowOpen,
        TriggerEvent::WindowClose,
        TriggerEvent::FileAvPlayerChange,
        TriggerEvent::Location,
        TriggerEvent::PsosCallback,
        TriggerEvent::WindowTransaction,
    ];

    /// The event id the runtime and stored trigger records use.
    pub fn id(self) -> u16 {
        use TriggerEvent::*;
        match self {
            ObjectEnter => 1,
            ObjectExit => 2,
            ObjectModify => 3,
            ObjectSave => 4,
            ObjectKeystroke => 5,
            ObjectValidate => 6,
            PanelSwitch => 7,
            ObjectAvPlayerChange => 8,
            LayoutEnter => 101,
            LayoutKeystroke => 102,
            RecordLoad => 103,
            RecordCommit => 104,
            RecordRevert => 105,
            ModeEnter => 106,
            ModeExit => 107,
            OldViewChange => 108,
            LayoutExit => 109,
            ViewChange => 110,
            LayoutSizeChange => 111,
            GestureTap => 112,
            ExternalCommandReceived => 113,
            FirstWindowOpen => 201,
            LastWindowClose => 202,
            Timer => 203,
            WindowOpen => 204,
            WindowClose => 205,
            FileAvPlayerChange => 206,
            Location => 207,
            PsosCallback => 208,
            WindowTransaction => 209,
        }
    }

    pub fn from_id(id: u16) -> Option<Self> {
        TriggerEvent::ALL.into_iter().find(|e| e.id() == id)
    }

    /// `ScriptTrigger::IsDelayedTrigger`: the script runs after the event
    /// is processed, queued behind running scripts; the others run before
    /// it and their result decides whether it happens.
    pub fn is_delayed(self) -> bool {
        use TriggerEvent::*;
        matches!(
            self,
            ObjectEnter
                | ObjectModify
                | ObjectAvPlayerChange
                | LayoutEnter
                | RecordLoad
                | ModeEnter
                | OldViewChange
                | ViewChange
                | LayoutSizeChange
                | FirstWindowOpen
                | Timer
                | WindowOpen
                | FileAvPlayerChange
                | PsosCallback
                | WindowTransaction
        )
    }

    /// Whether `Exit Script [False]` cancels the event (the pre-event
    /// triggers, per the Claris trigger pages; OnExternalCommandReceived
    /// runs after its event).
    pub fn can_cancel(self) -> bool {
        !self.is_delayed()
            && !matches!(
                self,
                TriggerEvent::ExternalCommandReceived | TriggerEvent::Location
            )
    }

    /// File and window events (2xx), which additionally need no script
    /// running or queued (`OKToFireWindowTrigger`, 7.3 step 6). Timers and
    /// PSOS callbacks are queued behind running scripts instead.
    pub fn is_window_event(self) -> bool {
        matches!(
            self,
            TriggerEvent::FirstWindowOpen
                | TriggerEvent::LastWindowClose
                | TriggerEvent::WindowOpen
                | TriggerEvent::WindowClose
                | TriggerEvent::FileAvPlayerChange
                | TriggerEvent::WindowTransaction
        )
    }
}

/// What the script of a trigger can ask about its event
/// (`Get ( TriggerKeystroke )` and friends).
#[derive(Clone, Debug, Default)]
pub struct TriggerInfo {
    pub keystroke: Option<String>,
    pub modifier_keys: Option<i64>,
    pub current_panel: Option<String>,
    pub target_panel: Option<String>,
    pub gesture: Option<String>,
    pub external_event: Option<i64>,
}

/// An event that has a trigger script: the trigger record of the layout or
/// object (script-runtime.md 7.2) and the state the host knows at fire
/// time.
#[derive(Clone, Debug)]
pub struct Trigger {
    pub event: TriggerEvent,
    pub file: FileId,
    pub script: u64,
    /// A data-source id in `file` when the script belongs to another file.
    /// Resolve only after the event's firing checks, using the same ordered
    /// path search as an external Perform Script.
    pub file_reference: Option<u64>,
    /// The trigger's parameter calculation, evaluated when it fires.
    pub parameter: Option<Calculation>,
    /// The window the event happened in.
    pub window: Option<WindowId>,
    /// The trigger record's enabled modes (1 Browse, 2 Find, 4 Preview).
    pub modes: u32,
    /// The window's mode when the event happened.
    pub mode: Mode,
    /// The event's default action, as a host token: performed when the
    /// script ends with a true (or no) result, dropped otherwise.
    pub action: Option<u64>,
    /// A script step caused the event (triggers still fire while the
    /// debugger disables user triggers).
    pub from_step: bool,
    pub info: TriggerInfo,
    /// Preset `Get ( LastError )` and `Get ( ScriptResult )` of the
    /// script (the PSOS callback: the server script's error and result).
    pub preset_error: Option<i32>,
    pub preset_result: Option<Value>,
    /// The script's parameter when the event supplies it rather than a
    /// stored calculation (OnWindowTransaction's JSON, script-runtime.md
    /// 7.1); [`Self::parameter`] goes first.
    pub preset_parameter: Option<Value>,
}

impl Trigger {
    /// A trigger enabled in every mode, without an action.
    pub fn new(event: TriggerEvent, file: FileId, script: u64) -> Self {
        Trigger {
            event,
            file,
            script,
            file_reference: None,
            parameter: None,
            window: None,
            modes: 7,
            mode: Mode::Browse,
            action: None,
            from_step: false,
            info: TriggerInfo::default(),
            preset_error: None,
            preset_result: None,
            preset_parameter: None,
        }
    }
}

/// The default action of an event after its trigger script ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriggerAction {
    pub event: TriggerEvent,
    pub action: u64,
    /// The script's result was true, or it ran without Exit Script, or the
    /// trigger did not fire: perform the action. False: cancel the event.
    pub proceed: bool,
}

/// What [`super::Runtime::fire_trigger`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fired {
    /// The trigger ran or will run; its action follows its script.
    Started,
    /// Queued behind the running script.
    Queued,
    /// The trigger did not fire; its action (if any) was performed at once.
    Suppressed(Suppressed),
}

/// Why a trigger did not fire (script-runtime.md 7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suppressed {
    /// The trigger record is not enabled in the window's mode.
    Mode,
    /// All triggers are off (the server disables them after an unknown
    /// step aborted a script).
    MasterSwitch,
    /// The debugger disabled user triggers and no step caused the event.
    UserTriggersDisabled,
    /// A window trigger while a script runs, is queued or is paused.
    ScriptRunning,
    /// The script could not be called (missing, no access, ...).
    CallFailed(i32),
}
