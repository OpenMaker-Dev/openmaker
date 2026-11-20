use super::*;
use crate::key::StoredKey;
use crate::step::{flag, StepData, VARIABLE_NAME_SLOT};
use crate::{Calculation, FieldSpec, ParamValue, Script, ScriptStep};
use om_formula::{Context, NameTable, Names};
use om_hbam::OwnedNode;
use std::collections::HashMap;

/// The Contacts names of the reference captures: occurrence `Contacts`
/// (0xd00002) on table 0x8002 (base table `Contacts`), layout 3 `Contact
/// Details`, script 2 `Go to Entry Field`.
struct Contacts {
    names: NameTable,
    layouts: HashMap<u64, (&'static str, &'static str)>,
    scripts: HashMap<u64, &'static str>,
}

const CONTACTS: u64 = 0xd00002;

impl Contacts {
    fn new() -> Self {
        let mut names = NameTable::default();
        names.add_occurrence(CONTACTS, "Contacts", 0x8002);
        names.add_field(0x8002, 1, "First Name");
        names.add_field(0x8002, 2, "Last Name");
        Self {
            names,
            layouts: HashMap::from([(3, ("Contact Details", "Contacts"))]),
            scripts: HashMap::from([(2, "Go to Entry Field"), (3, "Import from file: CSV")]),
        }
    }
}

impl StepNames for Contacts {
    fn calc_names(&self) -> &dyn Names {
        &self.names
    }
    fn layout_name(&self, id: u64) -> Option<&str> {
        self.layouts.get(&id).map(|l| l.0)
    }
    fn layout_occurrence_name(&self, id: u64) -> Option<&str> {
        self.layouts.get(&id).map(|l| l.1)
    }
    fn script_name(&self, id: u64) -> Option<&str> {
        self.scripts.get(&id).copied()
    }
    fn find_layout(&self, name: &str, occurrence: Option<&str>) -> Lookup {
        Lookup::by_name(
            self.layouts
                .iter()
                .filter(|(_, l)| occurrence.is_none_or(|o| o == l.1))
                .map(|(id, l)| (*id, l.0)),
            name,
        )
    }
    fn find_script(&self, name: &str) -> Lookup {
        Lookup::by_name(self.scripts.iter().map(|(id, s)| (*id, *s)), name)
    }
    fn table_name(&self, id: u64) -> Option<&str> {
        (id == 0x8002).then_some("Contacts")
    }
    fn find_table(&self, name: &str) -> Lookup {
        Lookup::by_name([(0x8002, "Contacts")], name)
    }
}

/// A step calculation compiled from source as FileMaker stores it.
fn calc(names: &Contacts, source: &str) -> Calculation {
    let compiled = om_formula::compile(source, &Context::new(&names.names)).unwrap();
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

fn shown(step: &ScriptStep) -> String {
    step_line(step, &Contacts::new()).text
}

/// A field spec as FileMaker mostly stores one: zero filler.
fn field(id: u64) -> FieldSpec {
    FieldSpec::new(
        StoredKey([4, 3, 0xd0, 0, 2, 0]),
        StoredKey([2, 1, id as u8, 0, 0, 0]),
        1,
    )
}

#[test]
fn the_reference_script_shows_as_in_the_capture() {
    // 35-script-workspace.png: Go to Entry Field.
    let names = Contacts::new();
    let mut go = ScriptStep::new(145);
    go.set_calculation(0, Some(&calc(&names, "Get ( ScriptParameter )")));
    let steps = [go, ScriptStep::new(50)];
    assert_eq!(
        script_text(&steps, &names),
        "Go to Object [ Object Name: Get ( ScriptParameter ) ]\nSelect All"
    );
    let line = step_line(&steps[0], &names);
    let option = line.option_at(line.text.find("Get").unwrap()).unwrap();
    assert_eq!(
        &line.text[option.range.clone()],
        "Object Name: Get ( ScriptParameter )"
    );
    // The schema's "Object name" parameter.
    assert_eq!(option.parameter, Some(0));
    assert_eq!(line.option_at(2), None);
}

#[test]
fn options_follow_filemakers_labels_and_order() {
    let names = Contacts::new();
    let mut set = ScriptStep::new(141);
    set.set_text(0, Some("$count"));
    set.set_calculation(1, Some(&calc(&names, "$count + 1")));
    assert_eq!(shown(&set), "Set Variable [ $count ; Value: $count + 1 ]");
    set.set_calculation(2, Some(&calc(&names, "2")));
    assert_eq!(
        shown(&set),
        "Set Variable [ $count[2] ; Value: $count + 1 ]"
    );

    let mut field_step = ScriptStep::new(76);
    field_step.set_field_spec(0, Some(field(1)));
    field_step.set_calculation(0, Some(&calc(&names, "\"Ann\"")));
    assert_eq!(
        shown(&field_step),
        "Set Field [ Contacts::First Name ; \"Ann\" ]"
    );

    let mut commit = ScriptStep::new(75);
    commit.set_flag(flag::NO_INTERACT, true);
    assert_eq!(
        shown(&commit),
        "Commit Records/Requests [ With dialog: Off ]"
    );

    let mut next = ScriptStep::new(16);
    next.set_parameter("Record/Request/Page", ParamValue::Choice(4))
        .unwrap();
    next.set_flag(flag::EXIT, true);
    assert_eq!(
        shown(&next),
        "Go to Record/Request/Page [ Next ; Exit after last: On ]"
    );
    let mut first = ScriptStep::new(16);
    first.value = 1;
    assert_eq!(shown(&first), "Go to Record/Request/Page [ First ]");

    let mut exit = ScriptStep::new(103);
    assert_eq!(shown(&exit), "Exit Script [ Text Result: ]");
    exit.set_calculation(0, Some(&calc(&names, "$result")));
    assert_eq!(shown(&exit), "Exit Script [ Text Result: $result ]");

    let mut capture = ScriptStep::new(86);
    capture.set_flag(flag::TURN_ON | flag::SET_ON_OFF, true);
    assert_eq!(shown(&capture), "Set Error Capture [ On ]");
    assert_eq!(shown(&ScriptStep::new(85)), "Allow User Abort [ Off ]");
    assert_eq!(shown(&ScriptStep::new(71)), "Loop [ Flush: Always ]");
    assert_eq!(shown(&ScriptStep::new(28)), "Perform Find [ ]");
    assert_eq!(shown(&ScriptStep::new(7)), "New Record/Request");
    assert_eq!(shown(&ScriptStep::new(69)), "Else");

    let mut url = ScriptStep::new(160);
    url.set_flag(flag::SELECT_ALL | flag::NO_INTERACT, true);
    url.set_flag(flag::VERIFY_SSL_CERTIFICATES, true);
    url.set_variable_target(0, "$result");
    url.set_calculation(0, Some(&calc(&names, "\"https://example.com\"")));
    assert_eq!(
        shown(&url),
        "Insert from URL [ Select ; With dialog: Off ; Target: $result ; \
         \"https://example.com\" ; Verify SSL Certificates ]"
    );
}

#[test]
fn calculation_options_keep_the_specific_slot_within_a_parameter() {
    let names = Contacts::new();
    for id in [119, 122] {
        let mut step = ScriptStep::new(id);
        for (slot, value) in [(1, "400"), (2, "600"), (3, "20"), (4, "30")] {
            step.set_calculation(slot, Some(&calc(&names, value)));
        }
        let parameter = step
            .schema()
            .unwrap()
            .params
            .iter()
            .position(|p| p.name == "Bounds")
            .unwrap();
        for enabled in [true, false] {
            step.set_enabled(enabled);
            let line = step_line(&step, &names);
            for (label, slot) in [("Height:", 1), ("Width:", 2), ("Top:", 3), ("Left:", 4)] {
                let at = line.text.find(label).unwrap();
                let option = line.option_at(at).unwrap();
                assert_eq!(option.parameter, Some(parameter));
                assert_eq!(option.calculation_slot, Some(slot));
            }
        }
    }
    let mut step = ScriptStep::new(211);
    for (slot, source) in [(0, "\"flow\""), (1, "\"url\""), (2, "\"json\"")] {
        step.set_calculation(slot, Some(&calc(&names, source)));
    }
    let line = step_line(&step, &names);
    for (label, slot) in [("Flow:", 0), ("URL:", 1), ("JSON Data:", 2)] {
        let option = line.option_at(line.text.find(label).unwrap()).unwrap();
        assert_eq!(option.calculation_slot, Some(slot));
    }
    // A fixed option is distinct from an empty calculation option.
    let line = step_line(&ScriptStep::new(119), &names);
    let current = line
        .option_at(line.text.find("Current Window").unwrap())
        .unwrap();
    assert_eq!(current.calculation_slot, None);
    let line = step_line(&ScriptStep::new(103), &names);
    assert_eq!(line.options[0].calculation_slot, Some(0));
}

#[test]
fn references_show_by_name_in_typographic_quotes() {
    let names = Contacts::new();
    let mut layout = ScriptStep::new(6);
    layout.set_member_key(StoredKey::from_id(3));
    layout.value = 5;
    assert_eq!(
        shown(&layout),
        "Go to Layout [ “Contact Details” (Contacts) ]"
    );
    layout.set_list_value(6, Some(0));
    assert_eq!(
        shown(&layout),
        "Go to Layout [ “Contact Details” (Contacts) ; Animation: None ]"
    );
    layout.set_member_key(StoredKey::from_id(9));
    assert_eq!(
        shown(&layout),
        "Go to Layout [ <unknown> ; Animation: None ]"
    );
    let mut original = ScriptStep::new(6);
    original.value = 1;
    assert_eq!(shown(&original), "Go to Layout [ original layout ]");

    let mut perform = ScriptStep::new(1);
    perform.set_script_key(StoredKey::from_serial_id(2));
    assert_eq!(
        shown(&perform),
        "Perform Script [ Specified: From list ; “Go to Entry Field” ; Parameter: ]"
    );
    perform.set_calculation(0, Some(&calc(&names, "Contacts::Last Name")));
    assert_eq!(
        shown(&perform),
        "Perform Script [ Specified: From list ; “Go to Entry Field” ; \
         Parameter: Contacts::Last Name ]"
    );
    // By name: [00 1] = 1, the name calculation in slot 2.
    let mut by_name = ScriptStep::new(1);
    by_name.set_list_value(1, Some(1));
    by_name.set_calculation(2, Some(&calc(&names, "\"Button\" & 1")));
    assert_eq!(
        shown(&by_name),
        "Perform Script [ Specified: By name ; \"Button\" & 1 ; Parameter: ]"
    );
}

#[test]
fn blocks_indent_and_disabled_steps_and_comments_show_their_prefix() {
    let names = Contacts::new();
    let mut script = Script::default();
    let mut test = ScriptStep::new(68);
    test.set_calculation(0, Some(&calc(&names, "Get ( FoundCount ) = 0")));
    let mut note = ScriptStep::new(89);
    note.set_text(0, Some("Nothing found"));
    script
        .insert_steps(
            0,
            [
                test,
                note,
                ScriptStep::new(71),
                ScriptStep::new(7),
                ScriptStep::new(73),
                ScriptStep::new(69),
                ScriptStep::new(89),
                ScriptStep::new(93),
                ScriptStep::new(70),
            ],
        )
        .unwrap();
    script.set_step_enabled(7, false).unwrap();
    assert_eq!(
        script_text(&script.steps, &names),
        "If [ Get ( FoundCount ) = 0 ]\n    # Nothing found\n    Loop [ Flush: Always ]\n        \
         New Record/Request\n    End Loop\nElse\n    \n    // Beep\nEnd If"
    );
    let lines = script_lines(&script.steps, &names);
    assert_eq!(lines[1].kind, LineKind::Comment);
    assert_eq!(lines[6].text, "");
    assert!(!lines[7].enabled);
}

#[test]
fn a_formula_without_its_text_is_rebuilt_from_its_polish_stream() {
    let names = Contacts::new();
    let full = calc(
        &names,
        "If ( Contacts::First Name = \"a\" ; Get ( ScriptParameter ) ; -2 * (1 + 3) )",
    );
    let stripped = Calculation::new(None, full.polish.clone().unwrap());
    assert_eq!(
        calculation_text(&stripped, &names),
        "If ( Contacts::First Name = \"a\" ; Get ( ScriptParameter ) ; -2 * ( 1 + 3 ) )"
    );
    let concat = calc(&names, "Let ( [ x = 1 ; $y = 2 ] ; x & \"¶\" )");
    let stripped = Calculation::new(None, concat.polish.unwrap());
    assert_eq!(
        calculation_text(&stripped, &names),
        "Let ( [ _v0 = 1 ; $y = 2 ] ; _v0 & \"¶\" )"
    );
}

#[test]
fn every_named_step_renders_from_its_name() {
    let names = Contacts::new();
    for id in crate::step_ids().filter(|id| crate::step_name(*id).is_some()) {
        let step = ScriptStep::new(id);
        let line = step_line(&step, &names);
        if id == 89 {
            assert_eq!(line.text, "");
            continue;
        }
        assert!(line.text.starts_with(&step.name()), "{id}: {}", line.text);
        for option in &line.options {
            assert!(line.text.is_char_boundary(option.range.start));
            assert!(line.text.is_char_boundary(option.range.end));
        }
        let mut disabled = step.clone();
        disabled.set_enabled(false);
        assert_eq!(
            step_line(&disabled, &names).text,
            format!("// {}", line.text)
        );
    }
}

#[test]
fn file_paths_and_data_sources_show_quoted() {
    let names = Contacts::new();
    let mut create = ScriptStep::new(190);
    create.set_slot_child(
        0,
        StepData::PATH_STRING,
        Some(crate::text_node(&[StepData::PATH_STRING], "$file")),
    );
    assert_eq!(
        step_line(&create, &names).text,
        "Create Data File [ “$file” ; Create folders: Off ]"
    );
    let mut close = ScriptStep::new(34);
    assert_eq!(shown(&close), "Close File [ Current File ]");
    close.set_data_leaf(
        StepData::FILE_REFERENCES,
        Some(OwnedNode::leaf([0x20], [1, 2])),
    );
    assert_eq!(shown(&close), "Close File [ <unknown> ]");
}

/// Send Mail through the email client shows as FileMaker Pro 26.0.3 shows
/// it (#836; Pro's reading on #221): in the workspace its attachment by
/// file name, and no "Collect addresses". The text form keeps the stored
/// path and what the line does not show, and reads back. A step with an
/// option that reading did not show (a CC here) keeps the generic text.
#[test]
fn send_mail_shows_its_message_as_filemaker_does() {
    let names = Contacts::new();
    let mut mail = ScriptStep::new(63);
    mail.flags |= flag::NO_INTERACT | flag::FILE_PATH;
    mail.set_calculation(0, Some(&calc(&names, "\"pat@example.com\"")));
    mail.set_calculation(3, Some(&calc(&names, "\"Weekly report\"")));
    mail.set_calculation(4, Some(&calc(&names, "\"See attached\"")));
    // As the step's options write the attachment list.
    mail.set_path_list("file:report.pdf").unwrap();
    assert_eq!(
        step_line(&mail, &names).text,
        "Send Mail [ Send via E-mail Client ; With dialog: Off ; To: \"pat@example.com\" ; \
         Subject: \"Weekly report\" ; Message: \"See attached\" ; “report.pdf” ]"
    );
    let text = step_text(&mail, &names);
    assert!(text.ends_with("; “file:report.pdf” ]"), "{text}");
    assert_eq!(canonical(&pasted(&text)), canonical(&mail), "{text}");
    // "Collect addresses" is not in the line; the text form keeps it.
    let mut collected = mail.clone();
    collected.set_number_bool(0, Some(true));
    assert_eq!(
        step_line(&collected, &names).text,
        step_line(&mail, &names).text
    );
    let text = step_text(&collected, &names);
    assert_eq!(canonical(&pasted(&text)), canonical(&collected), "{text}");

    mail.set_calculation(1, Some(&calc(&names, "\"lee@example.com\"")));
    assert_eq!(
        step_line(&mail, &names).text,
        "Send Mail [ With dialog: Off ]"
    );
    // So does Collect addresses for a CC that is not set, and an attachment
    // whose name has a typographic quote.
    mail.set_calculation(1, None);
    mail.set_number_bool(1, Some(true));
    assert_eq!(
        step_line(&mail, &names).text,
        "Send Mail [ With dialog: Off ]"
    );
    mail.set_number_bool(1, None);
    mail.set_path_list("file:report”[.pdf").unwrap();
    assert_eq!(
        step_line(&mail, &names).text,
        "Send Mail [ With dialog: Off ]"
    );
    let text = step_text(&mail, &names);
    assert_eq!(canonical(&pasted(&text)), canonical(&mail), "{text}");
}

#[test]
fn psos_canonical_names_have_a_real_calculation_span() {
    let names = Contacts::new();
    let mut step = ScriptStep::new(164);
    step.set_list_value(1, Some(1));
    step.set_calculation(0, Some(&calc(&names, "41")));
    step.set_calculation(2, Some(&calc(&names, "\"Target\"")));
    let line = step_line(&step, &names);
    assert!(
        line.text
            .contains("Specified: By name ; \"Target\" ; Parameter: 41"),
        "{}",
        line.text
    );
    let name = line
        .options
        .iter()
        .find(|span| span.calculation_slot == Some(2))
        .unwrap();
    assert_eq!(&line.text[name.range.clone()], "\"Target\"");
    let selector = line
        .options
        .iter()
        .find(|span| line.text[span.range.clone()].contains("Specified: By name"))
        .unwrap();
    assert!(selector.calculation_slot.is_none());
    let schema = step.schema().unwrap();
    assert_eq!(
        schema.params[name.parameter.unwrap()].name,
        "Specify by name"
    );
}

#[test]
fn psos_callback_reads_and_writes_the_slots_filemaker_stores() {
    // The slots of #188's two FileMaker Pro 26.0.3 variants: the callback
    // from the list (script 2 as the literal "02" in slot 3, `kOption`)
    // and by name (`[00 6]` = 1, slot 7), which keeps that slot 3.
    let names = Contacts::new();
    let mut step = ScriptStep::new(210);
    step.set_list_value(1, Some(1));
    step.flags |= flag::STORE_REFERENCE | flag::RESTORE;
    step.set_calculation(2, Some(&calc(&names, "\"Server\"")));
    step.set_calculation(0, Some(&calc(&names, "\"ServerMarker\"")));
    step.set_calculation(3, Some(&calc(&names, "\"02\"")));
    step.set_calculation(4, Some(&calc(&names, "\"CallbackMarker\"")));
    step.flags |= flag::OPTION | flag::OPTION2 | flag::MENU_SELECTION;
    step.value = 3;
    let text = step_text(&step, &names);
    assert_eq!(
        text,
        "Perform Script on Server with Callback [ Specified: By name ; \"Server\" ; \
         Parameter: \"ServerMarker\" ; Callback script specified: From list ; \
         “Go to Entry Field” ; Callback script parameter: \"CallbackMarker\" ; \
         State: Resume ]"
    );
    assert_eq!(canonical(&pasted(&text)), canonical(&step), "{text}");

    step.set_list_value(6, Some(1));
    step.set_calculation(7, Some(&calc(&names, "\"Other\"")));
    step.flags |= flag::SELECT_ALL | flag::EXIT;
    let text = step_text(&step, &names);
    assert!(
        text.contains(
            "Callback script specified: By name ; \"Other\" ; \
             Callback script parameter: \"CallbackMarker\" ;"
        ),
        "{text}"
    );
    // The stale slot 3 is not shown; the text keeps it as stored.
    assert_eq!(canonical(&pasted(&text)), canonical(&step), "{text}");

    // A script of this file whose name holds " from file: " is not
    // another file's.
    let mut local = ScriptStep::new(210);
    local.set_script_key(StoredKey::from_serial_id(2));
    local.set_calculation(3, Some(&calc(&names, "\"03\"")));
    local.flags |= flag::OPTION;
    let text = step_text(&local, &names);
    assert!(
        text.contains("From list ; “Import from file: CSV”"),
        "{text}"
    );
    assert_eq!(canonical(&pasted(&text)), canonical(&local), "{text}");
}

#[test]
fn steps_read_back_from_their_text_as_one_undoable_list() {
    let names = Contacts::new();
    let mut go = ScriptStep::new(145);
    go.set_calculation(0, Some(&calc(&names, "Get ( ScriptParameter )")));
    let mut test = ScriptStep::new(68);
    test.set_calculation(
        0,
        Some(&calc(&names, "Contacts::First Name = \"a\" // why")),
    );
    let mut note = ScriptStep::new(89);
    note.set_text(0, Some("two\rlines ¶"));
    let mut set = ScriptStep::new(76);
    set.set_field_spec(0, Some(field(2)));
    set.set_calculation(0, Some(&calc(&names, "Upper ( Contacts::First Name )")));
    let mut beep = ScriptStep::new(93);
    beep.set_enabled(false);
    let mut script = Script::default();
    script
        .insert_steps(0, [go, test, note, set, ScriptStep::new(70), beep])
        .unwrap();
    let text = steps_text(&script.steps, &names);
    assert_eq!(
        text,
        "Go to Object [ Object Name: Get ( ScriptParameter ) ]\n\
         If [ Contacts::First Name = \"a\" // why\n ]\n\
         \t# two¶lines \\¶\n\
         \tSet Field [ Contacts::Last Name ; Upper ( Contacts::First Name ) ]\n\
         End If\n\
         // Beep\n"
    );
    let pasted = parse_steps(&text, &names, Origin::Unknown).unwrap();
    assert_eq!(pasted.len(), script.steps.len());
    for (pasted, step) in pasted.iter().zip(&script.steps) {
        let Pasted::Step(pasted) = pasted else {
            panic!("{pasted:?}");
        };
        assert_eq!(canonical(pasted), canonical(step), "{}", step.name());
    }
    // Every step but the comment comes back from its display text alone.
    assert_eq!(
        step_text_fidelity(&script.steps[0], &names).1,
        Fidelity::Display
    );
}

#[test]
fn only_the_text_form_escapes_returns_and_typographic_quotes() {
    let names = Contacts::new();
    let mut note = ScriptStep::new(89);
    note.set_text(0, Some("two\rlines ¶"));
    let mut insert = ScriptStep::new(61);
    insert.set_text(0, Some("say “hi”"));
    // The workspace shows both as stored (FileMaker's display of them was
    // not observed); the text form escapes them so that they read back.
    assert_eq!(step_line(&note, &names).text, "# two\rlines ¶");
    assert_eq!(step_text(&note, &names), "# two¶lines \\¶");
    assert!(step_line(&insert, &names).text.ends_with("\"say “hi”\" ]"));
    assert!(step_text(&insert, &names).ends_with("\"say \\“hi\\”\" ]"));
    for step in [note, insert] {
        let text = step_text(&step, &names);
        let Pasted::Step(back) = parse_steps(&text, &names, Origin::Unknown)
            .unwrap()
            .remove(0)
        else {
            panic!("{text}");
        };
        assert_eq!(canonical(&back), canonical(&step), "{text}");
    }
}

/// A pasted line's one step.
fn pasted(line: &str) -> ScriptStep {
    match parse_steps(line, &Contacts::new(), Origin::Unknown)
        .unwrap()
        .remove(0)
    {
        Pasted::Step(step) => step,
        failed => panic!("{failed:?}"),
    }
}

#[test]
fn pasted_text_stores_what_filemaker_shows() {
    // The forms FileMaker's Save as XML shows (script-steps.md, "FileMaker
    // XML on paste"); each line reads back as it was written.
    let lines = [
        "Insert Text [ Select ; Target: $t[2] ; \"x\" ]",
        "Insert Text [ Select ; Target: $t[$i] ; \"x\" ]",
        "Perform Script [ Specified: By name ; \"Go to Entry Field\" ; Parameter: 1 ]",
        "Perform Script on Server [ Specified: By name ; \"Go to Entry Field\" ; Parameter: 1 ; Wait for completion: On ]",
        "Select Window [ Name: \"w\" ; Current file ]",
        "Close Window [ Name: \"w\" ]",
        "New Window [ Style: Document ; Name: \"w\" ; Using layout: “Contact Details” (Contacts) ]",
        "Truncate Table [ With dialog: On ; Table: “Contacts” ]",
        "Import Records [ With dialog: On ; Source: “file:a.csv” ]",
        "New Window [ Style: Card ; Name: \"c\" ]",
        "Go to List of Records [ List of record IDs: \"1¶2\" ; Using layout: “Contact Details” (Contacts) ; New window ]",
    ];
    for line in lines {
        assert_eq!(shown(&pasted(line)), line);
    }
    // Go to List of Records: the ids in slot 7, a new window under
    // `kTurnOn` (script-steps.md 12.2).
    let list = pasted(lines[10]);
    assert!(list.calculation(7).is_some() && list.calculation(0).is_none());
    assert!(list.has_flag(flag::TURN_ON) && !list.has_flag(flag::OPTION));
    // Insert Text's text is slot 0.
    let text = pasted(lines[0]);
    assert_eq!((text.text(0).as_deref(), text.text(1)), (Some("x"), None));
    // A number is the variable's spec repetition, a calculation goes with
    // spec repetition 0.
    let number = text.field_spec().unwrap();
    assert!(number.is_variable());
    assert_eq!(number.repetition(), 2);
    assert_eq!(text.calculation(VARIABLE_NAME_SLOT), None);
    let calculated = pasted(lines[1]);
    assert_eq!(calculated.field_spec().unwrap().repetition(), 0);
    assert!(calculated.calculation(VARIABLE_NAME_SLOT).is_some());
    // By name: `[00 1]` = 1, the name in slot 2 and 0x800 | 0x2000000.
    for line in &lines[2..4] {
        let step = pasted(line);
        assert_eq!(step.list_value(1), Some(1), "{line}");
        assert!(step.calculation(2).is_some(), "{line}");
        assert_eq!(step.calculation(1), None, "{line}");
        assert_eq!(
            step.flags & (flag::STORE_REFERENCE | flag::RESTORE),
            flag::STORE_REFERENCE | flag::RESTORE,
            "{line}"
        );
    }
    // The step value that shows a window's name, a new window's layout,
    // the table and the import source.
    for (line, value, menu) in [
        (lines[4], 1, true),
        (lines[5], 1, true),
        (lines[6], 5, false),
        (lines[7], 1, true),
        (lines[8], 1, true),
    ] {
        let step = pasted(line);
        assert_eq!(step.value, value, "{line}");
        assert_eq!(step.flags & flag::MENU_SELECTION != 0, menu, "{line}");
    }
    // Save Records as Excel's records are slot 1, two bytes, not the step
    // value (script-steps.md 11.7); they read back from their text.
    let mut excel = ScriptStep::new(143);
    excel.set_flag(flag::RESTORE, true);
    excel.set_number_u16(1, Some(2));
    let text = shown(&excel);
    assert!(text.contains("; Current record"), "{text}");
    let back = pasted(&text);
    assert_eq!((back.value, back.number(1)), (0, Some(2)), "{text}");
    assert_eq!(back.data_node(), excel.data_node(), "{text}");
    // The current window and table store no value.
    assert_eq!(pasted("Close Window [ Current Window ]").value, 0);
    assert_eq!(
        pasted("Truncate Table [ With dialog: On ; Table: <Current Table> ]").value,
        0
    );
    // A by-name script stored in slot 1 still shows by name.
    let mut legacy = ScriptStep::new(164);
    legacy.set_calculation(1, Some(&calc(&Contacts::new(), "\"Go to Entry Field\"")));
    assert!(
        shown(&legacy)
            .starts_with("Perform Script on Server [ Specified: By name ; \"Go to Entry Field\""),
        "{}",
        shown(&legacy)
    );
}

#[test]
fn lines_that_do_not_read_are_kept_as_disabled_comments() {
    let names = Contacts::new();
    let pasted = parse_steps(
        "Set Field [ Contacts::Nickname ; 1 ]\nNot a step\nGo to Layout [ “Contact Details” (Contacts) ]",
        &names,
        Origin::Unknown,
    )
    .unwrap();
    assert!(matches!(&pasted[0], Pasted::Failed { reason, .. } if reason.contains("Nickname")));
    assert!(matches!(&pasted[1], Pasted::Failed { text, .. } if text == "Not a step"));
    let Pasted::Step(layout) = &pasted[2] else {
        panic!("{:?}", pasted[2]);
    };
    assert_eq!(layout.member_key().and_then(|k| k.id()), Some(3));
    // A step OpenMaker cannot yet edit or run is not made from text or
    // XML (#338).
    for text in [
        "Set Variable by Name",
        r#"<fmxmlsnippet type="FMObjectList"><Step id="249" name="Replace Field Contents by Name"/></fmxmlsnippet>"#,
    ] {
        let pasted = parse_steps(text, &names, Origin::Unknown).unwrap();
        assert!(
            matches!(&pasted[..], [Pasted::Failed { reason, .. }] if reason.contains("cannot make")),
            "{pasted:?}"
        );
    }
    // Its text as OpenMaker writes it, stored bytes included, reads back.
    let mut stored = ScriptStep::new(250);
    stored.set_calculation(3, Some(&calc(&names, "\"$x\"")));
    let text = step_text(&stored, &names);
    for origin in [Origin::ThisFile, Origin::Unknown] {
        let pasted = parse_steps(&text, &names, origin).unwrap();
        assert!(
            matches!(&pasted[..], [Pasted::Step(step)] if step.id() == 250),
            "{text}: {pasted:?}"
        );
    }
    let comment = failed_comment("Not a step");
    assert!(!comment.is_enabled());
    assert_eq!(comment.text(0).as_deref(), Some("Not a step"));
    // Stored options carry what the display leaves out: from this file,
    // stray reference bytes no flag uses too; from elsewhere they are
    // dropped.
    let mut stale = ScriptStep::new(17);
    stale.reserved = [4, 3, 0xd0, 0, 2, 0, 2, 1, 3, 0, 0, 0, 0, 1];
    let text = step_text(&stale, &names);
    assert!(
        text.contains("Stored reference: 0403d0000200020103000000000"),
        "{text}"
    );
    let Pasted::Step(back) = &parse_steps(&text, &names, Origin::ThisFile).unwrap()[0] else {
        panic!();
    };
    assert_eq!(canonical(back), canonical(&stale));
    let Pasted::Step(back) = &parse_steps(&text, &names, Origin::Unknown).unwrap()[0] else {
        panic!();
    };
    assert_eq!(back.reserved, [0; 14]);
}

/// Hostile pastes, run in a child process so that exhausting the stack
/// fails this test rather than aborting the test binary.
#[test]
fn hostile_pastes_are_refused_without_exhausting_the_stack() {
    // On the 8 MB stack of the GUI's main thread, where pastes are read.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "display::tests::hostile_pastes", "--ignored"])
        .env("OM_SCRIPTS_HOSTILE_PASTE", "1")
        .env("RUST_MIN_STACK", (8 << 20).to_string())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "run in a child process by hostile_pastes_are_refused_without_exhausting_the_stack"]
fn hostile_pastes() {
    if std::env::var_os("OM_SCRIPTS_HOSTILE_PASTE").is_none() {
        return;
    }
    let names = Contacts::new();
    // A million nested elements (7 MB): refused, nothing deep is built or
    // dropped.
    let n = 1_000_000;
    let nested = format!("{}{}", "<a>".repeat(n), "</a>".repeat(n));
    let refused = parse_steps(&nested, &names, Origin::Unknown).unwrap_err();
    assert!(refused.contains("more than 128 deep"), "{refused}");
    // The same inside a step, and an unclosed nest.
    let n = 300_000;
    let in_step = format!(
        "<fmxmlsnippet><Step id=\"93\">{}</Step></fmxmlsnippet>",
        "<b>".repeat(n)
    );
    assert!(parse_steps(&in_step, &names, Origin::Unknown).is_err());
    // More text, or more steps, than a paste reads.
    let big = "#".repeat(MAX_PASTE_BYTES + 1);
    assert!(parse_steps(&big, &names, Origin::Unknown)
        .unwrap_err()
        .contains("KB of text"));
    let many = "Beep\n".repeat(MAX_PASTE_STEPS + 1);
    assert!(parse_steps(&many, &names, Origin::Unknown)
        .unwrap_err()
        .contains("steps"));
}

/// Deeply nested calculations, run in a child process as above. The paste
/// leaves them to the formula parser's nesting bound (calc-encoding.md).
#[test]
fn deeply_nested_pasted_calculations_fail_their_step() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "display::tests::hostile_calculations",
            "--ignored",
        ])
        .env("OM_SCRIPTS_HOSTILE_PASTE", "1")
        .env("RUST_MIN_STACK", (8 << 20).to_string())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "run in a child process by deeply_nested_pasted_calculations_fail_their_step"]
fn hostile_calculations() {
    if std::env::var_os("OM_SCRIPTS_HOSTILE_PASTE").is_none() {
        return;
    }
    let names = Contacts::new();
    // A calculation of 100,000 nested parentheses fails its step, in the
    // text form and in XML.
    let deep = format!("{}1{}", "(".repeat(100_000), ")".repeat(100_000));
    let line = format!("Set Variable [ $x ; Value: {deep} ]");
    let pasted = parse_steps(&line, &names, Origin::Unknown).unwrap();
    assert!(
        matches!(&pasted[..], [Pasted::Failed { .. }]),
        "{:?}",
        pasted.len()
    );
    let xml = format!(
        "<Step id=\"141\"><Options>16388</Options><ParameterValues><Parameter type=\"Variable\"><value><Calculation position=\"1\"><Calculation><Text><![CDATA[{deep}]]></Text></Calculation></Calculation></value><Name value=\"$x\"></Name></Parameter></ParameterValues></Step>"
    );
    let pasted = parse_steps(&xml, &names, Origin::Unknown).unwrap();
    assert!(
        matches!(&pasted[..], [Pasted::Failed { .. }]),
        "{:?}",
        pasted.len()
    );
}

#[test]
fn pastes_up_to_the_limits_read() {
    let names = Contacts::new();
    let steps = parse_steps(&"Beep\n".repeat(MAX_PASTE_STEPS), &names, Origin::Unknown).unwrap();
    assert_eq!(steps.len(), MAX_PASTE_STEPS);
    // XML as deep as the bound reads (its one step does not map).
    let depth = super::xml::MAX_XML_DEPTH;
    let xml = format!(
        "{}<Step id=\"93\"></Step>{}",
        "<g>".repeat(depth - 1),
        "</g>".repeat(depth - 1)
    );
    assert!(matches!(
        &parse_steps(&xml, &names, Origin::Unknown).unwrap()[..],
        [Pasted::Step(beep)] if beep.id() == 93
    ));
    let deeper = format!("<g>{xml}</g>");
    assert!(parse_steps(&deeper, &names, Origin::Unknown).is_err());
}

/// Contacts with layouts and scripts sharing names: layout 3 Contact
/// Details of Contacts, 4 Contact Details of Archive, 5 and 6 Dup of
/// Contacts; scripts 2 Go to Entry Field, 8 and 9 Twin.
struct Shared(Contacts);

impl Shared {
    fn new() -> Self {
        let mut contacts = Contacts::new();
        contacts.layouts.insert(4, ("Contact Details", "Archive"));
        contacts.layouts.insert(5, ("Dup", "Contacts"));
        contacts.layouts.insert(6, ("Dup", "Contacts"));
        contacts.scripts.insert(8, "Twin");
        contacts.scripts.insert(9, "Twin");
        Self(contacts)
    }
}

impl StepNames for Shared {
    fn calc_names(&self) -> &dyn Names {
        self.0.calc_names()
    }
    fn layout_name(&self, id: u64) -> Option<&str> {
        self.0.layout_name(id)
    }
    fn layout_occurrence_name(&self, id: u64) -> Option<&str> {
        self.0.layout_occurrence_name(id)
    }
    fn script_name(&self, id: u64) -> Option<&str> {
        self.0.script_name(id)
    }
    fn find_layout(&self, name: &str, occurrence: Option<&str>) -> Lookup {
        self.0.find_layout(name, occurrence)
    }
    fn find_script(&self, name: &str) -> Lookup {
        self.0.find_script(name)
    }
}

#[test]
fn shared_names_resolve_by_occurrence_or_fail() {
    let names = Shared::new();
    let read = |line: &str| {
        parse_steps(line, &names, Origin::Unknown)
            .unwrap()
            .remove(0)
    };
    // The formatter's occurrence tells the two Contact Details apart.
    for id in [3, 4] {
        let mut go = ScriptStep::new(6);
        go.value = 5;
        go.set_member_key(StoredKey::from_id(id));
        let line = step_line(&go, &names).text;
        let Pasted::Step(back) = read(&line) else {
            panic!("{line}");
        };
        assert_eq!(back.member_key().and_then(|k| k.id()), Some(id), "{line}");
    }
    assert!(step_line(
        &{
            let mut go = ScriptStep::new(6);
            go.value = 5;
            go.set_member_key(StoredKey::from_id(4));
            go
        },
        &names
    )
    .text
    .contains("“Contact Details” (Archive)"));
    // Without it, or when it does not tell, the step fails.
    for (line, why) in [
        ("Go to Layout [ “Contact Details” ]", "is ambiguous"),
        ("Go to Layout [ “Dup” (Contacts) ]", "is ambiguous"),
        (
            "Go to Layout [ “Contact Details” (Elsewhere) ]",
            "is not in this file",
        ),
        (
            "Perform Script [ Specified: From list ; “Twin” ; Parameter:    ]",
            "is ambiguous",
        ),
    ] {
        match read(line) {
            Pasted::Failed { reason, .. } => assert!(reason.contains(why), "{line}: {reason}"),
            Pasted::Step(step) => panic!("{line} read as {step:?}"),
        }
    }
}

/// Another file: Contacts' names under other ids (occurrence Contacts
/// 0xd00007 on table 0x8009, First Name 5, Last Name 6, layout Contact
/// Details 9, script Go to Entry Field 7), and Contacts' own ids for other
/// objects (occurrence 0xd00002 is People, its field 2 Phone, layout 3
/// Other, script 2 Cleanup). Without `last_name`, it has no Last Name.
fn other_file(last_name: bool) -> Contacts {
    let mut names = NameTable::default();
    names.add_occurrence(0xd00007, "Contacts", 0x8009);
    names.add_field(0x8009, 5, "First Name");
    if last_name {
        names.add_field(0x8009, 6, "Last Name");
    }
    names.add_occurrence(CONTACTS, "People", 0x8002);
    names.add_field(0x8002, 1, "Nick");
    names.add_field(0x8002, 2, "Phone");
    Contacts {
        names,
        layouts: HashMap::from([
            (9, ("Contact Details", "Contacts")),
            (3, ("Other", "People")),
        ]),
        scripts: HashMap::from([(7, "Go to Entry Field"), (2, "Cleanup")]),
    }
}

/// A key with non-zero filler, so the text needs `Stored reference`.
fn with_filler(mut key: StoredKey) -> StoredKey {
    key.0[5] = 0x77;
    key
}

#[test]
fn stored_ids_from_another_file_are_named_or_fail() {
    let a = Contacts::new();
    let b = other_file(true);
    let read = |step: &ScriptStep, names: &Contacts| {
        let text = step_text(step, &a);
        assert!(text.contains("Stored"), "{text}");
        parse_steps(&text, names, Origin::Unknown)
            .unwrap()
            .remove(0)
    };
    // Set Field of Last Name, Go to Layout of Contact Details and Perform
    // Script of Go to Entry Field, each with its key's filler stored: the
    // names choose B's objects, not the objects of A's ids in B.
    let mut set = ScriptStep::new(76);
    let last = FieldSpec::new(
        with_filler(StoredKey([4, 3, 0xd0, 0, 2, 0])),
        StoredKey([2, 1, 2, 0, 0, 0]),
        1,
    );
    set.set_field_spec(0, Some(last));
    set.set_calculation(0, Some(&calc(&a, "\"x\"")));
    let Pasted::Step(pasted) = read(&set, &b) else {
        panic!()
    };
    let spec = pasted.field_spec().unwrap();
    assert_eq!(
        (spec.table().id(), spec.field().id()),
        (Some(0xd00007), Some(6))
    );
    let mut go = ScriptStep::new(6);
    go.value = 5;
    go.set_member_key(Some(with_filler(StoredKey::from_id(3).unwrap())));
    let Pasted::Step(pasted) = read(&go, &b) else {
        panic!()
    };
    assert_eq!(pasted.member_key().and_then(|k| k.id()), Some(9));
    let mut perform = ScriptStep::new(1);
    perform.set_script_key(Some(with_filler(StoredKey::from_serial_id(2).unwrap())));
    let Pasted::Step(pasted) = read(&perform, &b) else {
        panic!()
    };
    assert_eq!(pasted.script_id(), Some(7));
    // A name B lacks is not covered by A's stored bytes.
    match read(&set, &other_file(false)) {
        Pasted::Failed { reason, .. } => assert_eq!(
            reason,
            "The field “Contacts::Last Name” is not in this file"
        ),
        Pasted::Step(step) => panic!("enabled with {:?}", step.field_spec()),
    }
    // From A itself, the same texts give the same steps.
    for step in [&set, &go, &perform] {
        let text = step_text(step, &a);
        let Pasted::Step(back) = parse_steps(&text, &a, Origin::Unknown).unwrap().remove(0) else {
            panic!("{text}")
        };
        assert_eq!(canonical(&back), canonical(step), "{text}");
    }
}

#[test]
fn stored_specs_and_calculations_from_elsewhere() {
    let a = Contacts::new();
    let b = other_file(true);
    // A sort order is stored by ids the text does not name: it fails from
    // elsewhere, even from A itself, and reads from this file's own text.
    let mut sort = ScriptStep::new(39);
    sort.set_flag(flag::RESTORE | flag::NO_INTERACT, true);
    let spec = crate::specs::SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![crate::specs::SortEntry {
            field: crate::specs::SpecField {
                occurrence: Some(CONTACTS),
                field: 2,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: false,
            locale: 0,
        }],
    };
    sort.set_data_leaf(
        StepData::SORT_ORDER,
        Some(crate::bytes_node(&[StepData::SORT_ORDER], &spec.encode())),
    );
    let text = step_text(&sort, &a);
    for names in [&a, &b] {
        match parse_steps(&text, names, Origin::Unknown)
            .unwrap()
            .remove(0)
        {
            Pasted::Failed { reason, .. } => assert!(reason.contains("its sort order"), "{reason}"),
            Pasted::Step(_) => panic!("{text}"),
        }
    }
    let Pasted::Step(back) = parse_steps(&text, &a, Origin::ThisFile).unwrap().remove(0) else {
        panic!()
    };
    assert_eq!(canonical(&back), canonical(&sort));
    // A stored slot whose calculation is its text's but for the ids keeps
    // what the text does not show (a number beside it) and takes B's ids.
    let mut exit = ScriptStep::new(103);
    exit.set_calculation(0, Some(&calc(&a, "Contacts::Last Name")));
    exit.set_number(0, Some(7));
    let text = step_text(&exit, &a);
    assert!(text.contains("Stored slot 0"), "{text}");
    let Pasted::Step(pasted) = parse_steps(&text, &b, Origin::Unknown).unwrap().remove(0) else {
        panic!("{text}")
    };
    assert_eq!(pasted.calculation(0), Some(calc(&b, "Contacts::Last Name")));
    assert_eq!(pasted.number(0), Some(7));
    // One whose code is not its text's fails: either could be meant.
    let mut odd = ScriptStep::new(103);
    let shown = calc(&a, "Contacts::Last Name");
    let code = calc(&a, "Contacts::Last Name & 1");
    odd.set_calculation(
        0,
        Some(&Calculation::new(shown.formatted, code.polish.unwrap())),
    );
    let text = step_text(&odd, &a);
    match parse_steps(&text, &b, Origin::Unknown).unwrap().remove(0) {
        Pasted::Failed { reason, .. } => {
            assert!(reason.contains("the calculation of slot 0"), "{reason}")
        }
        Pasted::Step(_) => panic!("{text}"),
    }
}

/// Contacts with First Name and Last Name under each other's ids.
fn swapped_file() -> Contacts {
    let mut file = Contacts::new();
    file.names = NameTable::default();
    file.names.add_occurrence(CONTACTS, "Contacts", 0x8002);
    file.names.add_field(0x8002, 2, "First Name");
    file.names.add_field(0x8002, 1, "Last Name");
    file
}

#[test]
fn pasted_calculations_bind_fields_in_order_not_as_a_set_of_ids() {
    // B holds A's two field ids the other way round, so a calculation of
    // both names the same set of ids in either file.
    let a = Contacts::new();
    let b = swapped_file();
    let source = "Contacts::First Name & Contacts::Last Name";
    let read =
        |text: &str, names: &Contacts| parse_steps(text, names, Origin::Unknown).unwrap().remove(0);
    let mut exit = ScriptStep::new(103);
    exit.set_calculation(0, Some(&calc(&a, source)));
    exit.set_number(0, Some(7));
    let text = step_text(&exit, &a);
    assert!(text.contains("Stored slot 0"), "{text}");
    let Pasted::Step(pasted) = read(&text, &b) else {
        panic!("{text}")
    };
    assert!(pasted.is_enabled());
    assert_eq!(pasted.calculation(0), Some(calc(&b, source)));
    assert_eq!(pasted.number(0), Some(7));
    let Pasted::Step(back) = read(&text, &a) else {
        panic!("{text}")
    };
    assert_eq!(canonical(&back), canonical(&exit));
    // Code that binds its text's fields the other way round fails, from A
    // itself too: its text and code disagree.
    let shown = calc(&a, source);
    let code = calc(&a, "Contacts::Last Name & Contacts::First Name");
    let mut odd = ScriptStep::new(103);
    odd.set_calculation(
        0,
        Some(&Calculation::new(shown.formatted, code.polish.unwrap())),
    );
    let text = step_text(&odd, &a);
    assert!(text.contains(source), "{text}");
    for names in [&a, &b] {
        match read(&text, names) {
            Pasted::Failed { reason, .. } => {
                assert!(reason.contains("the calculation of slot 0"), "{reason}")
            }
            Pasted::Step(step) => panic!("enabled with {:?}", step.calculation(0)),
        }
    }
    let back = parse_record(&text, &a, Origin::ThisFile).unwrap();
    assert_eq!(canonical(&back), canonical(&odd));
}

#[test]
fn pasted_calculations_keep_names_quoted_another_way() {
    // The text prints `Contacts::First Name` for code that quoted either
    // name as `${ }`; quoting the same names is not a different binding.
    let a = Contacts::new();
    for source in [
        "Contacts::${First Name} & Contacts::Last Name",
        "${Contacts}::First Name",
        "${Contacts}::${First Name}",
    ] {
        let mut exit = ScriptStep::new(103);
        exit.set_calculation(0, Some(&calc(&a, source)));
        exit.set_number(0, Some(7));
        let text = step_text(&exit, &a);
        match parse_steps(&text, &a, Origin::Unknown).unwrap().remove(0) {
            Pasted::Step(pasted) => {
                assert!(pasted.is_enabled(), "{source}");
                assert_eq!(pasted.calculation(0), exit.calculation(0), "{source}");
            }
            Pasted::Failed { reason, .. } => panic!("{source}: {reason}"),
        }
    }
}

#[test]
fn pasted_plugin_calls_need_the_function_their_text_names() {
    let evil = om_formula::PluginFunctionId {
        plugin: *b"EVIL",
        function: 37,
    };
    let code = |function| {
        om_formula::polish::encode(&om_formula::Expr::PluginCall {
            function,
            args: vec![],
        })
    };
    let read =
        |text: &str, names: &Contacts| parse_steps(text, names, Origin::Unknown).unwrap().remove(0);
    // A file without the plug-in shows the call as `<Function Missing>`:
    // from elsewhere the step fails, for its text shows no code.
    let mut exit = ScriptStep::new(103);
    exit.set_calculation(0, Some(&Calculation::new(None, code(evil))));
    let a = Contacts::new();
    let text = step_text(&exit, &a);
    assert!(text.contains("<Function Missing>"), "{text}");
    match read(&text, &a) {
        Pasted::Failed { reason, .. } => {
            assert!(reason.contains("the calculation of slot 0"), "{reason}")
        }
        Pasted::Step(step) => panic!("enabled with {:?}", step.calculation(0)),
    }
    // A file with the plug-in names the call, and it reads.
    let mut b = Contacts::new();
    b.names.add_plugin_function(evil, "Evil_Call", 0, None);
    let text = step_text(&exit, &b);
    assert!(text.contains("Evil_Call"), "{text}");
    let Pasted::Step(back) = read(&text, &b) else {
        panic!("{text}")
    };
    assert_eq!(canonical(&back), canonical(&exit));
    // Text naming one plug-in function over code calling another fails.
    let shown = calc(&b, "Evil_Call");
    let other = om_formula::PluginFunctionId {
        function: 38,
        ..evil
    };
    let mut odd = ScriptStep::new(103);
    odd.set_calculation(0, Some(&Calculation::new(shown.formatted, code(other))));
    let text = step_text(&odd, &b);
    assert!(text.contains("Evil_Call"), "{text}");
    assert!(matches!(read(&text, &b), Pasted::Failed { .. }), "{text}");
}

#[test]
fn xml_past_the_attribute_bound_is_refused_whole() {
    let names = Contacts::new();
    // A step with `id` and `n` more attributes.
    let step = |n: usize| {
        let attrs: String = (0..n).map(|i| format!(" a{i}=\"\"")).collect();
        format!("<Step id=\"93\"{attrs}/>")
    };
    // At the bound the step reads, and fails: the attributes are unknown.
    match &parse_steps(&step(MAX_XML_ATTRIBUTES - 1), &names, Origin::Unknown).unwrap()[..] {
        [Pasted::Failed { reason, .. }] => assert!(reason.contains("a0"), "{reason}"),
        other => panic!("{other:?}"),
    }
    // One more is refused, and so are 40,000 (which took seconds to check
    // one against another) without reading past the bound.
    for n in [MAX_XML_ATTRIBUTES, 40_000] {
        let refused = parse_steps(&step(n), &names, Origin::Unknown).unwrap_err();
        assert!(refused.contains("more than 256 attributes"), "{refused}");
    }
    // A repeated attribute is still found among many.
    let repeated = step(200).replace("/>", " a0=\"\"/>");
    match &parse_steps(&repeated, &names, Origin::Unknown).unwrap()[..] {
        [Pasted::Failed { reason, .. }] => {
            assert!(reason.contains("attribute a0 is repeated"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn empty_elements_count_toward_the_xml_depth_bound() {
    let names = Contacts::new();
    let nested = |n: usize| format!("{}<Step id=\"93\"/>{}", "<g>".repeat(n), "</g>".repeat(n));
    // The step as deep as the bound reads; one level deeper is refused.
    assert!(matches!(
        &parse_steps(&nested(MAX_XML_DEPTH - 1), &names, Origin::Unknown).unwrap()[..],
        [Pasted::Step(beep)] if beep.id() == 93
    ));
    let refused = parse_steps(&nested(MAX_XML_DEPTH), &names, Origin::Unknown).unwrap_err();
    assert!(refused.contains("more than 128 deep"), "{refused}");
}

/// The account steps' options are the step value, nonzero when on, as
/// FileMaker Server's engine reads them (local-files.md 2.4, #525): text
/// sets them there, not in flag 0x100, and prints them back; so does
/// FileMaker's XML.
#[test]
fn account_step_options_are_the_step_value() {
    for (line, value) in [
        ("Add Account [ Account Name: \"u\" ; Password: \"p\" ; Expire password ]", 1),
        ("Add Account [ Account Name: \"u\" ; Password: \"p\" ]", 0),
        ("Reset Account Password [ Account Name: \"u\" ; New Password: \"q\" ; Expire password ]", 1),
        ("Enable Account [ Account Name: \"u\" ; Deactivate ]", 1),
        ("Enable Account [ Account Name: \"u\" ; Activate ]", 0),
    ] {
        let step = pasted(line);
        assert_eq!(step.value, value, "{line}");
        assert!(!step.has_flag(flag::OPTION), "{line}");
        assert_eq!(shown(&step), line);
    }
    let xml = r#"<fmxmlsnippet type="FMObjectList"><Step id="137" name="Enable Account"><Options>16384</Options><ParameterValues><Parameter type="AccountName"><Calculation position="0"><Calculation><Text><![CDATA["u"]]></Text></Calculation></Calculation></Parameter><Parameter type="Boolean"><Boolean type="EnableAccountBoolean" value="False" name="Activate"></Boolean></Parameter></ParameterValues></Step></fmxmlsnippet>"#;
    let pasted = parse_steps(xml, &Contacts::new(), Origin::Unknown).unwrap();
    let [Pasted::Step(step)] = &pasted[..] else {
        panic!("{pasted:?}");
    };
    assert_eq!(step.value, 1);
    assert_eq!(step.flags, 16384, "<Options> is the stored flags word");
    assert_eq!(
        shown(step),
        "Enable Account [ Account Name: \"u\" ; Deactivate ]"
    );
    // Activate with `kMenuSelection` in `<Options>`: value 0 is the choice
    // given, not a missing one.
    let xml = xml
        .replace("<Options>16384</Options>", "<Options>16386</Options>")
        .replace("value=\"False\"", "value=\"True\"");
    let pasted = parse_steps(&xml, &Contacts::new(), Origin::Unknown).unwrap();
    let [Pasted::Step(step)] = &pasted[..] else {
        panic!("{pasted:?}");
    };
    assert_eq!((step.value, step.flags), (0, 16386));
    assert_eq!(
        shown(step),
        "Enable Account [ Account Name: \"u\" ; Activate ]"
    );
}

/// Add Account's privilege set is a flagless key in `mReserved` (#525):
/// its text with the stored bytes keeps it within this file, and from
/// elsewhere the step is refused rather than added without its set.
#[test]
fn add_accounts_stored_privilege_set_is_kept_or_refused() {
    let mut step = pasted("Add Account [ Account Name: \"u\" ; Password: \"p\" ]");
    step.reserved[..3].copy_from_slice(&[2, 1, 2]);
    let text = step_text(&step, &Contacts::new());
    let here = parse_steps(&text, &Contacts::new(), Origin::ThisFile).unwrap();
    assert!(
        matches!(&here[..], [Pasted::Step(back)] if back.reserved == step.reserved),
        "{text}: {here:?}"
    );
    let elsewhere = parse_steps(&text, &Contacts::new(), Origin::Unknown).unwrap();
    assert!(
        matches!(&elsewhere[..], [Pasted::Failed { reason, .. }] if reason.contains("privilege set")),
        "{elsewhere:?}"
    );
}
