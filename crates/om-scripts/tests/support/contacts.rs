//! The probe file of the script oracle's batteries in memory: the subset of
//! the Contacts starter template (`DynamicTemplates/Contacts/en.fmp12`) the
//! probes touch, opened the way the harness opens its scratch copy
//! `work.fmp12` in a server session (script-runtime.md 13.1).
//!
//! Ids are the template's (catalog serial ids; `file_metadata` and
//! `inspect_scripts` print them). `the_fixture_matches_the_template` in
//! oracle.rs checks names, ids and field kinds against the template when it
//! is installed.

use std::sync::Arc;

use om_calc::eval::FieldInfo;
use om_calc::{CalcError, Date, Locale, Timestamp, Value, ValueType};
use om_scripts::runtime::{MemoryLayout, MemoryScriptHost, Mode, SessionFunction, TriggerEvent};
use om_scripts::{flag, Script, ScriptKind, ScriptStep};
use om_sql::{FieldClass, FieldDef, FieldType, SqlError, SqlSource, TableDef};

/// The scratch copy's name: `Get ( FileName )` and the window's name.
pub const FILE_NAME: &str = "work";
pub const CONTACTS_TABLE: u64 = 130;
pub const CONTACTS: u64 = 1_065_090;
/// The file opens on Contact Details (the document preferences' default
/// layout).
pub const DEFAULT_LAYOUT: u64 = 3;
pub const LAYOUTS: [(u64, &str); 3] =
    [(1, "Contacts"), (2, "Contact List"), (3, "Contact Details")];
/// Sort by Name (script 3) runs OnLayoutEnter of Contact List, in Browse
/// mode.
pub const SORT_BY_NAME: u64 = 3;
pub const CONTACT_LIST: u64 = 2;

/// The Contacts table's fields: id, name, type; `Initial` is a calculation.
pub const FIELDS: [(u64, &str, ValueType); 13] = [
    (1, "First Name", ValueType::Text),
    (2, "Last Name", ValueType::Text),
    (3, "Photo", ValueType::Container),
    (4, "Title", ValueType::Text),
    (5, "Company", ValueType::Text),
    (6, "Job Title", ValueType::Text),
    (7, "Website", ValueType::Text),
    (8, "Initial", ValueType::Text),
    (9, "PrimaryKey", ValueType::Text),
    (10, "CreatedBy", ValueType::Text),
    (11, "ModifiedBy", ValueType::Text),
    (12, "CreationTimestamp", ValueType::Timestamp),
    (13, "ModificationTimestamp", ValueType::Timestamp),
];
pub const INITIAL: u64 = 8;
/// `Contacts::Initial` (calc-encoding.md's example).
const INITIAL_FORMULA: &str = "Case ( not IsEmpty ( Last Name ) ; Left ( Last Name ; 1 ) ; \
                               not IsEmpty ( First Name ) ; Left ( First Name ; 1 ) )";

/// `Get` answers of the harness's server session that come from the host
/// (13.1): FileMaker Server's script engine, the security-bypass account,
/// an unhosted file.
const SESSION: [(&str, &str); 12] = [
    ("ApplicationVersion", "Server 26.0.3"),
    ("AccountName", "Admin"),
    ("AccountPrivilegeSetName", "[Read-Only Access]"),
    ("UserName", "Admin"),
    ("SystemPlatform", "8"),
    ("Device", "6"),
    ("MultiUserState", "0"),
    ("ApplicationLanguage", "English"),
    ("SystemLanguage", "English"),
    ("WindowVisible", "1"),
    ("FileName", FILE_NAME),
    ("PersistentID", "D41D8CD98F00B204E9800998ECF8427E"),
];

/// A new session on the file: no records, the window on the default
/// layout, the template's scripts, layout trigger and `ExecuteSQL`.
pub fn host() -> MemoryScriptHost {
    let mut host =
        MemoryScriptHost::new(Locale::en_us(), Date::from_ymd(2026, 10, 29).expect("date"));
    host.add_file(0, FILE_NAME);
    host.add_table(CONTACTS_TABLE, "Contacts");
    host.add_occurrence(CONTACTS, "Contacts", CONTACTS_TABLE);
    for (id, name, ty) in FIELDS {
        if id != INITIAL {
            host.add_field(CONTACTS_TABLE, id, name, FieldInfo::stored(ty, 1));
        }
    }
    host.calc
        .add_calculation(
            CONTACTS,
            INITIAL,
            "Initial",
            FieldInfo::stored(ValueType::Text, 1),
            INITIAL_FORMULA,
        )
        .expect("Initial compiles");
    for (id, name) in LAYOUTS {
        host.add_layout(id, name, CONTACTS);
    }
    host.layout_triggers
        .insert((CONTACT_LIST, TriggerEvent::LayoutEnter), (0, SORT_BY_NAME));
    for (name, kind, id) in [
        ("Contacts", ScriptKind::Folder, 1),
        ("--", ScriptKind::FolderEnd, 4),
    ] {
        host.add_script(
            0,
            Script {
                id,
                name: name.into(),
                kind,
                ..Default::default()
            },
        );
    }
    // Sort by Name: Sort Records [ Restore ; With dialog: Off ].
    let mut sort = ScriptStep::new(39);
    sort.set_flag(flag::RESTORE | flag::NO_INTERACT, true);
    let mut script = Script {
        id: SORT_BY_NAME,
        name: "Sort by Name".into(),
        ..Default::default()
    };
    script.insert_steps(0, vec![sort]).expect("nests");
    host.add_script(0, script);
    for (name, value) in SESSION {
        let selector = selector(name);
        host.calc.set_get(selector, Value::text(value));
    }
    let noon = om_calc::Time::from_micros(12 * 3600 * 1_000_000, om_calc::TimeStyle::TimeOfDay);
    let now = Timestamp::from_date_time(host.calc.today, noon).expect("timestamp");
    host.calc
        .set_get(selector("CurrentHostTimestamp"), Value::Timestamp(now));
    // The file is not hosted: Perform Script on Server is 3 (13.7).
    host.server_scripts = false;
    // No PDF is being written: Cancel PDF does nothing (13.7).
    host.other_results.insert(247, Value::empty_text());
    let sql: SessionFunction = Arc::new(|host: &mut MemoryScriptHost, args: &[Value]| {
        om_sql::calc::execute_sql(&mut Source(host), om_sql::calc::Variant::ExecuteSql, args)
    });
    host.functions.insert("executesql".into(), sql);
    open_window(&mut host);
    host
}

/// What a new session sees: the window on the default layout in Browse
/// mode with every record, the last one current (script-runtime.md 13.17;
/// record changes stay in the file from run to run).
pub fn open_window(host: &mut MemoryScriptHost) {
    let found = host.records_of(CONTACTS);
    host.current_window = 0;
    let window = &mut host.windows[0];
    window.layout = DEFAULT_LAYOUT;
    window.mode = Mode::Browse;
    window.current = found.len().saturating_sub(1);
    window.found = found;
    window.focus = None;
}

/// A `Get` selector's number by its catalog name.
fn selector(name: &str) -> u8 {
    (0..=u8::MAX)
        .find(|&s| om_formula::catalog::get_selector_name(s) == Some(name))
        .unwrap_or_else(|| panic!("no selector {name}"))
}

/// The layouts as the reader looks them up.
pub fn layouts(host: &MemoryScriptHost) -> Vec<(String, u64)> {
    host.layouts
        .iter()
        .map(|l: &MemoryLayout| (l.name.clone(), l.id))
        .collect()
}

/// ExecuteSQL's view of the session: the Contacts occurrence, its fields
/// and records.
struct Source<'a>(&'a mut MemoryScriptHost);

impl SqlSource for Source<'_> {
    fn locale(&self) -> &Locale {
        &self.0.calc.locale
    }

    fn now(&self) -> Timestamp {
        self.0.calc.today.to_timestamp()
    }

    fn tables(&self) -> Vec<TableDef> {
        vec![TableDef {
            id: CONTACTS,
            name: "Contacts".into(),
            table_id: CONTACTS_TABLE,
            base: Some(("Contacts".into(), FILE_NAME.into())),
            mod_count: 0,
        }]
    }

    fn fields(&self, _table: &TableDef) -> Vec<FieldDef> {
        FIELDS
            .iter()
            .map(|&(id, name, ty)| FieldDef {
                id,
                name: name.into(),
                ty: match ty {
                    ValueType::Container => FieldType::Container,
                    ValueType::Timestamp => FieldType::Timestamp,
                    _ => FieldType::Text,
                },
                class: if id == INITIAL {
                    FieldClass::Calculated
                } else {
                    FieldClass::Normal
                },
                global: false,
                repetitions: 1,
                mod_count: 0,
            })
            .collect()
    }

    fn records(&mut self, _table: &TableDef) -> Result<Vec<u64>, SqlError> {
        Ok(self.0.records_of(CONTACTS))
    }

    fn value(
        &mut self,
        _table: &TableDef,
        record: u64,
        field: &FieldDef,
        repetition: u32,
        _cursor: om_sql::source::Cursor,
    ) -> Result<Value, CalcError> {
        use om_calc::eval::CalcHost;
        self.0
            .calc
            .field_value(CONTACTS, record, field.id, repetition)
    }
}
