//! `Get` selectors a client answers from its session: the account, the file,
//! the window and layout, the found set and record, the system
//! (calc-functions.md 6; selector numbers of `get-selectors.csv`).
//! A desktop client's answers are FileMaker Pro 26.0.3's for a local file
//! (6.3): the window from the desktop area ([`WindowInfo`]), focus and
//! selection, Find mode's requests, the record's open state and changed
//! fields, Preview's page, and the machine ([`Machine`]).
//!
//! Script selectors (`ScriptParameter`, `LastError`, ...) belong to the
//! script runtime and `CalculationRepetitionNumber` to the evaluator.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use om_calc::eval::Context;
use om_calc::{CalcError, Date, Locale, Number, Time, TimeStyle, Timestamp, Value};

use super::machine::{self, Machine};
use super::schema::CalcSchema;
use super::window::WindowInfo;
use super::{FileCalc, NO_RECORD};

/// Selector numbers (`get-selectors.csv`).
pub mod selector {
    pub const APPLICATION_VERSION: u8 = 1;
    pub const CURRENT_DATE: u8 = 2;
    pub const ACTIVE_FIELD_NAME: u8 = 4;
    pub const FILE_NAME: u8 = 5;
    pub const FILE_SIZE: u8 = 6;
    pub const FOUND_COUNT: u8 = 7;
    pub const HOST_NAME: u8 = 8;
    pub const LAYOUT_COUNT: u8 = 9;
    pub const LAYOUT_NAME: u8 = 10;
    pub const LAYOUT_NUMBER: u8 = 11;
    pub const SYSTEM_LANGUAGE: u8 = 12;
    pub const WINDOW_MODE: u8 = 13;
    pub const MULTI_USER_STATE: u8 = 14;
    pub const PAGE_NUMBER: u8 = 15;
    pub const SYSTEM_PLATFORM: u8 = 16;
    pub const ACTIVE_PORTAL_ROW_NUMBER: u8 = 17;
    pub const PRINTER_NAME: u8 = 18;
    pub const TOTAL_RECORD_COUNT: u8 = 19;
    pub const RECORD_NUMBER: u8 = 20;
    pub const ACTIVE_REPETITION_NUMBER: u8 = 21;
    pub const REQUEST_COUNT: u8 = 22;
    pub const SCREEN_DEPTH: u8 = 23;
    pub const SCREEN_HEIGHT: u8 = 24;
    pub const SCREEN_WIDTH: u8 = 25;
    pub const SORT_STATE: u8 = 27;
    pub const SYSTEM_VERSION: u8 = 28;
    pub const CURRENT_TIME: u8 = 29;
    pub const USER_COUNT: u8 = 30;
    pub const USER_NAME: u8 = 31;
    pub const ACCOUNT_NAME: u8 = 32;
    pub const CURRENT_PRIVILEGE_SET_NAME: u8 = 34;
    pub const ACTIVE_MODIFIER_KEYS: u8 = 35;
    pub const NETWORK_PROTOCOL: u8 = 36;
    pub const RECORD_ID: u8 = 37;
    pub const RECORD_MODIFICATION_COUNT: u8 = 38;
    pub const ACTIVE_FIELD_CONTENTS: u8 = 39;
    pub const FILE_PATH: u8 = 40;
    pub const LAYOUT_ACCESS: u8 = 42;
    pub const RECORD_ACCESS: u8 = 43;
    pub const HIGH_CONTRAST_STATE: u8 = 44;
    pub const SYSTEM_APPEARANCE: u8 = 45;
    pub const STATUS_AREA_STATE: u8 = 46;
    pub const LAYOUT_VIEW_STATE: u8 = 47;
    pub const WINDOW_NAME: u8 = 48;
    pub const WINDOW_HEIGHT: u8 = 49;
    pub const WINDOW_LEFT: u8 = 50;
    pub const WINDOW_TOP: u8 = 51;
    pub const WINDOW_VISIBLE: u8 = 52;
    pub const WINDOW_WIDTH: u8 = 53;
    pub const SYSTEM_NIC_ADDRESS: u8 = 54;
    pub const SYSTEM_IP_ADDRESS: u8 = 55;
    pub const ACTIVE_FIELD_TABLE_NAME: u8 = 56;
    pub const ACTIVE_SELECTION_SIZE: u8 = 57;
    pub const ACTIVE_SELECTION_START: u8 = 58;
    pub const APPLICATION_LANGUAGE: u8 = 59;
    pub const CURRENT_HOST_TIMESTAMP: u8 = 60;
    pub const LAYOUT_TABLE_NAME: u8 = 61;
    pub const CURRENT_TIMESTAMP: u8 = 63;
    pub const WINDOW_DESKTOP_HEIGHT: u8 = 64;
    pub const WINDOW_DESKTOP_WIDTH: u8 = 65;
    pub const WINDOW_CONTENT_HEIGHT: u8 = 66;
    pub const WINDOW_CONTENT_WIDTH: u8 = 67;
    pub const CURRENT_EXTENDED_PRIVILEGES: u8 = 69;
    pub const HOST_IP_ADDRESS: u8 = 75;
    pub const REQUEST_OMIT_STATE: u8 = 76;
    pub const RECORD_OPEN_COUNT: u8 = 78;
    pub const RECORD_OPEN_STATE: u8 = 79;
    pub const TEXT_RULER_VISIBLE: u8 = 82;
    pub const ALLOW_FORMATTING_BAR_STATE: u8 = 83;
    pub const USE_SYSTEM_FORMATS_STATE: u8 = 84;
    pub const WINDOW_ZOOM_LEVEL: u8 = 85;
    pub const DB_ENGINE_VERSION: u8 = 87;
    pub const ACTIVE_LAYOUT_OBJECT_NAME: u8 = 88;
    pub const HOST_APPLICATION_VERSION: u8 = 90;
    pub const ACCOUNT_PRIVILEGE_SET_NAME: u8 = 94;
    pub const ACCOUNT_EXTENDED_PRIVILEGES: u8 = 95;
    pub const QUICK_FIND_TEXT: u8 = 96;
    pub const TRIGGER_CURRENT_PANEL: u8 = 97;
    pub const TRIGGER_TARGET_PANEL: u8 = 98;
    pub const WINDOW_STYLE: u8 = 99;
    pub const UUID: u8 = 101;
    pub const PERSISTENT_ID: u8 = 102;
    pub const CONNECTION_STATE: u8 = 103;
    pub const CURRENT_TIME_UTC_MILLISECONDS: u8 = 104;
    pub const DEVICE: u8 = 105;
    pub const WINDOW_ORIENTATION: u8 = 106;
    pub const ENCRYPTION_STATE: u8 = 108;
    pub const MODIFIED_FIELDS: u8 = 110;
    pub const SCREEN_SCALE_FACTOR: u8 = 113;
    pub const APPLICATION_ARCHITECTURE: u8 = 115;
    pub const TOUCH_KEYBOARD_STATE: u8 = 117;
    pub const MENUBAR_STATE: u8 = 118;
    pub const ACTIVE_RECORD_NUMBER: u8 = 121;
    pub const UUID_NUMBER: u8 = 122;
    pub const ACCOUNT_TYPE: u8 = 124;
    pub const SYSTEM_LOCALE_ELEMENTS: u8 = 126;
    pub const FILE_LOCALE_ELEMENTS: u8 = 127;
    pub const INSTALLED_PLUGINS_JSON: u8 = 128;
    pub const CURRENT_TIME_UTC_MICROSECONDS: u8 = 133;
    pub const SYSTEM_STORAGE_AVAILABLE: u8 = 137;
    pub const STUDIO_DATASOURCE: u8 = 138;
    pub const ACCOUNT_PASSWORD_DAYS_REMAINING: u8 = 139;
    pub const WINDOW_UUID: u8 = 140;
}

/// FileMaker's window modes (`Get ( WindowMode )`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMode {
    #[default]
    Browse,
    Find,
    Preview,
    Printing,
    Layout,
}

impl WindowMode {
    pub fn number(self) -> i64 {
        match self {
            WindowMode::Browse => 0,
            WindowMode::Find => 1,
            WindowMode::Preview => 2,
            WindowMode::Printing => 3,
            WindowMode::Layout => 4,
        }
    }
}

/// Authenticated account facts for one owning file. A missing fact is
/// unavailable, not an empty or non-expiring account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountInfo {
    pub account_type: Option<String>,
    /// -1 means no expiry; otherwise the day number when the password expires.
    pub password_expiry: Option<i64>,
    /// The privilege set rules the host verified and enforces for the
    /// account ([`super::security::set_access`]); `None` when the host did
    /// not say.
    pub set_access: Option<super::security::SetAccess>,
}

impl AccountInfo {
    pub fn answer(&self, selector: u8, today: Date) -> Option<Value> {
        use selector::*;
        match selector {
            ACCOUNT_TYPE => Some(Value::text(self.account_type.as_deref().unwrap_or(""))),
            ACCOUNT_PASSWORD_DAYS_REMAINING => Some(self.password_expiry.map_or_else(
                Value::empty_text,
                |expiry| {
                    Value::number(if expiry == -1 {
                        -1
                    } else {
                        (expiry - today.day_number()).max(0)
                    })
                },
            )),
            _ => None,
        }
    }

    pub fn unknown(&self, selector: u8) -> bool {
        use selector::*;
        match selector {
            ACCOUNT_TYPE => self.account_type.is_none(),
            ACCOUNT_PASSWORD_DAYS_REMAINING => self.password_expiry.is_none(),
            _ => false,
        }
    }
}

/// What a client knows about its session and window. Empty strings and
/// `None` answer empty text, as FileMaker does where it has no answer.
#[derive(Clone, Debug, Default)]
pub struct SessionInfo {
    /// `Get ( ApplicationVersion )`, e.g. `Pro 26.0.3`.
    pub application_version: String,
    pub host_application_version: String,
    pub account_name: String,
    /// Facts established by this file's authenticated session.
    pub account_info: AccountInfo,
    pub user_name: String,
    pub privilege_set_name: String,
    /// The account's privileges beyond the set's name, when known.
    pub privileges: Option<Privileges>,
    /// The effective set during an elevated script step; account metadata stays intact.
    pub current_privileges: Option<Privileges>,
    /// The file's name without its extension.
    pub file_name: String,
    /// `fmnet:/<host>/<file>.fmp12` or `file:/...`.
    pub file_path: String,
    /// The file's size in bytes (`Get ( FileSize )`), when known.
    pub file_size: Option<u64>,
    pub host_name: String,
    /// The host's IPv4 address; empty for a local file.
    pub host_ip_address: String,
    /// 0 single user, 1 hosting, 2 a client of a hosted file.
    pub multi_user_state: i64,
    /// Users of the file (`Get ( UserCount )`): 1 for a local file.
    pub user_count: Option<u64>,
    /// 0 not hosted, 1 unencrypted, 2 encrypted, 3 encrypted with a
    /// verified certificate (`Get ( ConnectionState )`).
    pub connection_state: i64,
    pub network_protocol: String,
    pub window_name: String,
    pub window_mode: WindowMode,
    /// The window and the screen; `None` answers their selectors (and the
    /// other user-interface selectors) empty.
    pub window: Option<WindowInfo>,
    /// The current layout: name, 1-based number in the layout list, and
    /// its table occurrence's name.
    pub layout_name: String,
    pub layout_number: Option<u64>,
    pub layout_table_name: String,
    pub layout_count: Option<u64>,
    /// `Get ( LayoutAccess )` for the context window's layout, when the host
    /// judged it from the account's set (session-identity.md 2.4); it wins
    /// over the set's own answer except in an elevated step.
    pub layout_access_here: Option<i64>,
    /// `Get ( RecordAccess )` for the context window's current record,
    /// likewise.
    pub record_access_here: Option<i64>,
    /// The window's found set size and current record (1-based), when the
    /// window knows them better than the data source. In Find mode: the
    /// number of requests and the current one.
    pub found_count: Option<u64>,
    pub active_record_number: Option<u64>,
    /// The active field: its name, its occurrence's name, repetition and
    /// portal row (1-based), its text as typed so far and the selection
    /// in it (1-based start and length; `None` puts the insertion point
    /// after the text).
    pub active_field_name: String,
    pub active_field_table_name: String,
    pub active_repetition_number: Option<u64>,
    pub active_portal_row_number: Option<u64>,
    pub active_field_contents: String,
    pub active_selection: Option<(u64, u64)>,
    /// The active layout object's name when the window knows the object
    /// (a portal, or a field in a portal row), over the first object
    /// showing the active field (`Get ( ActiveLayoutObjectName )`).
    pub active_layout_object: Option<String>,
    /// The modifier keys held: Shift 1, Caps Lock 2, Control 4,
    /// Option or Alt 8, Command 16.
    pub modifier_keys: i64,
    /// Find mode's requests: how many there are (the count stays after
    /// Find mode, 0 before any) and whether the current one omits.
    pub request_count: u64,
    pub request_omit: bool,
    /// The found set is sorted (`Get ( SortState )`).
    pub sorted: bool,
    /// 0 no open record, 1 new, 2 modified.
    pub record_open_state: Option<i64>,
    /// Names of the fields modified in the current record (without an
    /// occurrence, in field order).
    pub modified_fields: Vec<String>,
    /// The page Preview shows (1 when unknown).
    pub page_number: Option<u64>,
    /// Physical report page while preparing Preview or PDF content. This
    /// does not change the user's window mode or require desktop geometry.
    pub report_page_number: Option<u64>,
    /// The default printer's name; a single space when there is none.
    pub printer_name: String,
    /// The toolbar's Quick Find text.
    pub quick_find_text: String,
    pub system_language: String,
    pub application_language: String,
    pub system_version: String,
    /// The machine the client runs on; `None` answers its selectors empty.
    pub machine: Option<&'static Machine>,
    /// Where `Get ( SystemStorageAvailable )` measures free space.
    pub storage: Option<PathBuf>,
    /// The local time zone's offset from UTC in seconds, for the current
    /// time and date.
    pub utc_offset: i64,
    /// A fixed answer for the clock selectors (a record write computes
    /// every value from one reading), else the system clock.
    pub now: Option<Timestamp>,
    /// The host's clock for `Get ( CurrentHostTimestamp )` when it is not
    /// this machine's (a record write to a file another host serves reads
    /// it with op82); `None` answers it as `Get ( CurrentTimestamp )`.
    pub host_now: Option<Timestamp>,
    /// A fixed answer for `Get ( UUID )`, else a new one each time.
    pub uuid: Option<String>,
    /// The system's formats (`Get ( SystemLocaleElements )`); en-US when
    /// unknown.
    pub system_locale: Option<Locale>,
    /// The session uses the system's formats rather than the file's.
    pub use_system_formats: bool,
    /// The window's view: 0 form, 1 list, 2 table (`Get ( LayoutViewState
    /// )`); the layout's default view when unknown.
    pub layout_view_state: Option<i64>,
    /// `Get ( DBEngineVersion )`: FileMaker Pro 26.0.3 answers 260030307
    /// (calc-functions.md 9.1, U-19), FileMaker Server 26.0.3 260030309.
    pub db_engine_version: Option<i64>,
    /// The other files open in the same application, in the order they
    /// were opened, with this file's place among them; the Design
    /// functions answer for each by name.
    pub other_files: Vec<OpenFile>,
    /// This file's position in the open order (`DatabaseNames`).
    pub open_position: usize,
    /// Every window of the application, front first, as (file, window
    /// name); empty for this file's one window.
    pub windows: Vec<(String, String)>,
}

/// An account's privilege set (calc-functions.md 6.1 rows 42, 43, 69, 94,
/// 95).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Privileges {
    /// The account's own set, which a script with full access privileges
    /// does not change.
    pub account_set_name: String,
    /// The set's extended privileges' keywords, in catalog order.
    pub extended: Vec<String>,
    /// 2 full access, 1 view only, 0 none; `None` when the catalog
    /// does not establish the answer (`LayoutAccess`, `RecordAccess`).
    pub layout_access: Option<i64>,
    pub record_access: Option<i64>,
}

/// Another open file the Design functions can name.
#[derive(Clone, Debug)]
pub struct OpenFile {
    pub schema: Arc<CalcSchema>,
}

/// `Get ( TriggerCurrentPanel )` and `TriggerTargetPanel` outside a
/// trigger: panel 0 and no object name (calc-functions.md 6.3 O2).
const NO_PANEL: &str = "0\r";

impl SessionInfo {
    /// The platform answers of the machine this runs on (calc-functions.md
    /// 6: 1 macOS, -2 Windows; 8 is FileMaker's Ubuntu host value).
    pub fn for_this_machine() -> SessionInfo {
        let machine = Machine::current();
        SessionInfo {
            application_version: "Pro 26.0.3".into(),
            db_engine_version: Some(260_030_307),
            multi_user_state: 2,
            user_count: Some(1),
            network_protocol: "TCP/IP".into(),
            system_language: "English".into(),
            application_language: "English".into(),
            system_version: machine.system_version.clone(),
            user_name: machine::user_name(),
            machine: Some(machine),
            storage: home_folder(),
            printer_name: " ".into(),
            use_system_formats: true,
            ..SessionInfo::default()
        }
    }

    /// The other open file `name` names: its name in any case, with or
    /// without `.fmp12` in any case.
    pub fn other_file(&self, name: &str) -> Option<&OpenFile> {
        let lower = name.to_lowercase();
        let stem = lower.strip_suffix(".fmp12").unwrap_or(&lower);
        self.other_files
            .iter()
            .find(|f| f.schema.file_name.to_lowercase() == stem)
    }

    /// The open files' names without extension, in the order they were
    /// opened.
    pub fn database_names(&self, this: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .other_files
            .iter()
            .map(|f| f.schema.file_name.clone())
            .collect();
        names.insert(self.open_position.min(names.len()), this.to_owned());
        names
    }

    /// The windows of the file named `file` (every file's for `None`),
    /// front first.
    pub fn window_names(&self, this: &str, file: Option<&str>) -> Vec<String> {
        if self.windows.is_empty() {
            return match file {
                Some(f) if !same_file(f, this) => Vec::new(),
                _ if self.window_name.is_empty() => Vec::new(),
                _ => vec![self.window_name.clone()],
            };
        }
        self.windows
            .iter()
            .filter(|(owner, _)| file.map_or(true, |f| same_file(f, owner)))
            .map(|(_, name)| name.clone())
            .collect()
    }

    /// The set the current privilege selectors answer: an elevated step's,
    /// else the account's own.
    fn effective_privileges(&self) -> Option<&Privileges> {
        self.current_privileges.as_ref().or_else(|| {
            self.privileges
                .as_ref()
                .filter(|p| p.account_set_name == self.privilege_set_name)
        })
    }

    /// Whether this session answers `selector` without what FileMaker
    /// answers it from: security metadata the client could not read
    /// (session-identity.md 2). [`Self::get`] then answers empty rather
    /// than the account's value; a caller that must not take that for
    /// FileMaker's own answer (a layout object's hide condition) can tell.
    /// `Get ( LayoutAccess )`: an elevated step's set, else the context's
    /// own answer, else the account's set.
    fn layout_access(&self) -> Option<i64> {
        match &self.current_privileges {
            Some(elevated) => elevated.layout_access,
            None => self
                .layout_access_here
                .or_else(|| self.privileges.as_ref()?.layout_access),
        }
    }

    /// `Get ( RecordAccess )`, as [`Self::layout_access`].
    fn record_access(&self) -> Option<i64> {
        match &self.current_privileges {
            Some(elevated) => elevated.record_access,
            None => self
                .record_access_here
                .or_else(|| self.privileges.as_ref()?.record_access),
        }
    }

    pub fn unknown_answer(&self, selector: u8) -> bool {
        use selector::*;
        let current = self.effective_privileges();
        match selector {
            ACCOUNT_TYPE | ACCOUNT_PASSWORD_DAYS_REMAINING => self.account_info.unknown(selector),
            ACCOUNT_PRIVILEGE_SET_NAME | ACCOUNT_EXTENDED_PRIVILEGES => self.privileges.is_none(),
            CURRENT_PRIVILEGE_SET_NAME => current.is_none() && self.privilege_set_name.is_empty(),
            CURRENT_EXTENDED_PRIVILEGES => current.is_none(),
            LAYOUT_ACCESS => self.layout_access().is_none(),
            RECORD_ACCESS => self.record_access().is_none(),
            _ => false,
        }
    }

    /// The effective local calendar date, including an explicit clock
    /// snapshot used by calculations and deterministic callers.
    pub fn evaluation_date(&self) -> Date {
        self.now
            .map_or_else(|| today(self.utc_offset), |now| now.date())
    }

    /// Whether `other` describes the same session and window as this one:
    /// everything a `Get` selector reads that can change while a window is
    /// open, the window's geometry, focus, record and privileges included.
    /// The clocks are not compared (each read asks the system); the
    /// machine and other open files compare by identity.
    pub fn same_window_context(&self, other: &SessionInfo) -> bool {
        // Destructured without `..`: a new field must say whether it is
        // part of the window's context.
        let SessionInfo {
            application_version,
            host_application_version,
            account_name,
            account_info,
            user_name,
            privilege_set_name,
            privileges,
            current_privileges,
            file_name,
            file_path,
            file_size,
            host_name,
            host_ip_address,
            multi_user_state,
            user_count,
            connection_state,
            network_protocol,
            window_name,
            window_mode,
            window,
            layout_name,
            layout_number,
            layout_table_name,
            layout_count,
            layout_access_here,
            record_access_here,
            found_count,
            active_record_number,
            active_field_name,
            active_field_table_name,
            active_repetition_number,
            active_portal_row_number,
            active_field_contents,
            active_selection,
            active_layout_object,
            modifier_keys,
            request_count,
            request_omit,
            sorted,
            record_open_state,
            modified_fields,
            page_number,
            report_page_number,
            printer_name,
            quick_find_text,
            system_language,
            application_language,
            system_version,
            machine,
            storage,
            utc_offset,
            now: _,
            host_now: _,
            uuid,
            system_locale,
            use_system_formats,
            layout_view_state,
            db_engine_version,
            other_files,
            open_position,
            windows,
        } = self;
        *application_version == other.application_version
            && *host_application_version == other.host_application_version
            && *account_name == other.account_name
            && *account_info == other.account_info
            && *user_name == other.user_name
            && *privilege_set_name == other.privilege_set_name
            && *privileges == other.privileges
            && *current_privileges == other.current_privileges
            && *layout_access_here == other.layout_access_here
            && *record_access_here == other.record_access_here
            && *file_name == other.file_name
            && *file_path == other.file_path
            && *file_size == other.file_size
            && *host_name == other.host_name
            && *host_ip_address == other.host_ip_address
            && *multi_user_state == other.multi_user_state
            && *user_count == other.user_count
            && *connection_state == other.connection_state
            && *network_protocol == other.network_protocol
            && *window_name == other.window_name
            && *window_mode == other.window_mode
            && *window == other.window
            && *layout_name == other.layout_name
            && *layout_number == other.layout_number
            && *layout_table_name == other.layout_table_name
            && *layout_count == other.layout_count
            && *found_count == other.found_count
            && *active_record_number == other.active_record_number
            && *active_field_name == other.active_field_name
            && *active_field_table_name == other.active_field_table_name
            && *active_repetition_number == other.active_repetition_number
            && *active_portal_row_number == other.active_portal_row_number
            && *active_field_contents == other.active_field_contents
            && *active_selection == other.active_selection
            && *active_layout_object == other.active_layout_object
            && *modifier_keys == other.modifier_keys
            && *request_count == other.request_count
            && *request_omit == other.request_omit
            && *sorted == other.sorted
            && *record_open_state == other.record_open_state
            && *modified_fields == other.modified_fields
            && *page_number == other.page_number
            && *report_page_number == other.report_page_number
            && *printer_name == other.printer_name
            && *quick_find_text == other.quick_find_text
            && *system_language == other.system_language
            && *application_language == other.application_language
            && *system_version == other.system_version
            && match (machine, other.machine) {
                (Some(a), Some(b)) => std::ptr::eq(*a, b),
                (a, b) => a.is_none() && b.is_none(),
            }
            && *storage == other.storage
            && *utc_offset == other.utc_offset
            && *uuid == other.uuid
            && *system_locale == other.system_locale
            && *use_system_formats == other.use_system_formats
            && *layout_view_state == other.layout_view_state
            && *db_engine_version == other.db_engine_version
            && other_files.len() == other.other_files.len()
            && other_files
                .iter()
                .zip(&other.other_files)
                .all(|(a, b)| Arc::ptr_eq(&a.schema, &b.schema))
            && *open_position == other.open_position
            && *windows == other.windows
    }

    /// `Get ( selector )` in `context`, `None` for a selector this session
    /// does not answer.
    pub fn get(
        &self,
        selector: u8,
        context: &Context,
        file: &FileCalc<'_>,
    ) -> Option<Result<Value, CalcError>> {
        use selector::*;
        let text = |s: &str| Some(Ok(Value::text(s)));
        let number = |n: i64| Some(Ok(Value::number(n)));
        let optional = |n: Option<u64>| {
            Some(Ok(
                n.map_or_else(Value::empty_text, |n| Value::number(n as i64))
            ))
        };
        let desktop = self.window.is_some();
        let find = self.window_mode == WindowMode::Find;
        let focused = !self.active_field_name.is_empty();
        if let Some(value) = self.window.as_ref().and_then(|w| w.get(selector)) {
            return Some(Ok(value));
        }
        let machine = self.machine;
        let privileges = self.privileges.as_ref();
        let current = self.effective_privileges();
        let access =
            |level: Option<i64>| Some(Ok(level.map_or_else(Value::empty_text, Value::number)));
        match selector {
            APPLICATION_VERSION => text(&self.application_version),
            HOST_APPLICATION_VERSION => text(&self.host_application_version),
            ACCOUNT_NAME => text(&self.account_name),
            USER_NAME => text(&self.user_name),
            CURRENT_PRIVILEGE_SET_NAME => {
                text(current.map_or(&self.privilege_set_name, |p| &p.account_set_name))
            }
            ACCOUNT_PRIVILEGE_SET_NAME => text(privileges.map_or("", |p| &p.account_set_name)),
            CURRENT_EXTENDED_PRIVILEGES => {
                text(&current.map_or_else(String::new, |p| p.extended.join("\r")))
            }
            ACCOUNT_EXTENDED_PRIVILEGES => {
                text(&privileges.map_or_else(String::new, |p| p.extended.join("\r")))
            }
            LAYOUT_ACCESS => access(self.layout_access()),
            RECORD_ACCESS => access(self.record_access()),
            ACCOUNT_TYPE | ACCOUNT_PASSWORD_DAYS_REMAINING => self
                .account_info
                .answer(selector, self.evaluation_date())
                .map(Ok),
            FILE_NAME => text(&self.file_name),
            FILE_PATH => text(&self.file_path),
            FILE_SIZE => optional(self.file_size),
            HOST_NAME => text(&self.host_name),
            HOST_IP_ADDRESS => text(&self.host_ip_address),
            MULTI_USER_STATE => number(self.multi_user_state),
            USER_COUNT => optional(self.user_count),
            CONNECTION_STATE if desktop => number(self.connection_state),
            NETWORK_PROTOCOL => text(&self.network_protocol),
            WINDOW_NAME => text(&self.window_name),
            WINDOW_MODE => number(self.window_mode.number()),
            LAYOUT_NAME => text(&self.layout_name),
            LAYOUT_NUMBER => optional(self.layout_number),
            LAYOUT_TABLE_NAME => text(&self.layout_table_name),
            LAYOUT_COUNT => optional(
                self.layout_count
                    .or(Some(file.schema.listed_layouts().count() as u64)),
            ),
            LAYOUT_VIEW_STATE => optional(
                self.layout_view_state
                    .or_else(|| {
                        let flags = file
                            .schema
                            .layout_named(&self.layout_name)?
                            .layout
                            .as_ref()?
                            .def
                            .as_ref()?
                            .flags;
                        Some(if flags & 0x4000 != 0 {
                            2
                        } else if flags & 0x200 != 0 {
                            1
                        } else {
                            0
                        })
                    })
                    .map(|v| v as u64),
            ),
            DB_ENGINE_VERSION => optional(self.db_engine_version.map(|v| v as u64)),
            UUID_NUMBER => Some(Ok(Value::Number(Number::parse_text(
                &match &self.uuid {
                    Some(fixed) => {
                        let hex: String = fixed.chars().filter(char::is_ascii_hexdigit).collect();
                        u128::from_str_radix(&hex, 16).unwrap_or(0).to_string()
                    }
                    None => uuid_number(),
                },
                '.',
                ',',
            )))),
            ACTIVE_FIELD_NAME => text(&self.active_field_name),
            ACTIVE_FIELD_TABLE_NAME => text(&self.active_field_table_name),
            ACTIVE_FIELD_CONTENTS => text(if focused {
                &self.active_field_contents
            } else {
                ""
            }),
            ACTIVE_REPETITION_NUMBER if desktop => number(if focused {
                self.active_repetition_number.unwrap_or(1) as i64
            } else {
                0
            }),
            ACTIVE_REPETITION_NUMBER => optional(self.active_repetition_number),
            ACTIVE_PORTAL_ROW_NUMBER if desktop => {
                number(self.active_portal_row_number.unwrap_or(0) as i64)
            }
            ACTIVE_PORTAL_ROW_NUMBER => optional(self.active_portal_row_number),
            ACTIVE_SELECTION_START | ACTIVE_SELECTION_SIZE if !focused => text(""),
            ACTIVE_SELECTION_START | ACTIVE_SELECTION_SIZE => {
                let length = self.active_field_contents.chars().count() as u64;
                let (start, size) = self.active_selection.unwrap_or((length + 1, 0));
                number(if selector == ACTIVE_SELECTION_START {
                    start as i64
                } else {
                    size as i64
                })
            }
            ACTIVE_LAYOUT_OBJECT_NAME if self.active_layout_object.is_some() => {
                text(self.active_layout_object.as_deref().unwrap_or_default())
            }
            ACTIVE_LAYOUT_OBJECT_NAME => text(
                &super::layout_objects::focused_object(
                    file.schema,
                    &self.layout_name,
                    &self.active_field_table_name,
                    &self.active_field_name,
                )
                .and_then(|o| o.name.clone())
                .unwrap_or_default(),
            ),
            ACTIVE_MODIFIER_KEYS if desktop => number(self.modifier_keys),
            ACTIVE_RECORD_NUMBER => optional(self.active_record_number),
            RECORD_OPEN_STATE => number(self.record_open_state.unwrap_or(0)),
            RECORD_OPEN_COUNT => number(i64::from(self.record_open_state.unwrap_or(0) != 0)),
            MODIFIED_FIELDS => text(&self.modified_fields.join("\r")),
            REQUEST_COUNT if desktop => number(self.request_count as i64),
            REQUEST_OMIT_STATE if desktop => number(i64::from(find && self.request_omit)),
            SORT_STATE if desktop => number(i64::from(self.sorted)),
            PAGE_NUMBER if self.report_page_number.is_some() => {
                number(self.report_page_number.unwrap_or(1) as i64)
            }
            PAGE_NUMBER if desktop => number(if self.window_mode == WindowMode::Preview {
                self.page_number.unwrap_or(1) as i64
            } else {
                0
            }),
            PRINTER_NAME => text(&self.printer_name),
            QUICK_FIND_TEXT => text(&self.quick_find_text),
            // Outside a trigger (the runtime answers inside one).
            TRIGGER_CURRENT_PANEL | TRIGGER_TARGET_PANEL if desktop => text(NO_PANEL),
            HIGH_CONTRAST_STATE | TOUCH_KEYBOARD_STATE | STUDIO_DATASOURCE | ENCRYPTION_STATE
                if desktop =>
            {
                number(0)
            }
            ALLOW_FORMATTING_BAR_STATE if desktop => number(1),
            // macOS's light appearance (FileMaker Pro on Windows says
            // `Light`; Linux has none).
            SYSTEM_APPEARANCE if desktop => text(if cfg!(target_os = "macos") {
                "Aqua"
            } else if cfg!(windows) {
                "Light"
            } else {
                ""
            }),
            SYSTEM_LANGUAGE => text(&self.system_language),
            APPLICATION_LANGUAGE => text(&self.application_language),
            SYSTEM_VERSION if !self.system_version.is_empty() => text(&self.system_version),
            SYSTEM_NIC_ADDRESS => machine.and_then(|m| text(&m.nic_addresses.join("\r"))),
            SYSTEM_IP_ADDRESS => machine.and_then(|m| text(&m.ip_addresses.join("\r"))),
            PERSISTENT_ID => machine.and_then(|m| text(&m.persistent_id)),
            INSTALLED_PLUGINS_JSON if machine.is_some() => text(&machine::plugins_json()),
            SYSTEM_STORAGE_AVAILABLE => self
                .storage
                .as_deref()
                .and_then(machine::storage_available)
                .map(|n| Ok(Value::Number(Number::parse_text(&n.to_string(), '.', ',')))),
            SYSTEM_PLATFORM => number(machine::platform()),
            DEVICE => number(machine::device()),
            APPLICATION_ARCHITECTURE => text(machine::architecture()),
            USE_SYSTEM_FORMATS_STATE => number(i64::from(self.use_system_formats)),
            // `Active` marks the formats in use.
            FILE_LOCALE_ELEMENTS => {
                text(&file.schema.locale.elements_json(!self.use_system_formats))
            }
            SYSTEM_LOCALE_ELEMENTS => text(
                &self
                    .system_locale
                    .clone()
                    .unwrap_or_else(Locale::en_us)
                    .elements_json(self.use_system_formats),
            ),
            CURRENT_DATE => Some(Ok(Value::Date(
                self.now
                    .map_or_else(|| today(self.utc_offset), |t| t.date()),
            ))),
            CURRENT_TIME => Some(Ok(Value::Time(
                self.now
                    .map_or_else(|| time_of_day(self.utc_offset), |t| t.time_of_day()),
            ))),
            CURRENT_TIMESTAMP => Some(match self.now {
                Some(t) => Ok(Value::Timestamp(t)),
                None => now(self.utc_offset).map(Value::Timestamp),
            }),
            CURRENT_HOST_TIMESTAMP => Some(match self.host_now.or(self.now) {
                Some(t) => Ok(Value::Timestamp(t)),
                None => now(self.utc_offset).map(Value::Timestamp),
            }),
            CURRENT_TIME_UTC_MILLISECONDS => utc_since_year_one(1_000),
            CURRENT_TIME_UTC_MICROSECONDS => utc_since_year_one(1),
            UUID => Some(Ok(Value::text(self.uuid.clone().unwrap_or_else(uuid)))),
            // No current record (Find mode, an empty found set): 0.
            RECORD_ID => Some(Ok(match context.record {
                Some(NO_RECORD) => Value::number(0),
                Some(r) => Value::number(r as i64),
                None => Value::empty_text(),
            })),
            FOUND_COUNT => Some(file.found_count(context, self.found_count)),
            // In Find mode, the current request.
            RECORD_NUMBER if find => optional(self.active_record_number),
            RECORD_NUMBER => Some(file.record_number(context)),
            TOTAL_RECORD_COUNT => Some(file.total_count(context)),
            RECORD_MODIFICATION_COUNT => Some(file.modification_count(context)),
            _ => None,
        }
    }
}

/// The auto-enter options of a field definition (schema-path.md 5.5):
/// creation and modification values, serial numbers, data, calculations,
/// the previous record's value and lookups.
pub const AUTO_ENTER: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x400;

/// Whether `def` enters a value of its own into a new record.
pub fn auto_enters(def: &super::schema::FieldDef) -> bool {
    def.options & AUTO_ENTER != 0
}

/// `Get ( ModifiedFields )` of a record of `table` (calc-functions.md 6.3
/// O3): the fields `changed` sets, the stored calculations that depend on
/// them (also through each other), and a new record's auto-entered fields
/// `entered`, by name in field order.
pub fn modified_fields(
    schema: &CalcSchema,
    table: u64,
    changed: &[u64],
    entered: &[u64],
) -> Vec<String> {
    let Some(def) = schema.table(table) else {
        return Vec::new();
    };
    let mut fields: std::collections::BTreeSet<u64> = changed.iter().copied().collect();
    loop {
        let more: Vec<u64> = def
            .fields
            .values()
            .filter(|f| {
                f.class == super::schema::FieldClass::Calculation
                    && f.stored
                    && !f.global
                    && !fields.contains(&f.id)
            })
            .filter(|f| {
                f.formula.as_ref().is_some_and(|formula| {
                    formula.polish.field_references().iter().any(|r| {
                        let same_table = r
                            .occurrence
                            .id()
                            .and_then(|o| schema.table_of(o))
                            .map_or(true, |t| t.id == table);
                        same_table && r.field.id().is_some_and(|id| fields.contains(&id))
                    })
                })
            })
            .map(|f| f.id)
            .collect();
        if more.is_empty() {
            break;
        }
        fields.extend(more);
    }
    fields.extend(entered.iter().copied());
    fields
        .into_iter()
        .filter_map(|id| def.fields.get(&id).map(|f| f.name.clone()))
        .collect()
}

/// Two names of one file: equal in any case, with or without `.fmp12`.
fn same_file(name: &str, file: &str) -> bool {
    let lower = name.to_lowercase();
    lower.strip_suffix(".fmp12").unwrap_or(&lower) == file.to_lowercase()
}

/// The user's home folder, where a desktop client measures free storage.
fn home_folder() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn now_micros() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i128)
}

/// Microseconds of local time since 1/1/1970.
fn local_micros(utc_offset: i64) -> i128 {
    now_micros() + i128::from(utc_offset) * 1_000_000
}

/// Today's date `utc_offset` seconds east of UTC.
pub fn today(utc_offset: i64) -> Date {
    let epoch = Date::from_ymd(1970, 1, 1).expect("valid date").day_number();
    let days = local_micros(utc_offset).div_euclid(86_400_000_000) as i64;
    Date::from_day(epoch + days).unwrap_or_else(|| Date::from_ymd(1970, 1, 1).expect("valid date"))
}

/// The local time of day to the second (`Get ( CurrentTime )`).
fn time_of_day(utc_offset: i64) -> Time {
    let seconds = local_micros(utc_offset).rem_euclid(86_400_000_000) / 1_000_000;
    Time::from_micros(seconds * 1_000_000, TimeStyle::default())
}

/// The local date and time to the second (`Get ( CurrentTimestamp )`).
pub fn now(utc_offset: i64) -> Result<Timestamp, CalcError> {
    Timestamp::from_date_time(today(utc_offset), time_of_day(utc_offset))
        .ok_or(CalcError::INVALID_DATE)
}

/// Microseconds from 1/1/0001 (the timestamp epoch) to now, in `unit`s.
fn utc_since_year_one(unit: i128) -> Option<Result<Value, CalcError>> {
    let epoch = Date::from_ymd(1970, 1, 1)?.to_timestamp().micros();
    let n = (epoch + now_micros()) / unit;
    Some(Ok(Value::Number(Number::parse_text(
        &n.to_string(),
        '.',
        ',',
    ))))
}

/// `Get ( UUIDNumber )`: FileMaker's answers are random numbers below
/// 2^192 (mostly 58 digits, calc-functions.md 6.3 O1; the server's s31
/// answers), so a random 192-bit number [I: their structure].
pub fn uuid_number() -> String {
    let mut bytes = [0u8; 24];
    if getrandom::getrandom(&mut bytes).is_err() {
        for chunk in bytes.chunks_mut(8) {
            let r = (om_calc::eval::shared_random() * (1u64 << 53) as f64) as u64;
            chunk.copy_from_slice(&(r ^ now_micros() as u64).to_be_bytes());
        }
    }
    // Base 10^19 digits of the 192-bit number, most significant first.
    let mut words: Vec<u64> = bytes
        .chunks(8)
        .map(|c| u64::from_be_bytes(c.try_into().expect("8 bytes")))
        .collect();
    let mut digits = Vec::new();
    while words.iter().any(|w| *w != 0) {
        let mut rest: u128 = 0;
        for word in words.iter_mut() {
            let value = (rest << 64) | u128::from(*word);
            *word = (value / 10_000_000_000_000_000_000) as u64;
            rest = value % 10_000_000_000_000_000_000;
        }
        digits.push(rest as u64);
    }
    let mut text = digits.last().map_or_else(|| "0".to_owned(), u64::to_string);
    for part in digits.iter().rev().skip(1) {
        text.push_str(&format!("{part:019}"));
    }
    text
}

/// A random version 4 UUID in FileMaker's upper-case form.
pub fn uuid() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // No system randomness: the calculation generator and the clock.
        for chunk in bytes.chunks_mut(8) {
            let r = (om_calc::eval::shared_random() * (1u64 << 53) as f64) as u64;
            chunk.copy_from_slice(&(r ^ now_micros() as u64).to_be_bytes());
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
