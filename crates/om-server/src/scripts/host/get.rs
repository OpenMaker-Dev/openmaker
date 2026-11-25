//! `Get` selectors of a server script session as FileMaker Server's script
//! engine answers them (calc-functions.md 6, the host matrix; batteries
//! `s31-get-server`, `s33-get-*`): the session's window is a 640 x 480
//! window named after the file at 0, 0 on a 640 x 480 x 32 screen, with
//! no plug-ins, printer or user paths. New Record and Find mode put the
//! focus in the first field of the layout's tab order, Go to Field in its
//! field; leaving the record or the mode takes it away. The selectors this
//! module does not answer are the session's own (`ServerHost::get_value`)
//! or the script runtime's.

use om_calc::eval::CalcHost;
use om_calc::{CalcError, Locale, Value};
use om_data::host::layout_objects::first_in_tab_order;
use om_data::host::machine;
use om_data::host::session::selector::*;
use om_data::host::session::{auto_enters, modified_fields};
use om_formula::Names;
use std::ops::Bound;

use crate::engine::{Direction, ReadError, View};

use super::{Mode, ServerHost, NO_RECORD};
use crate::config::Privileges;

/// Selectors only this module answers (catalogs/get-selectors.csv).
const MULTI_USER_STATE: u8 = 14;
const PAGE_NUMBER: u8 = 15;
const ACTIVE_PORTAL_ROW_NUMBER: u8 = 17;
const PRINTER_NAME: u8 = 18;
const ACTIVE_REPETITION_NUMBER: u8 = 21;
const SCREEN_DEPTH: u8 = 23;
const SCREEN_HEIGHT: u8 = 24;
const SCREEN_WIDTH: u8 = 25;
const USER_COUNT: u8 = 30;
const ACTIVE_MODIFIER_KEYS: u8 = 35;
const LAYOUT_ACCESS: u8 = 42;
const RECORD_ACCESS: u8 = 43;
const HIGH_CONTRAST_STATE: u8 = 44;
const STATUS_AREA_STATE: u8 = 46;
const WINDOW_HEIGHT: u8 = 49;
const WINDOW_LEFT: u8 = 50;
const WINDOW_TOP: u8 = 51;
const WINDOW_VISIBLE: u8 = 52;
const WINDOW_WIDTH: u8 = 53;
const WINDOW_DESKTOP_HEIGHT: u8 = 64;
const WINDOW_DESKTOP_WIDTH: u8 = 65;
const WINDOW_CONTENT_HEIGHT: u8 = 66;
const WINDOW_CONTENT_WIDTH: u8 = 67;
const CURRENT_EXTENDED_PRIVILEGES: u8 = 69;
const REQUEST_OMIT_STATE: u8 = 76;
const TEXT_RULER_VISIBLE: u8 = 82;
const ALLOW_FORMATTING_BAR_STATE: u8 = 83;
const WINDOW_ZOOM_LEVEL: u8 = 85;
const ACCOUNT_PRIVILEGE_SET_NAME: u8 = 94;
const ACCOUNT_EXTENDED_PRIVILEGES: u8 = 95;
const WINDOW_STYLE: u8 = 99;
const CONNECTION_STATE: u8 = 103;
const WINDOW_ORIENTATION: u8 = 106;
const ENCRYPTION_STATE: u8 = 108;
const SCREEN_SCALE_FACTOR: u8 = 113;
const TOUCH_KEYBOARD_STATE: u8 = 117;
const MENUBAR_STATE: u8 = 118;
const INSTALLED_PLUGINS_JSON: u8 = 128;
const STUDIO_DATASOURCE: u8 = 138;
const WINDOW_UUID: u8 = 140;

/// The session window's size and the screen's, as FileMaker Server's
/// script engine reports them.
const WINDOW_SIZE: (i64, i64) = (640, 480);
const SCREEN_BITS: i64 = 32;
/// `Get ( DBEngineVersion )` of FileMaker Server 26.0.3.309.
const DB_ENGINE: i64 = 260_030_309;
/// The security catalog: accounts `[23, 1]`, privilege sets `[23, 2]`,
/// extended privileges `[23, 3]` (file-format.md 4).
const SECURITY: u8 = 23;
const ACCOUNTS: u8 = 1;
const PRIVILEGE_SETS: u8 = 2;
const EXTENDED_PRIVILEGES: u8 = 3;
const MEMBERS: u8 = 5;
const NAME: u8 = 0x10;
/// An account's privilege set, a key string naming the set's member.
const SET_REFERENCE: u8 = 0x0b;
/// A privilege set's record privileges (calc-functions.md 9).
const RECORD_PRIVILEGES: u8 = 0x02;

/// What a privilege set gives, as `Get` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SetPrivileges {
    name: String,
    /// Extended privilege keywords, in alphabetical order.
    extended: Vec<String>,
    /// `Get ( RecordAccess )`: 2 records can be changed, 1 viewed, 0 not.
    record_access: i64,
}

/// The field with the focus: its occurrence and field, and the name of the
/// object showing it (`Get ( ActiveLayoutObjectName )`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Focus {
    pub occurrence: u64,
    pub field: u64,
    pub object: String,
    /// The layout object entered, when known: a field shown by several
    /// objects, or by an unnamed one, is entered again by it.
    pub object_id: Option<u64>,
    /// The repetition entered (`Get ( ActiveRepetitionNumber )`).
    pub repetition: u32,
    /// Insert steps typed into it: not a modified field until it is left.
    pub typed: bool,
}

impl ServerHost {
    /// The selectors FileMaker Server's script engine answers for the
    /// session itself; `None` for the rest.
    pub(super) fn get_answer(&self, selector: u8) -> Option<Result<Value, CalcError>> {
        let number = |n: i64| Some(Ok(Value::number(n)));
        let text = |t: &str| Some(Ok(Value::text(t)));
        let window = self.window.as_ref();
        match selector {
            // No file is shared by the session itself: the engine answers 0
            // for a script session's file [O: harness, not hosted].
            MULTI_USER_STATE => number(0),
            PAGE_NUMBER => text(""),
            ACTIVE_PORTAL_ROW_NUMBER => number(
                window
                    .and_then(|w| w.portal.as_ref())
                    .map_or(0, |r| i64::try_from(r.row).unwrap_or(0)),
            ),
            ACTIVE_REPETITION_NUMBER => {
                number(self.focus.as_ref().map_or(0, |f| i64::from(f.repetition)))
            }
            ACTIVE_FIELD_NAME
            | ACTIVE_FIELD_TABLE_NAME
            | ACTIVE_FIELD_CONTENTS
            | ACTIVE_SELECTION_START
            | ACTIVE_SELECTION_SIZE
            | ACTIVE_LAYOUT_OBJECT_NAME => Some(Ok(self.focus_answer(selector))),
            // "Function not available": no printers on the server.
            PRINTER_NAME => Some(Err(CalcError::from_code(1223).expect("nonzero"))),
            SCREEN_DEPTH => number(SCREEN_BITS),
            SCREEN_HEIGHT | WINDOW_HEIGHT | WINDOW_DESKTOP_HEIGHT | WINDOW_CONTENT_HEIGHT => {
                number(WINDOW_SIZE.1)
            }
            SCREEN_WIDTH | WINDOW_WIDTH | WINDOW_DESKTOP_WIDTH | WINDOW_CONTENT_WIDTH => {
                number(WINDOW_SIZE.0)
            }
            WINDOW_LEFT | WINDOW_TOP => number(0),
            WINDOW_VISIBLE | STATUS_AREA_STATE | MENUBAR_STATE => number(1),
            WINDOW_NAME => text(&self.window_name()),
            WINDOW_STYLE
            | ACTIVE_MODIFIER_KEYS
            | HIGH_CONTRAST_STATE
            | REQUEST_OMIT_STATE
            | TEXT_RULER_VISIBLE
            | ALLOW_FORMATTING_BAR_STATE
            | CONNECTION_STATE
            | ENCRYPTION_STATE
            | TOUCH_KEYBOARD_STATE
            | STUDIO_DATASOURCE => number(0),
            WINDOW_ORIENTATION => number(-1),
            SCREEN_SCALE_FACTOR | USER_COUNT => number(1),
            WINDOW_ZOOM_LEVEL => number(100),
            DB_ENGINE_VERSION => number(DB_ENGINE),
            // The privilege set of the session's account in the file (s33):
            // its name, its extended privileges alphabetically, its record
            // access. An account of the server's configuration that the
            // file does not have is known only by full access.
            CURRENT_PRIVILEGE_SET_NAME => text(
                &self
                    .current_privilege_set()
                    .map(|s| s.name)
                    .unwrap_or_default(),
            ),
            ACCOUNT_PRIVILEGE_SET_NAME => {
                text(&self.privilege_set().map(|s| s.name).unwrap_or_default())
            }
            CURRENT_EXTENDED_PRIVILEGES => text(
                &self
                    .current_privilege_set()
                    .map(|s| s.extended.join("\r"))
                    .unwrap_or_default(),
            ),
            ACCOUNT_EXTENDED_PRIVILEGES => text(
                &self
                    .privilege_set()
                    .map(|s| s.extended.join("\r"))
                    .unwrap_or_default(),
            ),
            // The volume holding the hosted file.
            SYSTEM_STORAGE_AVAILABLE => self
                .hosted
                .path
                .parent()
                .and_then(machine::storage_available)
                .map(|n| {
                    Ok(Value::Number(om_calc::Number::parse_text(
                        &n.to_string(),
                        '.',
                        ',',
                    )))
                }),
            // Every set probed answered LayoutAccess 2 (s33); layouts with
            // privileges of their own are not decoded.
            LAYOUT_ACCESS => number(if self.current_privilege_set().is_some() {
                2
            } else {
                0
            }),
            RECORD_ACCESS => number(self.current_privilege_set().map_or(0, |s| s.record_access)),
            // The panel number and the panel object's name, outside a
            // trigger "0" and nothing.
            TRIGGER_CURRENT_PANEL | TRIGGER_TARGET_PANEL => text("0\r"),
            NETWORK_PROTOCOL => text("TCP/IP"),
            SYSTEM_VERSION => text(&machine::Machine::current().system_version),
            // In a script session the server's own formats are the active
            // ones and the file's are not (the reverse of a calculation
            // with no session).
            SYSTEM_LOCALE_ELEMENTS => text(&server_system_locale().elements_json(true)),
            FILE_LOCALE_ELEMENTS => text(&self.locale.elements_json(false)),
            USE_SYSTEM_FORMATS_STATE => number(0),
            INSTALLED_PLUGINS_JSON => text(&machine::plugins_json()),
            LAYOUT_COUNT => Some(Ok(Value::number(self.listed_layouts().len() as i64))),
            LAYOUT_NUMBER => {
                let id = window.and_then(|w| self.schema.layouts.get(w.layout))?.id;
                let position = self.listed_layouts().iter().position(|l| *l == id)?;
                number(position as i64 + 1)
            }
            LAYOUT_VIEW_STATE => {
                let flags = self.window_layout_flags()?;
                number(if flags & 0x4000 != 0 {
                    2
                } else if flags & 0x200 != 0 {
                    1
                } else {
                    0
                })
            }
            // No record: 0, as the engine answers on an empty found set.
            RECORD_ID | RECORD_MODIFICATION_COUNT | ACTIVE_RECORD_NUMBER
                if window.and_then(|w| w.record()).is_none() =>
            {
                number(0)
            }
            ACTIVE_RECORD_NUMBER => Some(Ok(self.get_value(RECORD_NUMBER))),
            UUID_NUMBER => Some(Ok(Value::Number(om_calc::Number::parse_text(
                &om_data::host::session::uuid_number(),
                '.',
                ',',
            )))),
            WINDOW_UUID => text(&self.window_uuid_of(self.window_serial)),
            _ => None,
        }
    }

    /// The layouts of the list (no folders), as layout ids.
    fn listed_layouts(&self) -> Vec<u64> {
        match self.design() {
            Some(schema) => schema.listed_layouts().map(|l| u64::from(l.id)).collect(),
            None => self.schema.layouts.iter().map(|l| l.id).collect(),
        }
    }

    /// The window's layout's view flags (layout-model.md 4.2).
    fn window_layout_flags(&self) -> Option<u64> {
        let id = self.schema.layouts.get(self.window.as_ref()?.layout)?.id;
        let schema = self.design()?;
        let entry = schema.layouts().iter().find(|l| u64::from(l.id) == id)?;
        Some(entry.layout.as_ref()?.def.as_ref()?.flags)
    }

    /// Give the focus to the first field of the window layout's tab order
    /// (none when it has no field in it).
    pub(super) fn focus_first_field(&mut self) {
        let first = self.design().and_then(|schema| {
            let window = self.window.as_ref()?;
            let id = self.schema.layouts.get(window.layout)?.id;
            let entry = schema.layouts().iter().find(|l| u64::from(l.id) == id)?;
            let object = first_in_tab_order(entry.layout.as_deref()?)?;
            let spec = object.field_spec()?;
            Some(Focus {
                occurrence: om_layout::occurrence_id(spec.table_key)?,
                field: om_hbam::serial::raw_of_id(spec.field_key)?,
                object: object.name.clone().unwrap_or_default(),
                object_id: Some(object.id),
                repetition: 1,
                typed: false,
            })
        });
        self.focus = first;
    }

    /// The focus selectors: the field's name and occurrence, its text (in
    /// Find mode the request's criterion), the insertion point after it,
    /// the object's name; empty without a focus.
    fn focus_answer(&self, selector: u8) -> Value {
        let Some(focus) = &self.focus else {
            // A portal active without a field entered is the active object.
            let portal = self.window.as_ref().and_then(|w| w.portal.as_ref());
            return match (selector, portal) {
                (ACTIVE_LAYOUT_OBJECT_NAME, Some(row)) => Value::text(&row.name),
                _ => Value::empty_text(),
            };
        };
        let contents = || -> String {
            let Some(window) = self.window.as_ref() else {
                return String::new();
            };
            let record = match window.mode {
                Mode::Find => NO_RECORD,
                _ => window.record().unwrap_or(NO_RECORD),
            };
            // A related field reads the active row's record, else the first
            // related record; never the window's record in another table.
            let record = if focus.occurrence == window.occurrence || record == NO_RECORD {
                record
            } else {
                match self.active_row_record(window.occurrence, record, focus.occurrence) {
                    Ok(Some(Some(related))) => related,
                    Ok(Some(None)) | Err(_) => return String::new(),
                    Ok(None) => match self
                        .related_set(window.occurrence, record, focus.occurrence)
                        .ok()
                        .and_then(|records| records.first().copied())
                    {
                        Some(related) => related,
                        None => return String::new(),
                    },
                }
            };
            let value = self
                .field_value(
                    focus.occurrence,
                    record,
                    focus.field,
                    focus.repetition.max(1),
                )
                .unwrap_or_else(|_| Value::empty_text());
            om_calc::eval::display(&value, self)
        };
        match selector {
            ACTIVE_FIELD_NAME => Value::text(
                self.schema
                    .field(focus.occurrence, focus.field)
                    .map(|(_, def)| def.name.clone())
                    .unwrap_or_default(),
            ),
            ACTIVE_FIELD_TABLE_NAME => Value::text(
                self.schema
                    .names
                    .occurrence_name(focus.occurrence)
                    .unwrap_or_default(),
            ),
            ACTIVE_FIELD_CONTENTS => Value::text(contents()),
            ACTIVE_SELECTION_START => Value::number(contents().chars().count() as i64 + 1),
            ACTIVE_SELECTION_SIZE => Value::number(0),
            ACTIVE_LAYOUT_OBJECT_NAME => Value::text(&focus.object),
            _ => Value::empty_text(),
        }
    }

    /// `Get ( ModifiedFields )` of the open record of `table`: the cells
    /// steps set, the stored calculations on them and, for a new record,
    /// its auto-entered fields, by name in field order (s31, s33).
    pub(super) fn modified_fields(
        &self,
        table: u64,
        changed: &std::collections::BTreeSet<u64>,
        new: bool,
    ) -> Vec<String> {
        let Some(schema) = self.schema.calc() else {
            return Vec::new();
        };
        // A cell is a field's first repetition, or `field * 256 + n`.
        let is_field = |id: u64| {
            schema
                .table(table)
                .is_some_and(|t| t.fields.contains_key(&id))
        };
        let fields: Vec<u64> = changed
            .iter()
            .filter_map(|&cell| {
                if is_field(cell) {
                    Some(cell)
                } else {
                    Some(cell / 256).filter(|f| is_field(*f))
                }
            })
            .collect();
        let entered: Vec<u64> = match (new, schema.table(table)) {
            (true, Some(def)) => def
                .fields
                .values()
                .filter(|f| auto_enters(f))
                .map(|f| f.id)
                .collect(),
            _ => Vec::new(),
        };
        modified_fields(schema, table, &fields, &entered)
    }

    /// The session account's privilege set: its own in the file's
    /// security catalog, else [Full Access]'s for a session given full
    /// access whose account the file does not have (a test's session).
    ///
    /// Read through a reading taken for it, which reads each account's name
    /// and set reference alone, never a password; one that fails gives
    /// none and fails the step ([`ServerHost::read`]).
    fn privilege_set(&self) -> Option<SetPrivileges> {
        let full_access = self.privileges == Privileges::FullAccess;
        self.read(|read| {
            Ok(match account_set(read.view, &self.account)? {
                Some(set) => Some(set),
                None if full_access => named_set(read.view, "[Full Access]")?,
                None => None,
            })
        })
        .ok()
        .flatten()
    }

    /// Effective set while a script runs with full access. Account Get
    /// selectors continue to use the signed-in account's original set.
    fn current_privilege_set(&self) -> Option<SetPrivileges> {
        if self.elevated {
            self.read(|read| named_set(read.view, "[Full Access]"))
                .ok()
                .flatten()
        } else {
            self.privilege_set()
        }
    }

    /// Find mode's current request is one New Request made.
    pub(super) fn find_request_open(&self) -> bool {
        self.request_open && self.window.as_ref().is_some_and(|w| w.mode == Mode::Find)
    }

    /// The UUID of the session's window `serial` (0 the first): 32
    /// upper-case hex digits without dashes, one per window (13.25).
    pub(super) fn window_uuid_of(&self, serial: u32) -> String {
        let mut seed = format!(
            "{}\u{0}{}\u{0}{}",
            self.hosted.name, self.user.connection, self.holder
        );
        if serial > 0 {
            seed.push_str(&format!("\u{0}{serial}"));
        }
        use md5::{Digest, Md5};
        Md5::digest(seed.as_bytes())
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect()
    }
}

/// The formats of FileMaker Server's system (en-US on Ubuntu), as
/// `Get ( SystemLocaleElements )` shows them: days, months and hours with
/// two digits, four-digit years, and the long date `M# D# D$`.
fn server_system_locale() -> Locale {
    let mut locale = Locale::en_us();
    locale.pad_day = true;
    locale.pad_month = true;
    locale.show_century = true;
    locale.pad_hour = true;
    locale.long_date = om_calc::locale::LongDate {
        // Elements 1, 2, 3 and 0: month number, day number, day name and
        // day name (catalogs: D# 1, D$ 3, M# 4).
        elements: [3, 4, 1, 3],
        separators: [
            String::new(),
            " ".into(),
            " ".into(),
            ", ".into(),
            String::new(),
        ],
    };
    locale
}

/// The name `10` of the member at `member` of the security catalog, its
/// own value as SCSU text, read through `view`.
fn member_name(view: &dyn View, member: &[&[u8]]) -> Result<Option<String>, ReadError> {
    let mut path = member.to_vec();
    path.push(&[NAME]);
    Ok(view
        .node(&path)?
        .and_then(|head| head.value)
        .and_then(|value| om_scsu::decode_scsu(&value).ok()))
}

/// The privilege set account `account` (any case) uses, from the security
/// catalog `[23]`: the first account member under `[1, 5]`, in key order,
/// whose name `10` is `account`, its set reference `0b` a key string
/// (length, then the set member's segment under `[2, 5]`; `01 02` is set
/// 2). Only each account's name and set reference are read.
fn account_set(view: &dyn View, account: &str) -> Result<Option<SetPrivileges>, ReadError> {
    let lower = account.to_lowercase();
    let accounts: [&[u8]; 3] = [&[SECURITY], &[ACCOUNTS], &[MEMBERS]];
    let mut found = None;
    for member in view.children(&accounts, Bound::Unbounded, Direction::Forward) {
        let member = member?;
        let at: [&[u8]; 4] = [&[SECURITY], &[ACCOUNTS], &[MEMBERS], &member.segment];
        if member_name(view, &at)?.is_some_and(|n| n.to_lowercase() == lower) {
            found = Some(member.segment.into_owned());
            break;
        }
    }
    let Some(member) = found else {
        return Ok(None);
    };
    let reference: [&[u8]; 5] = [
        &[SECURITY],
        &[ACCOUNTS],
        &[MEMBERS],
        &member,
        &[SET_REFERENCE],
    ];
    let Some(reference) = view.node(&reference)?.and_then(|head| head.value) else {
        return Ok(None);
    };
    let Some((&length, segment)) = reference.split_first() else {
        return Ok(None);
    };
    match segment.get(..usize::from(length)) {
        Some(segment) => set_privileges(view, segment),
        None => Ok(None),
    }
}

/// The privilege set named `set`: the first set member under `[2, 5]`, in
/// key order, whose name `10` is `set`.
fn named_set(view: &dyn View, set: &str) -> Result<Option<SetPrivileges>, ReadError> {
    let sets: [&[u8]; 3] = [&[SECURITY], &[PRIVILEGE_SETS], &[MEMBERS]];
    for member in view.children(&sets, Bound::Unbounded, Direction::Forward) {
        let member = member?;
        let at: [&[u8]; 4] = [&[SECURITY], &[PRIVILEGE_SETS], &[MEMBERS], &member.segment];
        if member_name(view, &at)?.as_deref() == Some(set) {
            return set_privileges(view, &member.segment);
        }
    }
    Ok(None)
}

/// The privilege set member `segment` under `[2, 5]`: its name, the
/// extended privileges `[3, 5, E]` that list it under `05` (keywords in
/// alphabetical order), and its record access from the first byte of
/// `02`: bits `0x01` delete, `0x04` and `0x40` create and edit, `0x10`
/// view (`55` full and data entry, `54` without delete, `10` read only;
/// s33-account-privileges).
fn set_privileges(view: &dyn View, segment: &[u8]) -> Result<Option<SetPrivileges>, ReadError> {
    let set: [&[u8]; 4] = [&[SECURITY], &[PRIVILEGE_SETS], &[MEMBERS], segment];
    if view.node(&set)?.is_none() {
        return Ok(None);
    }
    let privileges: [&[u8]; 3] = [&[SECURITY], &[EXTENDED_PRIVILEGES], &[MEMBERS]];
    let mut extended = Vec::new();
    for privilege in view.children(&privileges, Bound::Unbounded, Direction::Forward) {
        let privilege = privilege?;
        let p = &privilege.segment;
        let listed: [&[u8]; 6] = [
            &[SECURITY],
            &[EXTENDED_PRIVILEGES],
            &[MEMBERS],
            p,
            &[MEMBERS],
            segment,
        ];
        if view.node(&listed)?.is_some() {
            let at: [&[u8]; 4] = [&[SECURITY], &[EXTENDED_PRIVILEGES], &[MEMBERS], p];
            extended.extend(member_name(view, &at)?);
        }
    }
    extended.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    let flags: [&[u8]; 5] = [
        &[SECURITY],
        &[PRIVILEGE_SETS],
        &[MEMBERS],
        segment,
        &[RECORD_PRIVILEGES],
    ];
    let records = view
        .node(&flags)?
        .and_then(|head| head.value)
        .and_then(|v| v.first().copied())
        .unwrap_or(0);
    let Some(name) = member_name(view, &set)? else {
        return Ok(None);
    };
    Ok(Some(SetPrivileges {
        name,
        extended,
        record_access: if records & 0x45 != 0 {
            2
        } else if records & 0x10 != 0 {
            1
        } else {
            0
        },
    }))
}

#[cfg(test)]
mod tests {
    //! FileMaker Server's `Get` answers in a script session
    //! (`tests/data/get/s31-get-server.contacts-en.tsv`) replayed through a
    //! session on the same file, step by step: the fresh session, New
    //! Record, Set Field, Commit, twice. Ignored by default: the file exists
    //! only on the analysis host (`OM_HARNESS_FILES`).
    use std::path::{Path, PathBuf};

    use std::sync::Arc;

    use om_calc::eval::{CalcHost, Context};
    use om_calc::{Ctx, Value};
    use om_hbam::OwnedNode;
    use om_scripts::runtime::{CommitOptions, Fields, Records, ScriptHost};

    use super::super::design::tests::{session, unescape, FILES};
    use super::super::ServerHost;
    use super::{
        SetPrivileges, ACCOUNTS, EXTENDED_PRIVILEGES, MEMBERS, NAME, PRIVILEGE_SETS,
        RECORD_PRIVILEGES, SECURITY, SET_REFERENCE,
    };
    use crate::config::Privileges;

    /// Answers that differ by run (clocks, random ids, the machine), that
    /// the script runtime gives rather than the host, or that the harness's
    /// security bypass account and unhosted copy give (calc-functions.md 6).
    const SKIPPED: &[&str] = &[
        "ApplicationVersion",
        "CurrentDate",
        "CurrentTime",
        "CurrentTimestamp",
        "CurrentHostTimestamp",
        "CurrentTimeUTCMilliseconds",
        "CurrentTimeUTCMicroseconds",
        "UUID",
        "UUIDNumber",
        "HostName",
        "FileSize",
        "FilePath",
        "PersistentID",
        "SystemStorageAvailable",
        "SystemVersion",
        "InstalledFMPluginsAsJSON",
        "WindowUUID",
        "DocumentsPath",
        "TemporaryPath",
        "LastError",
        "ScriptName",
        "ScriptParameter",
        "ScriptResult",
        "LastMessageChoice",
        "LastErrorDetail",
        "AllowAbortState",
        "ErrorCaptureState",
        "ScriptAnimationState",
        "TransactionOpenState",
        "LastErrorLocation",
        "RevertTransactionOnErrorState",
        "CalculationRepetitionNumber",
        "AccountType",
        "AccountPrivilegeSetName",
    ];

    #[test]
    #[ignore = "requires the harness's contacts-en.fmp12"]
    fn get_selectors_answer_as_filemaker_server_in_a_script_session() {
        let files = std::env::var_os("OM_HARNESS_FILES")
            .map_or_else(|| PathBuf::from(FILES), PathBuf::from);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/get/s31-get-server.contacts-en.tsv");
        let text = std::fs::read_to_string(fixture).unwrap();
        let rows: Vec<(String, Vec<String>)> = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let mut cols = l.split('\t');
                let selector = cols.next().unwrap().to_owned();
                (selector, cols.map(unescape).collect())
            })
            .collect();
        let dir = std::env::temp_dir().join(format!("om-get-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut host = session(&files.join("contacts-en.fmp12"), &dir);
        let commit = CommitOptions {
            with_dialog: false,
            skip_validation: false,
            override_ess_locking: false,
        };
        let mut failures = Vec::new();
        // The probe's first seven steps: Set Variable, New Record, Set
        // Field, Commit, New Record, Set Field, Commit.
        for step in 0..7 {
            match step {
                1 | 4 => host.new_record().unwrap(),
                2 => host
                    .set_field_by_name("Contacts::First Name", Value::text("Ann"))
                    .unwrap(),
                5 => host
                    .set_field_by_name("Contacts::First Name", Value::text("Bob"))
                    .unwrap(),
                3 | 6 => host.commit_records(commit).unwrap(),
                _ => {}
            }
            let context = {
                let w = host.window.as_ref().unwrap();
                Context {
                    occurrence: Some(w.occurrence),
                    record: w.record(),
                    ..Context::new()
                }
            };
            for (selector, values) in &rows {
                let name = &selector["Get ( ".len()..selector.len() - 2];
                if SKIPPED.contains(&name) {
                    continue;
                }
                let result = om_calc::eval::evaluate_text(selector, &mut host, &context);
                let cx = Ctx::new(host.locale(), host.today());
                let mut got = result.value.to_text(&cx).into_string();
                if result.code() != 0 {
                    // The harness writes an error's text empty.
                    got = format!("#{} ", result.code());
                }
                if got != values[step] {
                    failures.push(format!(
                        "step {}: {selector}: want {:?} got {got:?}",
                        step + 1,
                        values[step]
                    ));
                }
            }
        }
        host.close();
        let _ = std::fs::remove_dir_all(&dir);
        for f in &failures {
            println!("{f}");
        }
        assert!(failures.is_empty(), "{} answers differ", failures.len());
    }

    /// The probe expression of the s33 batteries.
    const FOCUS: &str = "Get ( ActiveFieldName ) & \"|\" & Get ( ActiveFieldTableName ) & \"|\" & \
        Get ( ActiveSelectionStart ) & \"|\" & Get ( ActiveSelectionSize ) & \"|\" & \
        Get ( ActiveRepetitionNumber ) & \"|\" & Get ( ActiveLayoutObjectName ) & \"|\" & \
        Substitute ( Get ( ModifiedFields ) ; Char ( 13 ) ; \",\" ) & \"|\" & \
        Get ( RecordOpenState ) & \"|\" & Get ( RequestCount ) & \"|\" & Get ( WindowMode )";

    /// One step of an s33 probe script.
    enum Probe {
        Layout(&'static str),
        NewRecord,
        Set(&'static str, &'static str),
        Commit,
        Revert,
        Find,
        Browse,
        First,
        GoToField(Option<&'static str>),
        Insert(&'static str),
        Snap,
    }

    /// FileMaker Server's focus, open state, ModifiedFields and request
    /// count after New Record, Set Field, Commit, Find mode and Go to
    /// Field on a template (s33-get-focus), each probe script in a session
    /// of its own on one copy of the file, as the battery's run lines.
    fn replay_focus(file: &str, fixture: &str, table: &str, layouts: [&'static str; 2]) {
        use om_scripts::runtime::{
            Fields, InsertTarget, LayoutTarget, Mode, Navigation, RecordMove,
        };
        let files = std::env::var_os("OM_HARNESS_FILES")
            .map_or_else(|| PathBuf::from(FILES), PathBuf::from);
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data/get")
                .join(fixture),
        )
        .unwrap();
        let expected: Vec<(String, Vec<String>)> = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                (
                    cols[0].to_owned(),
                    cols[5..].iter().map(|c| unescape(c)).collect(),
                )
            })
            .collect();
        let field = |name: &str| format!("{table}::{name}");
        let [details, list] = layouts;
        let (a, b, c) = match table {
            "Contacts" => ("Last Name", "Company", "First Name"),
            _ => ("Description", "Status", "Task"),
        };
        let scripts: Vec<(&str, Vec<Probe>)> = vec![
            (
                "s33 details new",
                vec![
                    Probe::Layout(details),
                    Probe::NewRecord,
                    Probe::Snap,
                    Probe::Set(a, "Zed"),
                    Probe::Snap,
                    Probe::Commit,
                    Probe::Snap,
                    Probe::Set(b, "Acme"),
                    Probe::Snap,
                    Probe::Set(c, "Ann"),
                    Probe::Snap,
                    Probe::Commit,
                    Probe::Snap,
                ],
            ),
            (
                "s33 list new",
                vec![
                    Probe::Layout(list),
                    Probe::NewRecord,
                    Probe::Snap,
                    Probe::Revert,
                    Probe::Snap,
                ],
            ),
            (
                "s33 details find",
                vec![
                    Probe::Layout(details),
                    Probe::Find,
                    Probe::Snap,
                    Probe::NewRecord,
                    Probe::Snap,
                    Probe::Browse,
                    Probe::Snap,
                    Probe::Find,
                    Probe::Snap,
                    Probe::Browse,
                    Probe::Snap,
                ],
            ),
            (
                "s33 list find",
                vec![
                    Probe::Layout(list),
                    Probe::Find,
                    Probe::Snap,
                    Probe::Browse,
                    Probe::Snap,
                ],
            ),
            (
                "s33 go to field",
                vec![
                    Probe::Layout(details),
                    Probe::First,
                    Probe::GoToField(Some(c)),
                    Probe::Snap,
                    Probe::Insert("Bo"),
                    Probe::Snap,
                    Probe::GoToField(None),
                    Probe::Snap,
                    Probe::Commit,
                    Probe::Snap,
                ],
            ),
        ];
        let dir = std::env::temp_dir().join(format!("om-s33-{}-{table}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut first = session(&files.join(file), &dir);
        let (shared, hosted) = (Arc::clone(&first.shared), Arc::clone(&first.hosted));
        first.close();
        let commit = CommitOptions {
            with_dialog: false,
            skip_validation: false,
            override_ess_locking: false,
        };
        let mut failures = Vec::new();
        for (i, (name, steps)) in scripts.into_iter().enumerate() {
            let mut host = ServerHost::new(
                Arc::clone(&shared),
                Arc::clone(&hosted),
                0x60 + i as u32,
                "Admin",
                Privileges::FullAccess,
            );
            let mut got = Vec::new();
            for step in steps {
                match step {
                    Probe::Layout(l) => host
                        .go_to_layout(&LayoutTarget::Name(l.into()), None)
                        .unwrap(),
                    Probe::NewRecord => host.new_record().unwrap(),
                    Probe::Set(f, v) => host.set_field_by_name(&field(f), Value::text(v)).unwrap(),
                    Probe::Commit => host.commit_records(commit).unwrap(),
                    Probe::Revert => host.revert_record(false).unwrap(),
                    Probe::Find => host.enter_mode(Mode::Find, false, None).unwrap(),
                    Probe::Browse => host.enter_mode(Mode::Browse, false, None).unwrap(),
                    Probe::First => {
                        host.go_to_record(RecordMove::First, false).unwrap();
                    }
                    Probe::GoToField(f) => {
                        let target = f.map(|f| {
                            let (occurrence, field) = host.field_named(&field(f)).unwrap();
                            om_scripts::runtime::FieldRef {
                                occurrence,
                                field,
                                repetition: 1,
                            }
                        });
                        host.go_to_field(target, false).unwrap();
                    }
                    Probe::Insert(v) => host
                        .insert(
                            InsertTarget {
                                field: None,
                                select_all: false,
                            },
                            Value::text(v),
                        )
                        .unwrap(),
                    Probe::Snap => {
                        let context = host.calc_context();
                        let result = om_calc::eval::evaluate_text(FOCUS, &mut host, &context);
                        let cx = Ctx::new(host.locale(), host.today());
                        got.push(result.value.to_text(&cx).into_string());
                    }
                }
            }
            host.close();
            let want = &expected.iter().find(|(n, _)| n == name).unwrap().1;
            for (j, value) in got.iter().enumerate() {
                if value != &want[j] {
                    failures.push(format!(
                        "{name} {}: want {:?} got {value:?}",
                        j + 1,
                        want[j]
                    ));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        for f in &failures {
            println!("{f}");
        }
        assert!(failures.is_empty(), "{} answers differ", failures.len());
    }

    #[test]
    #[ignore = "requires the harness's contacts-en.fmp12"]
    fn new_record_and_find_mode_focus_the_first_field_of_the_tab_order() {
        replay_focus(
            "contacts-en.fmp12",
            "s33-get-focus.contacts-en.tsv",
            "Contacts",
            ["Contact Details", "Contact List"],
        );
    }

    #[test]
    #[ignore = "requires the harness's tasks-en.fmp12"]
    fn the_tasks_templates_focus_follows_its_tab_order() {
        replay_focus(
            "tasks-en.fmp12",
            "s33-get-focus-tasks.tasks-en.tsv",
            "Tasks",
            ["Task Details", "Task List"],
        );
    }

    /// The file s33-get-privileges ran on (`s33-privileges.fmp12`: the
    /// Contacts template generated with fmapp, fmxml, fmextscriptaccess and
    /// fmrest also granted to [Full Access]), and a full-access session's
    /// free storage.
    #[test]
    #[ignore = "requires the harness's s33-privileges.fmp12"]
    fn a_full_access_session_answers_the_sets_extended_privileges() {
        let files = std::env::var_os("OM_HARNESS_FILES")
            .map_or_else(|| PathBuf::from(FILES), PathBuf::from);
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data/get/s33-get-privileges.contacts-en.tsv"),
        )
        .unwrap();
        let row: Vec<String> = text
            .lines()
            .find(|l| l.starts_with("priv probe"))
            .unwrap()
            .split('\t')
            .map(unescape)
            .collect();
        let dir = std::env::temp_dir().join(format!("om-s33-priv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut host = session(&files.join("s33-privileges.fmp12"), &dir);
        let context = host.calc_context();
        let mut eval = |e: &str| {
            let result = om_calc::eval::evaluate_text(e, &mut host, &context);
            let cx = Ctx::new(host.locale(), host.today());
            result.value.to_text(&cx).into_string()
        };
        assert_eq!(eval("Get ( CurrentExtendedPrivileges )"), row[5]);
        assert_eq!(eval("Get ( AccountExtendedPrivileges )"), row[6]);
        assert_eq!(eval("Get ( CurrentPrivilegeSetName )"), row[7]);
        assert_eq!(eval("Get ( SystemStorageAvailable ) > 0"), row[9]);
        host.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FileMaker Server's answers for each account of s33-account-
    /// privileges' file (`s33-accounts.fmp12`), replayed in a session of
    /// that account; the server's configuration gives none of them full
    /// access, so every answer comes from the file's security catalog.
    #[test]
    #[ignore = "requires the harness's s33-accounts.fmp12"]
    fn each_account_answers_its_privilege_set_from_the_file() {
        let files = std::env::var_os("OM_HARNESS_FILES")
            .map_or_else(|| PathBuf::from(FILES), PathBuf::from);
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data/get/s33-account-privileges.contacts-en.tsv"),
        )
        .unwrap();
        let expressions = [
            "Get ( AccountName )",
            "Get ( AccountPrivilegeSetName )",
            "Get ( CurrentPrivilegeSetName )",
            "Get ( AccountExtendedPrivileges )",
            "Get ( CurrentExtendedPrivileges )",
            "Get ( LayoutAccess )",
            "Get ( RecordAccess )",
            "Get ( AccountType )",
        ];
        let dir = std::env::temp_dir().join(format!("om-s33-acct-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut first = session(&files.join("s33-accounts.fmp12"), &dir);
        let (shared, hosted) = (Arc::clone(&first.shared), Arc::clone(&first.hosted));
        first.close();
        let mut failures = Vec::new();
        for (i, line) in text.lines().filter(|l| !l.starts_with('#')).enumerate() {
            let want: Vec<String> = line.split('\t').skip(5).map(unescape).collect();
            let mut host = ServerHost::new(
                Arc::clone(&shared),
                Arc::clone(&hosted),
                0x70 + i as u32,
                &want[0],
                Privileges::Unknown,
            );
            let context = host.calc_context();
            for (expression, want) in expressions.iter().zip(&want) {
                let result = om_calc::eval::evaluate_text(expression, &mut host, &context);
                let cx = Ctx::new(host.locale(), host.today());
                let got = result.value.to_text(&cx).into_string();
                if &got != want {
                    failures.push(format!("{expression}: want {want:?} got {got:?}"));
                }
            }
            host.close();
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The security catalog of s33-account-privileges' file: an account's
    /// `0b` names its set, an extended privilege lists its sets under `05`,
    /// a set's record privileges are the first byte of `02`; FileMaker
    /// Server's answers for each account, keywords in alphabetical order.
    #[test]
    fn privilege_sets_answer_as_filemaker_server_for_each_account() {
        let text = |name: &str| om_scsu::encode_scsu(name);
        // Children in key order, as in a file.
        let account = |id: u8, name: &str, set: u8| {
            OwnedNode::list(
                [id],
                vec![
                    OwnedNode::leaf([SET_REFERENCE], vec![1, set]),
                    OwnedNode::leaf([NAME], text(name)),
                ],
            )
        };
        let set = |id: u8, name: &str, records: u8| {
            OwnedNode::list(
                [id],
                vec![
                    OwnedNode::leaf([RECORD_PRIVILEGES], vec![records, 3]),
                    OwnedNode::leaf([NAME], text(name)),
                ],
            )
        };
        let extended = |id: u8, name: &str, sets: &[u8]| {
            let mut children = Vec::new();
            if !sets.is_empty() {
                children.push(OwnedNode::list(
                    [MEMBERS],
                    sets.iter()
                        .map(|s| OwnedNode::leaf([*s], Vec::new()))
                        .collect(),
                ));
            }
            children.push(OwnedNode::leaf([NAME], text(name)));
            OwnedNode::list([id], children)
        };
        let list = |key: u8, members: Vec<OwnedNode>| {
            OwnedNode::list([key], vec![OwnedNode::list([MEMBERS], members)])
        };
        let security = OwnedNode::list(
            [SECURITY],
            vec![
                list(
                    ACCOUNTS,
                    vec![
                        account(2, "Admin", 1),
                        account(3, "Entry", 2),
                        account(4, "Viewer", 3),
                        account(5, "Prober", 4),
                    ],
                ),
                list(
                    PRIVILEGE_SETS,
                    vec![
                        set(1, "[Full Access]", 0x55),
                        set(2, "[Data Entry Only]", 0x55),
                        set(3, "[Read-Only Access]", 0x10),
                        set(4, "Probe Set", 0x54),
                    ],
                ),
                list(
                    EXTENDED_PRIVILEGES,
                    vec![
                        extended(3, "fmapp", &[2]),
                        extended(4, "fmreauthenticate10", &[1, 2, 3]),
                        extended(5, "fmxml", &[4]),
                        extended(6, "fmphp", &[]),
                        extended(7, "fmextscriptaccess", &[]),
                        extended(9, "fmrest", &[2, 4]),
                    ],
                ),
            ],
        );
        let answer = |name: &str, extended: &[&str], record_access| SetPrivileges {
            name: name.into(),
            extended: extended.iter().map(|e| (*e).to_owned()).collect(),
            record_access,
        };
        let full = answer("[Full Access]", &["fmreauthenticate10"], 2);
        // Read through a view of a file holding the catalog.
        let root = OwnedNode::list(Vec::new(), vec![security.clone()]);
        let view = crate::engine::TreeView::new(&root, 0);
        let account_set = |name: &str| super::account_set(&view, name).unwrap();
        let named_set = |name: &str| super::named_set(&view, name).unwrap();
        assert_eq!(account_set("Admin"), Some(full.clone()));
        // Account names match in any case.
        assert_eq!(
            account_set("entry"),
            Some(answer(
                "[Data Entry Only]",
                &["fmapp", "fmreauthenticate10", "fmrest"],
                2
            ))
        );
        assert_eq!(
            account_set("Viewer"),
            Some(answer("[Read-Only Access]", &["fmreauthenticate10"], 1))
        );
        assert_eq!(
            account_set("Prober"),
            Some(answer("Probe Set", &["fmrest", "fmxml"], 2))
        );
        assert_eq!(account_set("Nobody"), None);
        assert_eq!(named_set("[Full Access]"), Some(full.clone()));
        assert_eq!(named_set("[Nobody]"), None);

        // Exercise real Get dispatch before, during and after elevation.
        use crate::scripts::{
            fixture,
            tests::{hosted, shared},
        };
        use om_data::host::session::selector as get;
        let accounts = ["Entry", "Viewer"].map(|name| (name, account_set(name).unwrap()));
        let mut tree = fixture::tree(&[], true);
        tree.put(&[&[SECURITY]], security);
        let file = fixture::TempFile::from_tree("elevated-get-privileges", &tree);
        let server = shared(&file);
        let answer = |host: &mut ServerHost, selector: u8| {
            let value = CalcHost::get(host, selector, &Context::new()).unwrap();
            value
                .to_text(&Ctx::new(host.locale(), host.today()))
                .into_string()
        };
        for (index, (account, own)) in accounts.into_iter().enumerate() {
            let mut host = ServerHost::new(
                Arc::clone(&server),
                hosted(&server),
                0x5500 + index as u32,
                account,
                Privileges::Unknown,
            );
            for elevated in [false, true, false] {
                host.elevated = elevated;
                let effective = if elevated { &full } else { &own };
                assert_eq!(
                    answer(&mut host, get::CURRENT_PRIVILEGE_SET_NAME),
                    effective.name
                );
                assert_eq!(answer(&mut host, get::ACCOUNT_PRIVILEGE_SET_NAME), own.name);
                assert_eq!(
                    answer(&mut host, get::CURRENT_EXTENDED_PRIVILEGES),
                    effective.extended.join("\r")
                );
                assert_eq!(
                    answer(&mut host, get::ACCOUNT_EXTENDED_PRIVILEGES),
                    own.extended.join("\r")
                );
                assert_eq!(
                    answer(&mut host, get::RECORD_ACCESS),
                    effective.record_access.to_string()
                );
                assert_eq!(answer(&mut host, get::LAYOUT_ACCESS), "2");
            }
            host.close();
        }
    }
}
