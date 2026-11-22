//! FileMaker Pro 26.0.3's answers for a local file (calc-functions.md 6.3)
//! replayed through the calculation host a desktop client evaluates with
//! (`om_data::host::FileHost`), given the session FileMaker Pro's window
//! had: the window at 100, 120 of the desktop area, focus and selection,
//! Find mode, the record's open state, Preview, layout objects in screen
//! points at each zoom and toolbar state, and other open files.
//!
//! The always-run tests build Contact Details from the template's own
//! numbers; the ignored ones read the shipped templates the harness keeps
//! (`OM_HARNESS_FILES`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use om_data::calc::eval::{evaluate_text, Context, Variables};
use om_data::calc::{Ctx, Date, Locale, ValueType};
use om_data::host::{
    stored_value, CalcSchema, FieldDef, FileCalc, FileHost, Frame, LayoutEntry, MemorySource,
    OpenFile, Privileges, RecordValues, SessionInfo, WindowInfo, WindowMode, NO_RECORD,
};
use om_layout::{
    FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject, ObjectStore, ObjectStyle, Part, Rect,
};

const FILES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../re/work/fms-harness/files"
);
const CONTACTS: u64 = 5;
const PHONES: u64 = 6;
const RECORD: u64 = 1;

fn today() -> Date {
    Date::from_ymd(2026, 10, 29).unwrap()
}

fn rect(left: f64, top: f64, right: f64, bottom: f64) -> Option<Rect> {
    Some(Rect {
        left,
        top,
        right,
        bottom,
        ..Rect::default()
    })
}

fn field_object(
    id: u64,
    name: Option<&str>,
    bounds: [f64; 4],
    flags: u64,
    field: u64,
) -> LayoutObject {
    LayoutObject {
        id,
        object_type: 1,
        flags,
        name: name.map(str::to_owned),
        bounds: rect(bounds[0], bounds[1], bounds[2], bounds[3]),
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key: CONTACTS,
                field_key: field,
                repetition: 1,
                ..FieldSpec::default()
            }),
            num_of_reps: 1,
            ..FieldObject::default()
        }),
        uuid: Some([
            0xbd, 0x9e, 0xb7, 0xe8, 0x12, 0x37, 0x46, 0x4d, 0xb0, 0xa2, 0x42, 0xe0, 0x58, 0xe0,
            0x0e, 0xc9,
        ]),
        ..LayoutObject::default()
    }
}

/// The Contacts template's Contact Details as `om-layout` reads it: the
/// top navigation part (44 points) and the body (528), 1024 points wide;
/// "Back to Contacts" in the top navigation; First Name anchored left,
/// right and bottom; Company and the web viewer on the front panel of a
/// tab control anchored top, right and bottom; the web viewer stretching
/// down.
fn contact_details() -> Layout {
    let objects = vec![
        LayoutObject {
            id: 3,
            object_type: 10,
            flags: 0x3002_4000,
            name: Some("Back to Contacts".into()),
            bounds: rect(0.0, 0.0, 200.0, 44.0),
            text: Some("Contacts".into()),
            ..LayoutObject::default()
        },
        LayoutObject {
            id: 4,
            object_type: 11,
            flags: 0xe000_0000,
            bounds: rect(349.0, 116.0, 1008.0, 557.0),
            children: vec![5],
            ..LayoutObject::default()
        },
        LayoutObject {
            id: 5,
            object_type: 12,
            flags: 0xe000_0000,
            parent: Some(4),
            bounds: rect(0.0, 0.0, 659.0, 441.0),
            children: vec![7, 10],
            ..LayoutObject::default()
        },
        LayoutObject {
            id: 7,
            object_type: 13,
            flags: 0xb000_0000,
            parent: Some(5),
            name: Some("website".into()),
            bounds: rect(17.0, 145.0, 642.0, 425.0),
            style: Some(ObjectStyle {
                object_type_id: 86,
                ..ObjectStyle::default()
            }),
            ..LayoutObject::default()
        },
        LayoutObject {
            parent: Some(5),
            ..field_object(
                10,
                Some("Company"),
                [17.0, 90.0, 317.0, 134.0],
                0x3000_4000,
                5,
            )
        },
        field_object(
            39,
            Some("First Name"),
            [32.0, 393.0, 323.0, 437.0],
            0xd000_4000,
            1,
        ),
    ];
    Layout {
        id: 3,
        name: "Contact Details".into(),
        def: Some(LayoutDef {
            base_width: 1024.0,
            table_alias_key: CONTACTS,
            ..LayoutDef::default()
        }),
        parts: vec![
            Part {
                id: 1,
                part_type: 12,
                top: 0.0,
                height: 44.0,
                ..Part::default()
            },
            Part {
                id: 2,
                part_type: 4,
                top: 44.0,
                height: 528.0,
                ..Part::default()
            },
        ],
        objects: ObjectStore {
            root: vec![3, 4, 39],
            objects,
            ..ObjectStore::default()
        },
        ..Layout::default()
    }
}

/// Contacts with its Phone Numbers, and Contact List before Contact
/// Details.
fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Contacts", Locale::en_us());
    s.add_table(CONTACTS, "Contacts");
    s.add_table(PHONES, "Phone Numbers");
    s.add_occurrence(CONTACTS, "Contacts", CONTACTS);
    s.add_occurrence(PHONES, "Phone Numbers", PHONES);
    for (id, name) in [(1, "First Name"), (2, "Last Name"), (5, "Company")] {
        s.add_field(CONTACTS, FieldDef::data(id, name, ValueType::Text));
    }
    s.add_field(
        CONTACTS,
        FieldDef::data(14, "zOut", ValueType::Text).with_global(true),
    );
    s.add_field(PHONES, FieldDef::data(1, "Number", ValueType::Text));
    s.add_layout_entry(LayoutEntry::new(2, "Contact List", Some(CONTACTS)));
    s.add_layout_entry(LayoutEntry {
        layout: Some(Arc::new(contact_details())),
        ..LayoutEntry::new(3, "Contact Details", Some(CONTACTS))
    });
    s
}

fn source(schema: &CalcSchema) -> MemorySource {
    let cx = Ctx::new(&schema.locale, today());
    let mut source = MemorySource::new();
    let mut values = RecordValues::default();
    values.set(1, 1, stored_value("Alice", ValueType::Text, &cx));
    values.set(2, 1, stored_value("Archer", ValueType::Text, &cx));
    source.put_record(CONTACTS, RECORD, values);
    let mut globals = RecordValues::default();
    globals.set(14, 1, stored_value("out", ValueType::Text, &cx));
    source.set_globals(CONTACTS, globals);
    source.set_found_set(CONTACTS, &[RECORD], None);
    source
}

/// FileMaker Pro's window of the probe: 1100 x 900 at 100, 120 of a 1854 x
/// 1050 desktop area that starts 66 points right of and 30 below the top
/// left of a 1920 x 1080 screen; 105 points of chrome with the toolbar, 28
/// and 52 points less window without it.
fn probe_window(toolbar: bool, zoom: u16) -> WindowInfo {
    let chrome = if toolbar { 105.0 } else { 28.0 };
    let height = if toolbar { 900.0 } else { 848.0 };
    WindowInfo {
        screen: Frame::new(0.0, 0.0, 1920.0, 1080.0),
        desktop: Frame::new(66.0, 30.0, 1854.0, 1050.0),
        frame: Frame::new(166.0, 150.0, 1100.0, height),
        content: Frame::new(166.0, 150.0 + chrome, 1100.0, height - chrome),
        zoom,
        toolbar,
        uuid: "CB1FCC2C5F55438CA3EC703A52FA87BA".into(),
        ..WindowInfo::default()
    }
}

/// FileMaker Pro's session on Contact Details, record 1 of 40.
fn session() -> SessionInfo {
    SessionInfo {
        account_name: "Admin".into(),
        account_info: om_data::host::AccountInfo {
            account_type: Some("FileMaker File".into()),
            password_expiry: Some(-1),
            set_access: None,
        },
        privilege_set_name: "[Full Access]".into(),
        privileges: Some(Privileges {
            account_set_name: "[Full Access]".into(),
            extended: vec!["fmextscriptaccess".into(), "fmreauthenticate10".into()],
            layout_access: Some(2),
            record_access: Some(2),
        }),
        file_name: "Contacts".into(),
        file_path: "file:/Macintosh HD/probe/Contacts.fmp12".into(),
        file_size: Some(921_600),
        multi_user_state: 0,
        window_name: "Contacts".into(),
        window: Some(probe_window(true, 100)),
        layout_name: "Contact Details".into(),
        layout_number: Some(2),
        layout_table_name: "Contacts".into(),
        layout_count: Some(2),
        layout_view_state: Some(0),
        found_count: Some(40),
        active_record_number: Some(1),
        ..SessionInfo::for_this_machine()
    }
}

/// `expression` as the probe recorded it: the text, then `#<error>` when
/// the evaluation failed.
fn eval_in(
    schema: &CalcSchema,
    source: &MemorySource,
    session: &SessionInfo,
    context: &Context,
    expression: &str,
) -> String {
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(schema, source, today()),
        session,
        &mut locals,
        &mut globals,
    );
    let result = evaluate_text(expression, &mut host, context);
    let cx = Ctx::new(&schema.locale, today());
    let text = result.value.to_text(&cx).into_string();
    match result.code() {
        0 => text,
        code => format!("{text}#{code}"),
    }
}

fn check(session: &SessionInfo, context: &Context, rows: &[(&str, &str)]) {
    let schema = schema();
    let source = source(&schema);
    let failures: Vec<String> = rows
        .iter()
        .filter_map(|(expression, want)| {
            let got = eval_in(&schema, &source, session, context, expression);
            (got != *want).then(|| format!("{expression}: want {want:?} got {got:?}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn on_record() -> Context {
    Context::record(CONTACTS, RECORD)
}

#[test]
fn o1_session_window_and_machine() {
    let platform = if cfg!(target_os = "macos") {
        "11Aqua"
    } else if cfg!(windows) {
        "-22Light"
    } else {
        "86"
    };
    let rows = [
        ("Get ( FileSize )", "921600"),
        ("Get ( SystemLanguage )", "English"),
        ("Get ( ScreenDepth )", "32"),
        ("Get ( ScreenHeight )", "1080"),
        ("Get ( ScreenWidth )", "1920"),
        ("Get ( UserCount )", "1"),
        ("Get ( CurrentPrivilegeSetName )", "[Full Access]"),
        ("Get ( AccountPrivilegeSetName )", "[Full Access]"),
        (
            "Get ( CurrentExtendedPrivileges )",
            "fmextscriptaccess\rfmreauthenticate10",
        ),
        (
            "Get ( AccountExtendedPrivileges )",
            "fmextscriptaccess\rfmreauthenticate10",
        ),
        ("Get ( AccountGroupName )", ""),
        ("Get ( AccountPasswordDaysRemaining )", "-1"),
        ("Get ( LayoutAccess )", "2"),
        ("Get ( RecordAccess )", "2"),
        ("Get ( HighContrastState )", "0"),
        ("Get ( StatusAreaState )", "1"),
        ("Get ( WindowHeight )", "900"),
        ("Get ( WindowLeft )", "100"),
        ("Get ( WindowTop )", "120"),
        ("Get ( WindowVisible )", "1"),
        ("Get ( WindowWidth )", "1100"),
        ("Get ( WindowDesktopHeight )", "1050"),
        ("Get ( WindowDesktopWidth )", "1854"),
        ("Get ( WindowContentHeight )", "795"),
        ("Get ( WindowContentWidth )", "1100"),
        ("Get ( WindowZoomLevel )", "100"),
        ("Get ( WindowStyle )", "0"),
        ("Get ( WindowOrientation )", "-1"),
        ("Get ( WindowUUID )", "CB1FCC2C5F55438CA3EC703A52FA87BA"),
        ("Get ( MenubarState )", "1"),
        ("Get ( TextRulerVisible )", "0"),
        ("Get ( AllowFormattingBarState )", "1"),
        ("Get ( CustomMenuSetName )", ""),
        ("Get ( HostIPAddress )", ""),
        ("Get ( HostApplicationVersion )", ""),
        (
            "GetAsNumber ( Get ( CurrentHostTimestamp ) - Get ( CurrentTimestamp ) )",
            "0",
        ),
        ("Length ( Get ( PersistentID ) )", "32"),
        ("Get ( ConnectionState )", "0"),
        ("Get ( ConnectionAttributes )", ""),
        ("Get ( EncryptionState )", "0"),
        ("Get ( ScreenScaleFactor )", "1"),
        ("Get ( SystemStorageAvailable ) > 0", "1"),
        (
            "JSONGetElement ( Get ( InstalledFMPluginsAsJSON ) ; \"APIVers\" )",
            "2",
        ),
        ("Get ( InstalledFMPlugins )", ""),
        (
            "JSONGetElement ( Get ( SystemLocaleElements ) ; \"Misc.Active\" )",
            "1",
        ),
        (
            "JSONGetElement ( Get ( FileLocaleElements ) ; \"Misc.Active\" )",
            "0",
        ),
        ("Get ( ApplicationLanguage )", "English"),
        ("Length ( Get ( UUIDNumber ) ) >= 50", "1"),
        ("Get ( ApplicationVersion )", "Pro 26.0.3"),
        ("Get ( WindowName )", "Contacts"),
        ("Get ( WindowMode )", "0"),
        ("Get ( LayoutName )", "Contact Details"),
        ("Get ( LayoutViewState )", "0"),
        ("Get ( AccountName )", "Admin"),
        ("Get ( AccountType )", "FileMaker File"),
        ("Get ( MultiUserState )", "0"),
        ("Get ( NetworkProtocol )", "TCP/IP"),
        ("Get ( NetworkType )", ""),
        ("Get ( DBEngineVersion )", "260030307"),
        ("Get ( StudioDatasource )", "0"),
        ("Get ( LayoutCount )", "2"),
        ("Get ( LayoutNumber )", "2"),
        ("Get ( LayoutTableName )", "Contacts"),
        ("Get ( FoundCount )", "40"),
        ("Get ( RecordNumber )", "1"),
        ("Get ( ActiveFieldName )", ""),
        ("Get ( SortState )", "0"),
        ("Get ( UseSystemFormatsState )", "1"),
        ("Get ( TouchKeyboardState )", "0"),
        ("Get ( GuidedAccessState )", ""),
        ("Get ( RegionMonitorEvents )", ""),
        ("Get ( LastStepTokensUsed )", ""),
        ("Get ( PageNumber )", "0"),
        ("Get ( PageCount )", ""),
        ("Get ( PrinterName )", " "),
        ("Get ( QuickFindText )", ""),
        (
            "Get ( SystemPlatform ) & Get ( Device ) & Get ( SystemAppearance )",
            platform,
        ),
    ];
    check(&session(), &on_record(), &rows);

    // The machine's own facts in FileMaker's forms.
    let schema = schema();
    let source = source(&schema);
    let eval = |e: &str| eval_in(&schema, &source, &session(), &on_record(), e);
    let nic = eval("Get ( SystemNICAddress )");
    assert!(nic
        .split('\r')
        .filter(|a| !a.is_empty())
        .all(|a| a.len() == 17 && a == a.to_lowercase()));
    let ip = eval("Get ( SystemIPAddress )");
    assert!(ip
        .split('\r')
        .filter(|a| !a.is_empty())
        .all(|a| a.split('.').count() == 4));
    assert_ne!(eval("Get ( UUIDNumber )"), eval("Get ( UUIDNumber )"));

    // The toolbar hidden.
    let hidden = SessionInfo {
        window: Some(probe_window(false, 100)),
        ..session()
    };
    check(
        &hidden,
        &on_record(),
        &[
            ("Get ( StatusAreaState )", "0"),
            ("Get ( WindowTop )", "120"),
            ("Get ( WindowLeft )", "100"),
            ("Get ( WindowHeight )", "848"),
            ("Get ( WindowWidth )", "1100"),
            ("Get ( WindowContentHeight )", "820"),
            ("Get ( WindowContentWidth )", "1100"),
            ("Get ( WindowDesktopHeight )", "1050"),
            ("Get ( WindowDesktopWidth )", "1854"),
            ("Get ( WindowZoomLevel )", "100"),
        ],
    );
    // The window moved to 0, 0 and to 10, 40 of the desktop area: the top
    // navigation's "Back to Contacts" follows the content's top left.
    for (left, top, bounds) in [
        (0.0, 0.0, "0 105 200 149 0"),
        (10.0, 40.0, "10 145 210 189 0"),
    ] {
        let mut window = probe_window(true, 100);
        window.frame.left = 66.0 + left;
        window.frame.top = 30.0 + top;
        window.content.left = window.frame.left;
        window.content.top = window.frame.top + 105.0;
        let moved = SessionInfo {
            window: Some(window),
            ..session()
        };
        let (top, left) = (format!("{top}"), format!("{left}"));
        check(
            &moved,
            &on_record(),
            &[
                ("Get ( WindowTop )", &top),
                ("Get ( WindowLeft )", &left),
                ("Get ( WindowHeight )", "900"),
                ("Get ( WindowWidth )", "1100"),
                (
                    "GetLayoutObjectAttribute ( \"Back to Contacts\" ; \"bounds\" )",
                    bounds,
                ),
            ],
        );
    }
}

#[test]
fn o2_focus_selection_and_trigger_panels() {
    // First Name active, Ann typed and nn selected: the record is open,
    // but typing is not a modified field yet.
    let typing = SessionInfo {
        active_field_name: "First Name".into(),
        active_field_table_name: "Contacts".into(),
        active_field_contents: "Ann".into(),
        active_selection: Some((2, 2)),
        record_open_state: Some(2),
        ..session()
    };
    check(
        &typing,
        &on_record(),
        &[
            ("Get ( ActiveFieldName )", "First Name"),
            ("Get ( ActiveFieldTableName )", "Contacts"),
            ("Get ( ActiveFieldContents )", "Ann"),
            ("Get ( ActiveRepetitionNumber )", "1"),
            ("Get ( ActiveSelectionStart )", "2"),
            ("Get ( ActiveSelectionSize )", "2"),
            ("Get ( ActiveModifierKeys )", "0"),
            ("Get ( ActiveLayoutObjectName )", "First Name"),
            ("Get ( ActivePortalRowNumber )", "0"),
            ("Get ( RecordOpenState )", "2"),
            ("Get ( RecordOpenCount )", "1"),
            ("Get ( ModifiedFields )", ""),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"isActive\" )",
                "0",
            ),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"hasFocus\" )",
                "1",
            ),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"containsFocus\" )",
                "1",
            ),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"source\" )",
                "Contacts::First Name",
            ),
        ],
    );
    // The second row of the Phone portal: the field's own occurrence, the
    // insertion point after its eight characters, no object name.
    let portal = SessionInfo {
        active_field_name: "Number".into(),
        active_field_table_name: "Phone Numbers".into(),
        active_field_contents: "555-0102".into(),
        active_portal_row_number: Some(2),
        ..session()
    };
    check(
        &portal,
        &on_record(),
        &[
            ("Get ( ActiveFieldName )", "Number"),
            ("Get ( ActiveFieldTableName )", "Phone Numbers"),
            ("Get ( ActiveFieldContents )", "555-0102"),
            ("Get ( ActiveRepetitionNumber )", "1"),
            ("Get ( ActiveSelectionStart )", "9"),
            ("Get ( ActiveSelectionSize )", "0"),
            ("Get ( ActiveLayoutObjectName )", ""),
            ("Get ( ActivePortalRowNumber )", "2"),
            ("Get ( RecordOpenState )", "0"),
            ("Get ( RecordOpenCount )", "0"),
        ],
    );
    // No active field; outside a trigger the panels are 0 and a CR.
    check(
        &session(),
        &on_record(),
        &[
            ("Get ( ActiveFieldName )", ""),
            ("Get ( ActiveFieldTableName )", ""),
            ("Get ( ActiveFieldContents )", ""),
            ("Get ( ActiveRepetitionNumber )", "0"),
            ("Get ( ActiveSelectionStart )", ""),
            ("Get ( ActiveSelectionSize )", ""),
            ("Get ( ActiveModifierKeys )", "0"),
            ("Get ( ActiveLayoutObjectName )", ""),
            ("Get ( ActivePortalRowNumber )", "0"),
            ("Get ( RecordOpenState )", "0"),
            ("Get ( RecordOpenCount )", "0"),
            ("Get ( ModifiedFields )", ""),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"content\" )",
                "Alice",
            ),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"hasFocus\" )",
                "0",
            ),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"containsFocus\" )",
                "0",
            ),
            ("Length ( Get ( TriggerCurrentPanel ) )", "2"),
            ("Code ( Get ( TriggerCurrentPanel ) )", "1300048"),
            ("Length ( Get ( TriggerTargetPanel ) )", "2"),
            ("Code ( Get ( TriggerTargetPanel ) )", "1300048"),
            ("Get ( TriggerKeystroke )", ""),
            ("Get ( TriggerModifierKeys )", ""),
            ("Get ( TriggerExternalEvent )", ""),
            ("Get ( TriggerGestureInfo )", ""),
        ],
    );
    // In the active window of the active application the focused field
    // is active [I].
    let mut window = probe_window(true, 100);
    window.active = true;
    let active = SessionInfo {
        window: Some(window),
        ..typing
    };
    check(
        &active,
        &on_record(),
        &[(
            "GetLayoutObjectAttribute ( \"First Name\" ; \"isActive\" )",
            "1",
        )],
    );
}

#[test]
fn o3_find_mode_sort_open_records_and_an_empty_found_set() {
    // Find mode, the second of two requests, which omits: counts are the
    // requests', and fields read empty or their values, not 1224.
    let find = SessionInfo {
        window_mode: WindowMode::Find,
        request_count: 2,
        request_omit: true,
        found_count: Some(2),
        active_record_number: Some(2),
        ..session()
    };
    let no_record = Context::record(CONTACTS, NO_RECORD);
    check(
        &find,
        &no_record,
        &[
            ("Get ( RequestCount )", "2"),
            ("Get ( RequestOmitState )", "1"),
            ("Get ( WindowMode )", "1"),
            ("Get ( RecordNumber )", "2"),
            ("Get ( ActiveRecordNumber )", "2"),
            ("Get ( FoundCount )", "2"),
            ("Contacts::First Name", ""),
            ("Contacts::zOut", "out"),
            ("EvaluationError ( Contacts::First Name )", "0"),
        ],
    );
    // Back in Browse mode without a find the count stays.
    let browse = SessionInfo {
        request_count: 2,
        request_omit: true,
        ..session()
    };
    check(
        &browse,
        &on_record(),
        &[
            ("Get ( RequestCount )", "2"),
            ("Get ( RequestOmitState )", "0"),
            ("Get ( WindowMode )", "0"),
            ("Get ( FoundCount )", "40"),
        ],
    );
    // Set Field into First Name: the field and the stored calculation on
    // it.
    let set = SessionInfo {
        record_open_state: Some(2),
        modified_fields: vec!["First Name".into(), "Initial".into()],
        ..session()
    };
    check(
        &set,
        &on_record(),
        &[
            ("Get ( RecordOpenState )", "2"),
            ("Get ( RecordOpenCount )", "1"),
            ("Get ( ModifiedFields )", "First Name\rInitial"),
        ],
    );
    let sorted_new = SessionInfo {
        sorted: true,
        record_open_state: Some(1),
        ..session()
    };
    check(
        &sorted_new,
        &on_record(),
        &[
            ("Get ( SortState )", "1"),
            ("Get ( RecordOpenState )", "1"),
            ("Get ( RecordModificationCount )", "0"),
        ],
    );
    // An empty found set (a find for nothing): no record.
    let empty = SessionInfo {
        found_count: Some(0),
        active_record_number: Some(0),
        ..session()
    };
    check(
        &empty,
        &no_record,
        &[
            ("Get ( FoundCount )", "0"),
            ("Get ( RecordID )", "0"),
            ("Get ( RecordModificationCount )", "0"),
            ("Get ( ActiveRecordNumber )", "0"),
            ("Get ( RecordNumber )", "0"),
            ("Get ( WindowMode )", "0"),
            ("Get ( RecordOpenState )", "0"),
            ("Get ( SortState )", "0"),
            ("Contacts::First Name", ""),
            ("EvaluationError ( Contacts::First Name )", "0"),
            ("Contacts::zOut", "out"),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"content\" )",
                "",
            ),
            ("Get ( QuickFindText )", ""),
        ],
    );
}

#[test]
fn o4_preview_pages_without_page_count() {
    let preview = SessionInfo {
        window_mode: WindowMode::Preview,
        page_number: Some(2),
        ..session()
    };
    check(
        &preview,
        &on_record(),
        &[
            ("Get ( WindowMode )", "2"),
            ("Get ( PageNumber )", "2"),
            ("Get ( PageCount )", ""),
            ("Get ( PrinterName )", " "),
            ("Get ( RecordNumber )", "1"),
            ("Get ( FoundCount )", "40"),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"bounds\" )",
                "",
            ),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"content\" )",
                "Alice",
            ),
        ],
    );
}

#[test]
fn o5_layout_objects_in_screen_points() {
    let states = [
        (
            true,
            100,
            [
                "132 841 499 885 0",
                "542 431 842 475 0",
                "542 486 1167 989 0",
                "100 225 300 269 0",
            ],
        ),
        (
            false,
            100,
            [
                "132 789 499 833 0",
                "542 354 842 398 0",
                "542 409 1167 937 0",
                "100 148 300 192 0",
            ],
        ),
        (
            true,
            150,
            [
                "148 793 585 859 0",
                "649 512 1099 578 0",
                "649 595 1587 1015 0",
                "100 225 300 269 0",
            ],
        ),
        (
            false,
            150,
            [
                "148 716 585 782 0",
                "649 435 1099 501 0",
                "649 518 1587 938 0",
                "100 148 300 192 0",
            ],
        ),
    ];
    for (toolbar, zoom, [first, company, website, back]) in states {
        let state = SessionInfo {
            window: Some(probe_window(toolbar, zoom)),
            ..session()
        };
        let zoom = zoom.to_string();
        check(
            &state,
            &on_record(),
            &[
                (
                    "GetLayoutObjectAttribute ( \"First Name\" ; \"bounds\" )",
                    first,
                ),
                (
                    "GetLayoutObjectAttribute ( \"Company\" ; \"bounds\" )",
                    company,
                ),
                (
                    "GetLayoutObjectAttribute ( \"website\" ; \"bounds\" )",
                    website,
                ),
                (
                    "GetLayoutObjectAttribute ( \"Company\" ; \"bounds\" ; 2 )",
                    "",
                ),
                (
                    "GetLayoutObjectAttribute ( \"Back to Contacts\" ; \"bounds\" )",
                    back,
                ),
                (
                    "GetLayoutObjectAttribute ( \"First Name\" ; \"content\" )",
                    "Alice",
                ),
                (
                    "GetLayoutObjectAttribute ( \"First Name\" ; \"isActive\" )",
                    "0",
                ),
                (
                    "GetLayoutObjectAttribute ( \"First Name\" ; \"hasFocus\" )",
                    "0",
                ),
                ("Get ( WindowZoomLevel )", &zoom),
            ],
        );
    }
    // The web viewer has no page on the desktop: no source or content.
    // Owner information answers the not-found document; LayoutObjectUUID
    // outside an object's calculation is error 10.
    let not_found = "{\r\t\"APIVers\" : 1,\r\t\"Object\" : \r\t{\r\t\t\"Index\" : 4294967295,\r\t\t\"Name\" : \"\",\r\t\t\"Owners\" : \r\t\t{\r\t\t\t\"Layout\" : \r\t\t\t{\r\t\t\t\t\"Name\" : \"\",\r\t\t\t\t\"UUID\" : \"\"\r\t\t\t}\r\t\t}\r\t}\r}";
    check(
        &session(),
        &on_record(),
        &[
            (
                "GetLayoutObjectAttribute ( \"website\" ; \"objectType\" )",
                "web viewer",
            ),
            (
                "GetLayoutObjectAttribute ( \"website\" ; \"isObjectHidden\" )",
                "0",
            ),
            (
                "GetLayoutObjectAttribute ( \"website\" ; \"source\" )",
                "",
            ),
            (
                "GetLayoutObjectAttribute ( \"website\" ; \"content\" )",
                "",
            ),
            (
                "GetLayoutObjectOwnerInfo ( GetLayoutObjectAttribute ( \"First Name\" ; \"UUID\" ) )",
                not_found,
            ),
            (
                "GetLayoutObjectOwnerInfo ( Lower ( GetLayoutObjectAttribute ( \"First Name\" ; \"UUID\" ) ) )",
                not_found,
            ),
            ("LayoutObjectUUID", "?#10"),
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"UUID\" )",
                "BD9EB7E8-1237-464D-B0A2-42E058E00EC9",
            ),
            // FileMaker Go's location functions are empty in FileMaker Pro
            // [doc]; a sensor it lists is empty, another name 1208, as on
            // FileMaker Server (s48) [I] (om_data::host::device).
            ("Location ( 10 ; 60 )", ""),
            ("LocationValues ( 10 ; 60 )", ""),
            ("GetSensor ( \"stepCount\" ; 3600 )", ""),
            ("GetSensor ( \"battery\" )", "?#1208"),
        ],
    );
    // Inside an object's own calculation: that object's UUID.
    let schema = schema();
    let source = source(&schema);
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&schema, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    host.layout_object = Some("BD9EB7E8-1237-464D-B0A2-42E058E00EC9".into());
    let inside = evaluate_text(
        "LayoutObjectUUID = GetLayoutObjectAttribute ( \"First Name\" ; \"UUID\" )",
        &mut host,
        &on_record(),
    );
    assert_eq!(inside.code(), 0);
    let cx = Ctx::new(&schema.locale, today());
    assert_eq!(inside.value.to_text(&cx).into_string(), "1");
}

/// Tasks' Task List as a list: a top navigation part, a header, a leading
/// sub-summary Browse mode does not show and a 60-point body; "Go to
/// details." anchored top and right.
fn task_list() -> Layout {
    let part = |id, part_type, top, height| Part {
        id,
        part_type,
        top,
        height,
        ..Part::default()
    };
    Layout {
        id: 2,
        name: "Task List".into(),
        def: Some(LayoutDef {
            base_width: 1024.0,
            table_alias_key: CONTACTS,
            flags: 0x200,
            ..LayoutDef::default()
        }),
        parts: vec![
            part(1, 12, 0.0, 44.0),
            part(2, 1, 44.0, 30.0),
            part(3, 3, 74.0, 30.0),
            part(4, 4, 104.0, 60.0),
        ],
        objects: ObjectStore {
            root: vec![9],
            objects: vec![LayoutObject {
                id: 9,
                object_type: 10,
                flags: 0x6000_4000,
                name: Some("Go to details.".into()),
                bounds: rect(980.0, 104.0, 1024.0, 164.0),
                ..LayoutObject::default()
            }],
            ..ObjectStore::default()
        },
        ..Layout::default()
    }
}

#[test]
fn o5_a_list_shows_the_current_records_row() {
    let mut schema = schema();
    schema.add_layout_entry(LayoutEntry {
        layout: Some(Arc::new(task_list())),
        ..LayoutEntry::new(4, "Task List", Some(CONTACTS))
    });
    let source = source(&schema);
    for (record, bounds) in [(1, "1156 299 1200 359 0"), (2, "1156 359 1200 419 0")] {
        let list = SessionInfo {
            layout_name: "Task List".into(),
            layout_view_state: Some(1),
            found_count: Some(3),
            active_record_number: Some(record),
            ..session()
        };
        let eval = |e: &str| eval_in(&schema, &source, &list, &on_record(), e);
        assert_eq!(
            eval("GetLayoutObjectAttribute ( \"Go to details.\" ; \"bounds\" )"),
            bounds
        );
        for row in ["0 ; 1", "0 ; 2", "0 ; 3", "0 ; 4", "1 ; 2"] {
            assert_eq!(
                eval(&format!(
                    "GetLayoutObjectAttribute ( \"Go to details.\" ; \"bounds\" ; {row} )"
                )),
                "",
                "{row}"
            );
        }
        assert_eq!(eval("Get ( LayoutViewState )"), "1");
    }
    // Table view draws no layout objects.
    let table = SessionInfo {
        layout_name: "Task List".into(),
        layout_view_state: Some(2),
        found_count: Some(3),
        ..session()
    };
    assert_eq!(
        eval_in(
            &schema,
            &source,
            &table,
            &on_record(),
            "GetLayoutObjectAttribute ( \"Go to details.\" ; \"bounds\" )"
        ),
        ""
    );
}

#[test]
fn o6_design_functions_name_other_open_files() {
    // Tasks, opened second, asks about Contacts.
    let mut tasks = CalcSchema::new("Tasks", Locale::en_us());
    tasks.add_table(1, "Tasks");
    tasks.add_occurrence(1, "Tasks", 1);
    tasks.add_field(1, FieldDef::data(1, "Task", ValueType::Text));
    tasks.add_layout_entry(LayoutEntry::new(2, "Task List", Some(1)));
    let contacts = Arc::new(schema());
    let session = SessionInfo {
        file_name: "Tasks".into(),
        window_name: "Tasks".into(),
        layout_name: "Task List".into(),
        other_files: vec![OpenFile {
            schema: Arc::clone(&contacts),
        }],
        open_position: 1,
        windows: vec![
            ("Tasks".into(), "Tasks".into()),
            ("Contacts".into(), "Contacts".into()),
        ],
        ..session()
    };
    let source = MemorySource::new();
    let eval = |e: &str| eval_in(&tasks, &source, &session, &Context::new(), e);
    for (expression, want) in [
        ("DatabaseNames", "Contacts\rTasks"),
        ("WindowNames", "Tasks\rContacts"),
        ("WindowNames ( \"Contacts\" )", "Contacts"),
        ("WindowNames ( \"Tasks\" )", "Tasks"),
        ("TableNames ( \"Contacts\" )", "Contacts\rPhone Numbers"),
        ("TableNames ( \"contacts\" )", "Contacts\rPhone Numbers"),
        (
            "TableNames ( \"CONTACTS.FMP12\" )",
            "Contacts\rPhone Numbers",
        ),
        ("TableNames ( \" Contacts\" )", ""),
        (
            "LayoutNames ( \"Contacts.fmp12\" )",
            "Contact List\rContact Details",
        ),
        (
            "LayoutNames ( \"Contacts\" )",
            "Contact List\rContact Details",
        ),
        ("LayoutIDs ( \"Contacts\" )", "2\r3"),
        ("TableNames ( \"Tasks\" )", "Tasks"),
        ("Get ( FileName )", "Tasks"),
        ("Get ( WindowName )", "Tasks"),
    ] {
        assert_eq!(eval(expression), want, "{expression}");
    }
}

// ----- the shipped templates --------------------------------------------------

fn files_dir() -> PathBuf {
    std::env::var_os("OM_HARNESS_FILES").map_or_else(|| PathBuf::from(FILES), PathBuf::from)
}

/// A template's calculation schema named `name`, and its records.
fn open(path: &Path, name: &str) -> (CalcSchema, MemorySource) {
    let file = om_hbam::HbamFile::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let tree = file.key_tree().unwrap();
    let catalog = om_hbam::Catalog::from_tree(&tree).unwrap();
    let schema = om_data::rules::schema::calc_schema(&tree, &catalog, name);
    let cx = Ctx::new(&schema.locale, today());
    let mut source = MemorySource::new();
    for table in schema.tables() {
        for record in om_hbam::record::records(&tree, table.id) {
            let mut values = RecordValues::default();
            for def in table.fields.values() {
                if let Some(Ok(text)) = record.text(def.id) {
                    values.set(def.id, 1, stored_value(&text, def.value_type, &cx));
                }
            }
            source.put_record(table.id, record.id, values);
        }
    }
    (schema, source)
}

#[test]
#[ignore = "requires the harness's template files"]
fn o5_the_templates_contact_details_in_screen_points() {
    let (schema, source) = open(&files_dir().join("contacts-en.fmp12"), "Contacts");
    let occurrence = schema
        .layout_named("Contact Details")
        .and_then(|l| l.occurrence)
        .unwrap();
    // The template has no records; the bounds are those FileMaker Pro drew.
    let context = Context::record(occurrence, NO_RECORD);
    for (toolbar, zoom, first, company, website) in [
        (
            true,
            100,
            "132 841 499 885 0",
            "542 431 842 475 0",
            "542 486 1167 989 0",
        ),
        (
            false,
            100,
            "132 789 499 833 0",
            "542 354 842 398 0",
            "542 409 1167 937 0",
        ),
        (
            true,
            150,
            "148 793 585 859 0",
            "649 512 1099 578 0",
            "649 595 1587 1015 0",
        ),
        (
            false,
            150,
            "148 716 585 782 0",
            "649 435 1099 501 0",
            "649 518 1587 938 0",
        ),
    ] {
        let state = SessionInfo {
            window: Some(probe_window(toolbar, zoom)),
            ..session()
        };
        for (expression, want) in [
            (
                "GetLayoutObjectAttribute ( \"First Name\" ; \"bounds\" )",
                first,
            ),
            (
                "GetLayoutObjectAttribute ( \"Company\" ; \"bounds\" )",
                company,
            ),
            (
                "GetLayoutObjectAttribute ( \"website\" ; \"bounds\" )",
                website,
            ),
            (
                "GetLayoutObjectAttribute ( \"Back to Contacts\" ; \"bounds\" )",
                if toolbar {
                    "100 225 300 269 0"
                } else {
                    "100 148 300 192 0"
                },
            ),
        ] {
            assert_eq!(
                eval_in(&schema, &source, &state, &context, expression),
                want,
                "{expression} toolbar {toolbar} zoom {zoom}"
            );
        }
    }
}

#[test]
#[ignore = "requires the harness's template files"]
fn o5_the_templates_task_list_rows() {
    let (schema, source) = open(&files_dir().join("tasks-en.fmp12"), "Tasks");
    let occurrence = schema
        .layout_named("Task List")
        .and_then(|l| l.occurrence)
        .unwrap();
    for (record, bounds) in [(1, "1156 299 1200 359 0"), (2, "1156 359 1200 419 0")] {
        let list = SessionInfo {
            file_name: "Tasks".into(),
            layout_name: "Task List".into(),
            layout_view_state: Some(1),
            found_count: Some(3),
            active_record_number: Some(record),
            ..session()
        };
        assert_eq!(
            eval_in(
                &schema,
                &source,
                &list,
                &Context::record(occurrence, NO_RECORD),
                "GetLayoutObjectAttribute ( \"Go to details.\" ; \"bounds\" )"
            ),
            bounds
        );
    }
}

#[test]
#[ignore = "requires the harness's template files"]
fn o6_the_templates_from_each_other() {
    let (contacts, _) = open(&files_dir().join("contacts-en.fmp12"), "Contacts");
    let (tasks, source) = open(&files_dir().join("tasks-en.fmp12"), "Tasks");
    let (contacts, tasks) = (Arc::new(contacts), Arc::new(tasks));
    let windows = vec![
        ("Tasks".to_owned(), "Tasks".to_owned()),
        ("Contacts".to_owned(), "Contacts".to_owned()),
    ];
    let from_tasks = SessionInfo {
        file_name: "Tasks".into(),
        window_name: "Tasks".into(),
        other_files: vec![OpenFile {
            schema: Arc::clone(&contacts),
        }],
        open_position: 1,
        windows: windows.clone(),
        ..session()
    };
    let eval = |e: &str| eval_in(&tasks, &source, &from_tasks, &Context::new(), e);
    let contacts_tables = "Addresses\rContacts\rEmail Addresses\rPhone Numbers";
    for (expression, want) in [
        ("DatabaseNames", "Contacts\rTasks"),
        ("WindowNames", "Tasks\rContacts"),
        ("WindowNames ( \"Contacts\" )", "Contacts"),
        ("WindowNames ( \"Tasks\" )", "Tasks"),
        ("TableNames ( \"Contacts\" )", contacts_tables),
        ("TableNames ( \"contacts\" )", contacts_tables),
        ("TableNames ( \"CONTACTS.FMP12\" )", contacts_tables),
        ("TableNames ( \" Contacts\" )", ""),
        (
            "LayoutNames ( \"Contacts.fmp12\" )",
            "Contact List\rContact Details",
        ),
        ("LayoutNames ( \"Contacts\" )", "Contact List\rContact Details"),
        (
            "FieldNames ( \"Contacts\" ; \"Contact Details\" )",
            "Website\rCompany\rPhone Numbers::Number\rPhone Numbers::Type\rEmail Addresses::Address\rEmail Addresses::Type\rAddresses::Country\rAddresses::Postal Code\rAddresses::State\rAddresses::City\rAddresses::Address Line 2\rAddresses::Address Line 1\rAddresses::Type\rJob Title\rLast Name\rFirst Name\rTitle\rPhoto\rPhoto",
        ),
        ("LayoutIDs ( \"Contacts\" )", "2\r3"),
        ("TableIDs ( \"Contacts\" )", "1065091\r1065090\r1065093\r1065092"),
    ] {
        assert_eq!(eval(expression), want, "{expression}");
    }
    let from_contacts = SessionInfo {
        other_files: vec![OpenFile {
            schema: Arc::clone(&tasks),
        }],
        open_position: 0,
        windows: windows.into_iter().rev().collect(),
        ..session()
    };
    let eval = |e: &str| eval_in(&contacts, &source, &from_contacts, &Context::new(), e);
    for (expression, want) in [
        ("DatabaseNames", "Contacts\rTasks"),
        ("WindowNames", "Contacts\rTasks"),
        ("WindowNames ( \"Tasks\" )", "Tasks"),
        (
            "TableNames ( \"Tasks\" )",
            "Assignees\rAssignments\rAttachments\rTasks",
        ),
        (
            "LayoutNames ( \"Tasks\" )",
            "Task List\rTask Details\rAssignee List\rAssignee Details\r-\rAssignee Chooser\rAdd Assignee\rAssignments",
        ),
        (
            "FieldNames ( \"Tasks\" ; \"Task List\" )",
            "Due Date\rCategory\rStatus\rTask",
        ),
    ] {
        assert_eq!(eval(expression), want, "{expression}");
    }
}

#[test]
fn unknown_access_and_elevated_extended_metadata_are_not_false_denials_or_account_keywords() {
    let schema = schema();
    let source = source(&schema);
    let context = Context::record(CONTACTS, RECORD);
    let mut info = session();
    info.privileges.as_mut().unwrap().layout_access = None;
    info.privileges.as_mut().unwrap().record_access = None;
    assert_eq!(
        eval_in(
            &schema,
            &source,
            &info,
            &context,
            "Get ( LayoutAccess ) & \"|\" & Get ( RecordAccess )"
        ),
        "|"
    );
    // The context's own answers (the account's set on this layout and
    // record, session-identity.md 2.4) win over the set's.
    info.layout_access_here = Some(1);
    info.record_access_here = Some(0);
    assert_eq!(
        eval_in(
            &schema,
            &source,
            &info,
            &context,
            "Get ( LayoutAccess ) & \"|\" & Get ( RecordAccess )"
        ),
        "1|0"
    );
    info.privilege_set_name = "Elevated".into();
    assert_eq!(
        eval_in(
            &schema,
            &source,
            &info,
            &context,
            "Get ( CurrentExtendedPrivileges ) & \"|\" & Get ( AccountExtendedPrivileges )"
        ),
        "|fmextscriptaccess\rfmreauthenticate10"
    );
    info.current_privileges = Some(Privileges {
        account_set_name: "Elevated".into(),
        extended: vec!["effective-only".into()],
        layout_access: Some(2),
        record_access: Some(2),
    });
    assert_eq!(
        eval_in(
            &schema,
            &source,
            &info,
            &context,
            "Get ( CurrentExtendedPrivileges ) & \"|\" & Get ( AccountExtendedPrivileges )"
        ),
        "effective-only|fmextscriptaccess\rfmreauthenticate10"
    );
}

#[test]
fn authenticated_password_deadline_counts_calendar_days_and_unknown_stays_unknown() {
    use om_data::host::session::selector::{ACCOUNT_PASSWORD_DAYS_REMAINING, ACCOUNT_TYPE};
    use om_data::host::AccountInfo;
    let date = Date::from_ymd(2026, 12, 31).unwrap();
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, date);
    let info = AccountInfo {
        account_type: Some("FileMaker File".into()),
        password_expiry: Some(date.day_number() + 2),
        set_access: None,
    };
    for (day, expected) in [
        (date, "2"),
        (Date::from_ymd(2027, 1, 2).unwrap(), "0"),
        (Date::from_ymd(2027, 1, 3).unwrap(), "0"),
    ] {
        assert_eq!(
            info.answer(ACCOUNT_PASSWORD_DAYS_REMAINING, day)
                .unwrap()
                .to_text(&cx)
                .as_str(),
            expected
        );
    }
    let unlimited = AccountInfo {
        password_expiry: Some(-1),
        ..info
    };
    assert_eq!(
        unlimited
            .answer(ACCOUNT_PASSWORD_DAYS_REMAINING, date)
            .unwrap()
            .to_text(&cx)
            .as_str(),
        "-1"
    );
    let unknown = AccountInfo::default();
    assert!(unknown.unknown(ACCOUNT_TYPE));
    assert!(unknown.unknown(ACCOUNT_PASSWORD_DAYS_REMAINING));
    assert_eq!(
        unknown
            .answer(ACCOUNT_PASSWORD_DAYS_REMAINING, date)
            .unwrap()
            .to_text(&cx)
            .as_str(),
        ""
    );
}
