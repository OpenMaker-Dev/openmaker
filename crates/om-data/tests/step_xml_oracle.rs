//! FileMaker's XML for script steps read back against the stored steps
//! (script-steps.md, "FileMaker XML on paste"): every `<Step>` of a
//! FileMaker Server `FMDeveloperTool --saveAsXML` export, read with
//! `om_scripts::display::parse_xml_steps` and the file's own names, must
//! give back the step the file stores (compared as `canonical` steps).
//!
//! Ignored by default: the exports and files exist only on the analysis
//! host. Run with
//!   OM_XML_ORACLE=<file.fmp12>=<export.xml>:... \
//!     cargo test --release -p om-data --test step_xml_oracle -- --ignored --nocapture
//! or `OM_XML_ORACLE=<dir>` for every `<name>.xml` beside a `<name>.fmp12`
//! in that directory (`tools/fms-script-harness/run.sh -x` writes them).
//! `OM_XML_ORACLE_SHOW=<n>` prints up to n differences per step id.
//!
//! The XML round trip: `OM_XML_ROUNDTRIP=<source.fmp12>:<export.xml>:<out.fmp12>`
//! reads every script step of an export and writes the steps that read
//! into the first script of a copy of `source`; after FileMaker exports
//! that copy, `OM_XML_ROUNDTRIP_CHECK=<source.fmp12>:<export.xml>:<re-export.xml>`
//! compares each step's XML with the original's (UUIDs, hashes and
//! indexes aside). Equal XML means the reader kept everything FileMaker's
//! XML shows of the step.
//!
//! `OM_XML_PROBE=<source.fmp12>:<out.fmp12>` writes a probe file for the
//! oracle instead: a copy of the Tasks template whose first script holds,
//! for every named step id, the bare step and one step per schema
//! parameter with only that parameter stored, plus the steps of
//! [`PROBE_LINES`] (written with OpenMaker's step text and `om_hbam`'s
//! writer). Its export shows how FileMaker's XML writes each parameter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use om_data::{ScriptCatalog, ScriptKind};
use om_hbam::{Catalog, HbamFile, OwnedNode};
use om_scripts::display::{canonical, parse_steps, parse_xml_steps, Origin, Pasted, StepNames};
use om_scripts::specs::{encode_requests, SortEntry, SortSpec, SpecField, StoredRequest};
use om_scripts::{
    flag, step_schemas, Calculation, FieldSpec, Location, RefKind, ScriptStep, SlotKind, StoredKey,
};

/// The (file, export) pairs of `OM_XML_ORACLE`.
fn pairs() -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    for entry in std::env::var("OM_XML_ORACLE")
        .unwrap_or_default()
        .split(':')
        .filter(|e| !e.is_empty())
    {
        if let Some((file, xml)) = entry.split_once('=') {
            out.push((PathBuf::from(file), PathBuf::from(xml)));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(entry) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().is_some_and(|e| e == "xml") {
                let file = path.with_extension("fmp12");
                if file.is_file() {
                    out.push((file, path));
                }
            }
        }
    }
    out.sort();
    out
}

/// The `<Step>` elements of each script of an export's `StepsForScripts`,
/// as text: (script id, script name, step index, step XML). Layout
/// objects' single steps lie outside it.
fn script_steps(xml: &str) -> Vec<(u64, String, usize, String)> {
    let mut out = Vec::new();
    let Some(start) = xml.find("<StepsForScripts") else {
        return out;
    };
    let end = xml[start..]
        .find("</StepsForScripts>")
        .map_or(xml.len(), |e| start + e);
    let mut rest = &xml[start..end];
    while let Some(at) = rest.find("<ScriptReference ") {
        rest = &rest[at..];
        let id = attr(rest, "id").and_then(|v| v.parse().ok()).unwrap_or(0);
        let name = attr(rest, "name").unwrap_or_default().to_owned();
        let block_end = rest.find("</Script>").unwrap_or(rest.len());
        let mut block = &rest[..block_end];
        let mut index = 0;
        while let Some(s) = block.find("<Step ") {
            let e = block[s..]
                .find("</Step>")
                .map(|e| s + e + "</Step>".len())
                .or_else(|| block[s..].find("/>").map(|e| s + e + 2))
                .unwrap_or(block.len());
            out.push((id, name.clone(), index, block[s..e].to_owned()));
            index += 1;
            block = &block[e..];
        }
        rest = &rest[block_end..];
    }
    out
}

/// An attribute of the first tag of `text`.
fn attr<'t>(text: &'t str, name: &str) -> Option<&'t str> {
    let tag = &text[..text.find('>')?];
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    Some(&tag[at..at + tag[at..].find('"')?])
}

/// A step's XML on one line, without the white space between elements.
fn compact(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(at) = rest.find('>') {
        out.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        let trimmed = rest.trim_start();
        if trimmed.starts_with('<') {
            rest = trimmed;
        }
    }
    out.push_str(rest);
    out
}

/// A key's bytes past its length zeroed: the filler the XML cannot carry
/// (stale or uninitialized bytes, script-steps.md 2.1).
fn zero_filler(key: &mut [u8]) {
    let used = (1 + usize::from(key[0])).min(key.len());
    key[used..].fill(0);
}

/// A canonical step with the filler of its references zeroed: the bytes
/// after each key in `mReserved` and in slot field specs, and all of
/// `mReserved` when no flag says it holds a reference.
fn without_filler(step: &ScriptStep) -> ScriptStep {
    let mut step = step.clone();
    let spec_filler = |bytes: &mut [u8; 14]| {
        zero_filler(&mut bytes[..6]);
        zero_filler(&mut bytes[6..12]);
    };
    if step.has_flag(flag::FIELD_SPEC) {
        spec_filler(&mut step.reserved);
    } else if step.has_flag(flag::MEMBER_KEY) || step.has_flag(flag::SCRIPT_KEY) {
        zero_filler(&mut step.reserved[..6]);
        step.reserved[6..].fill(0);
    } else {
        step.reserved = [0; 14];
    }
    for slot in 1..3 {
        if let Some(mut spec) = step.slot_field_spec(slot) {
            spec_filler(&mut spec.0);
            step.set_field_spec(slot, Some(spec));
        }
    }
    step
}

/// What differs between two canonical steps, briefly.
fn difference(want: &ScriptStep, got: &ScriptStep) -> String {
    let mut parts = Vec::new();
    if want.opcode != got.opcode {
        parts.push(format!("id {} vs {}", want.opcode, got.opcode));
    }
    if want.flags != got.flags {
        parts.push(format!("flags {:#x} vs {:#x}", want.flags, got.flags));
    }
    if want.value != got.value {
        parts.push(format!("value {} vs {}", want.value, got.value));
    }
    if want.reserved != got.reserved {
        let filler = without_filler(want).reserved == without_filler(got).reserved;
        let hex = |r: &[u8]| r.iter().map(|b| format!("{b:02x}")).collect::<String>();
        parts.push(format!(
            "reserved {} vs {}{}",
            hex(&want.reserved),
            hex(&got.reserved),
            if filler { " (filler)" } else { "" }
        ));
    }
    let empty = om_scripts::StepData::default();
    let (w, g) = (
        want.data.as_ref().unwrap_or(&empty),
        got.data.as_ref().unwrap_or(&empty),
    );
    let mut slots: Vec<u16> = w
        .slots()
        .map(|(n, _)| n)
        .chain(g.slots().map(|(n, _)| n))
        .collect();
    slots.sort();
    slots.dedup();
    for n in slots {
        if w.slot(n) != g.slot(n) {
            parts.push(format!("slot {n}: {:?} vs {:?}", w.slot(n), g.slot(n)));
        }
    }
    let mut keys: Vec<u8> = w.keys().chain(g.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        if w.key(k) != g.key(k) {
            parts.push(format!("key {k:02x}: {:?} vs {:?}", w.key(k), g.key(k)));
        }
    }
    if w.other() != g.other() || w.node_value() != g.node_value() {
        parts.push("other data".into());
    }
    parts.join("; ")
}

#[derive(Default)]
struct Tally {
    compared: usize,
    exact: usize,
    /// Equal but for key filler bytes, which the XML does not carry.
    filler: usize,
    failed: usize,
    mismatched: usize,
}

#[test]
#[ignore = "requires FileMaker Server exports of the sample files"]
fn xml_steps_read_back_as_their_stored_steps() {
    let pairs = pairs();
    assert!(!pairs.is_empty(), "OM_XML_ORACLE names no file and export");
    let show: usize = std::env::var("OM_XML_ORACLE_SHOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let mut total = Tally::default();
    let mut by_id: BTreeMap<u16, Tally> = BTreeMap::new();
    let mut details: BTreeMap<(u16, String), Vec<String>> = BTreeMap::new();
    let mut documents = 0;
    let mut layout_steps = (0, 0);
    for (file, export) in &pairs {
        let fm = HbamFile::open(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let tree = fm.key_tree().expect("key tree");
        let catalog = Catalog::from_tree(&tree).expect("catalog");
        let names = om_data::rules::schema::calc_schema(&tree, &catalog, "oracle");
        let scripts = ScriptCatalog::decode(&tree).expect("scripts");
        let xml = std::fs::read_to_string(export).expect("export");
        documents += 1;
        // The whole document reads as one step per `<Step>` element.
        let all = parse_xml_steps(&xml, &names).unwrap();
        assert_eq!(
            all.len(),
            xml.matches("<Step ").count(),
            "{}",
            export.display()
        );
        let in_scripts = script_steps(&xml);
        for (id, name, index, text) in &in_scripts {
            // A patch's scripts may be numbered anew when FMUpgradeTool
            // adds them: then they are found by name.
            let script = |by_id: bool| {
                scripts.scripts.iter().find(|s| {
                    s.kind == ScriptKind::Script
                        && if by_id { s.id == *id } else { s.name == *name }
                })
            };
            let Some(stored) = script(true)
                .or_else(|| script(false))
                .and_then(|s| s.steps.get(*index))
            else {
                panic!("{}: script {id} has no step {index}", file.display());
            };
            let step_id = stored.id();
            let tally = by_id.entry(step_id).or_default();
            tally.compared += 1;
            total.compared += 1;
            let read = parse_xml_steps(text, &names).unwrap();
            let at = format!("{} script {id} #{index}", file.display());
            let [one] = read.as_slice() else {
                panic!("{at}: {} steps", read.len());
            };
            match one {
                Pasted::Step(step) if canonical(step) == canonical(stored) => {
                    tally.exact += 1;
                    total.exact += 1;
                }
                Pasted::Step(step)
                    if without_filler(&canonical(step)) == without_filler(&canonical(stored)) =>
                {
                    tally.filler += 1;
                    total.filler += 1;
                }
                Pasted::Step(step) => {
                    tally.mismatched += 1;
                    total.mismatched += 1;
                    let diff = difference(&canonical(stored), &canonical(step));
                    details
                        .entry((step_id, "mismatched".into()))
                        .or_default()
                        .push(format!("{at}: {diff}\n      {}", compact(text)));
                }
                Pasted::Failed { reason, .. } => {
                    tally.failed += 1;
                    total.failed += 1;
                    details
                        .entry((step_id, "failed".into()))
                        .or_default()
                        .push(format!("{at}: {reason}\n      {}", compact(text)));
                }
            }
        }
        // Layout objects' steps (before and after the scripts in document
        // order) are read but not compared: their stored form lives in the
        // layout. Every one must read.
        let first = xml.find("<StepsForScripts").unwrap_or(xml.len());
        let before = xml[..first].matches("<Step ").count();
        let after = all.len() - before - in_scripts.len();
        for step in all[..before].iter().chain(&all[all.len() - after..]) {
            layout_steps.0 += 1;
            match step {
                Pasted::Step(_) => layout_steps.1 += 1,
                Pasted::Failed { reason, text } => {
                    println!(
                        "LAYOUT STEP FAILED {}: {reason}\n      {}",
                        export.display(),
                        compact(text)
                    )
                }
            }
        }
    }
    println!(
        "{documents} exports: {} script steps compared, {} exact, {} equal but for key filler, \
         {} failed as unmappable, {} mismatched; layout object steps: {} of {} read",
        total.compared,
        total.exact,
        total.filler,
        total.failed,
        total.mismatched,
        layout_steps.1,
        layout_steps.0
    );
    for (id, t) in &by_id {
        let name = om_scripts::step_name(*id).unwrap_or("?");
        if t.exact + t.filler == t.compared {
            println!(
                "  {id:>3} {name:<32} {:>5} equal ({} filler)",
                t.compared, t.filler
            );
        } else {
            println!(
                "  {id:>3} {name:<32} {:>5} compared, {} exact, {} filler, {} failed, {} mismatched",
                t.compared, t.exact, t.filler, t.failed, t.mismatched
            );
        }
    }
    for ((id, kind), lines) in &details {
        println!("{id} {kind}: {}", lines.len());
        for line in lines.iter().take(show) {
            println!("    {line}");
        }
    }
}

// ----- the probe file -------------------------------------------------------

/// Step text lines for parameter combinations the one-parameter steps do
/// not show, in OpenMaker's text form with the Tasks template's names.
const PROBE_LINES: &str = "\
Perform Script [ “Sort by Name” ; Parameter: 1 ]
Perform Script [ Specified: By name ; \"Sort by Name\" ; Parameter: 2 ]
Perform Script on Server [ “Sort by Status” ; Parameter: 3 ; Wait for completion: Off ]
Perform Script on Server [ Specified: By name ; \"Sort by Status\" ; Parameter: 3 ]
Go to Layout [ original layout ; Animation: None ]
Go to Layout [ “Task Details” (Tasks) ; Animation: Slide in from Left ]
Go to Layout [ Layout Name: \"Task List\" ; Animation: Zoom Out ]
Go to Layout [ Layout Number: 2 ]
Go to Record/Request/Page [ First ]
Go to Record/Request/Page [ Next ; Exit after last: On ]
Go to Record/Request/Page [ Previous ; Exit after last: Off ]
Go to Record/Request/Page [ With dialog: Off ; 3 ]
Go to Portal Row [ Select: On ; Next ; Exit after last: On ]
Go to Portal Row [ Select: Off ; Last ]
Set Variable [ $x[3] ; Value: 1 ]
Set Variable [ $$g ; Value: \"a\" ]
Set Variable [ $y ; Value: 2 ]
Set Field [ Tasks::Task ; \"x\" ]
Set Field [ Tasks::Task[2] ; \"x\" ]
Set Field [ Tasks::Task[$i] ; \"x\" ]
Set Field By Name [ \"Tasks::Task\" ; 1 ]
Go to Field [ Select/perform ; Tasks::Status ]
Go to Field [ Assignees::First Name[2] ]
Insert Text [ Select ; Target: $v ; \"hello\" ]
Insert Text [ Target: Tasks::Task ; \"hello¶two\" ]
Insert Calculated Result [ Select ; Target: $v[2] ; 1 + 1 ]
Insert Calculated Result [ Target: Tasks::Description ; 1 + 1 ]
Insert Current Date [ Select ; Target: $d ]
Insert from URL [ Select ; With dialog: Off ; Target: $r ; \"https://example.com\" ; Verify SSL Certificates ; cURL options: \"-X GET\" ]
Show Custom Dialog [ Title: \"T\" ; Message: \"M\" ; Default Button: “OK”, Commit: “Yes” ; Button 2: “Cancel”, Commit: “No” ]
Show Custom Dialog [ Title: \"T\" ; Message: \"M\" ; Default Button: “OK”, Commit: “Yes” ; Input #1: Tasks::Task, \"Label\" ; Input #2: $v, \"L2\", Password ]
Pause/Resume Script [ Duration (seconds): 2 ]
Pause/Resume Script [ Indefinitely ]
Loop [ Flush: Minimum ]
Loop [ Flush: Defer ]
End Loop
Revert Transaction [ Condition: 1 ; Error Code: 5001 ; Error Message: \"m\" ]
Open Transaction [ Skip auto-enter options ; Skip data entry validation ]
New Window [ Style: Document ; Name: \"w\" ; Using layout: “Task Details” (Tasks) ; Height: 100 ; Width: 200 ; Top: 10 ; Left: 20 ]
New Window [ Style: Dialog ; Name: \"d\" ]
Close Window [ Name: \"w\" ; Current file ]
Close Window [ Current Window ]
Select Window [ Name: \"w\" ]
Adjust Window [ Maximize ]
Go to Related Record [ Show only related records ; From table: “Assignees” ; Using layout: “Assignee List” (Assignees) ]
Go to Related Record [ From table: “Assignments” ; Using layout: <Current Layout> ; New window ; Animation: None ]
Sort Records [ Restore ; With dialog: Off ]
Sort Records by Field [ Descending ; Tasks::Due Date ]
Unsort Records
Perform Find [ Restore ]
Enter Find Mode [ Pause: Off ; Restore ]
Constrain Found Set [ Restore ; Find without indexes ]
Extend Found Set [ Restore ]
Commit Records/Requests [ Skip data entry validation ; With dialog: Off ]
Delete Record/Request [ With dialog: Off ]
Set Error Capture [ On ]
Allow User Abort [ Off ]
Exit Script [ Text Result: \"done\" ]
Exit Script [ Text Result: ]
If [ $x = 1 ]
Else If [ $x = 2 ]
Else
End If
Exit Loop If [ 1 ]
Halt Script
# a comment
# two¶lines
Beep
Refresh Window [ Flush cached join results ; Flush cached external data ]
Scroll Window [ Page Down ]
Show/Hide Toolbars [ Lock: On ; Hide ]
Set Zoom Level [ Lock: Off ; 150% ]
View As [ View as List ]
Freeze Window
Open URL [ With dialog: Off ; \"https://example.com\" ]
Send Mail
Export Field Contents [ Tasks::Description ; “file:out.txt” ; Create folders: Off ]
Insert File [ Tasks::Description ; “file:in.txt” ]
Truncate Table [ With dialog: Off ; Table: “Tasks” ]
Install OnTimer Script [ “Sort by Name” ; Parameter: 1 ; Interval: 10 ]
Set Web Viewer [ Object Name: \"wv\" ; Action: Reload ]
Perform JavaScript in Web Viewer [ Object Name: \"wv\" ; Function Name: \"f\" ; Parameters: 1, \"a\" ]
Execute SQL [ With dialog: Off ; Calculated SQL Text: \"SELECT 1\" ]
Set Layout Object Animation [ On ]
Set Revert Transaction on Error [ Off ]
Commit Transaction
Get Folder Path [ Allow Folder Creation ; $folder ]
Read from Data File [ File ID: 1 ; Amount (bytes): 10 ; Target: $data ; Read as: UTF-8 ]
Write to Data File [ File ID: 1 ; Data source: $data ; Write as: UTF-8 ; Append line feed ]
Create Data File [ “file:a.txt” ; Create folders: On ]
Open Data File [ “file:a.txt” ; Target: $id ]
Close Data File [ File ID: 1 ]
Rename File [ Source file: “file:a.txt” ; New name: \"b.txt\" ]
Delete File [ Target file: “file:a.txt” ]
Get File Exists [ “file:a.txt” ; Target: $e ]
Get File Size [ “file:a.txt” ; Target: $s ]
Set Data File Position [ File ID: 1 ; New position: 5 ]
Add Account [ Account Name: \"u\" ; Password: \"p\" ; Expire password ]
Delete Account [ Account Name: \"u\" ]
Reset Account Password [ Account Name: \"u\" ; New Password: \"q\" ]
Enable Account [ Account Name: \"u\" ; Deactivate ]
Re-Login [ With dialog: Off ; Account Name: \"u\" ; Password: \"p\" ]
Change Password [ Old Password: \"a\" ; New Password: \"b\" ; With dialog: Off ]
Set Multi-User [ On (Hidden) ]
Save a Copy as [ “file:copy.fmp12” ; compacted copy (smaller) ; Create folders: Off ]
Open File [ Open hidden: On ; \"x\" ]
Close File [ Current File ]
Dial Phone [ With dialog: Off ; \"555\" ]
Speak [ \"hello\" ]
Perform AppleScript [ “tell app” ]
Set Selection [ Tasks::Task ; Start Position: 1 ; End Position: 2 ]
Replace Field Contents [ With dialog: Off ; Tasks::Task ; Serial numbers ]
Replace Field Contents [ With dialog: Off ; Tasks::Task ; \"x\" ]
Relookup Field Contents [ With dialog: Off ; Tasks::Task ]
Cut [ Select ; Tasks::Task ]
Copy [ Tasks::Task ]
Paste [ Select ; No style ; Tasks::Task ]
Clear [ Select ; Tasks::Task ]
Undo/Redo [ Redo ]
Set Use System Formats [ On ]
Install Menu Set [ [Standard FileMaker Menus] ; Use as file default: On ]
Perform Find/Replace [ With dialog: Off ; \"a\" ; \"b\" ]
Go to Object [ Object Name: \"o\" ; Repetition: 2 ]
Refresh Object [ Object Name: \"o\" ; Repetition: 2 ]
Refresh Portal [ Object Name: \"p\" ]
Go to List of Records [ List of record IDs: \"1¶2\" ; Using layout: “Task List” (Tasks) ; New window ; Animation: None ]
Enable Touch Keyboard [ Toggle ]
Arrange All Windows [ Cascade Window ]
Move/Resize Window [ Current Window ; Height: 100 ; Width: 200 ; Top: 1 ; Left: 2 ]
Set Window Title [ Current Window ; New Title: \"t\" ]
Show/Hide Menubar [ Lock: On ; Toggle ]
Show/Hide Text Ruler [ Toggle ]
Print Setup [ Restore ; With dialog: Off ]
Print [ Restore ; With dialog: Off ]
Save Records as PDF [ Restore ; “file:a.pdf” ; Create folders: Off ; With dialog: Off ; Records being browsed ]
Save Records as Snapshot Link [ “file:a.fmpsl” ; Current record ; Create folders: Off ]
Save Records as Excel [ With dialog: Off ; “file:a.xlsx” ; Records being browsed ; Create folders: Off ]
Export Records [ With dialog: Off ; Create folders: Off ; “file:a.tab” ]
Import Records [ With dialog: Off ; Source: “file:a.tab” ]
Convert File [ With dialog: Off ; “file:a.tab” ]
Set Dictionary
Check Selection [ Select ; Tasks::Task ]
Check Record
Check Found Set
Correct Word
Spelling Options
Edit User Dictionary
Flush Cache to Disk
Exit Application
Open Settings
Open Find/Replace
Open Help
Open Upload to Host
Open Manage Database
Open Manage Containers
Open Manage Data Sources
Open Manage Layouts
Open Manage Themes
Open Manage Value Lists
Open Script Workspace
Open Sharing
Open File Options
Open Hosts
Open Favorites
Open Record/Request
Revert Record/Request [ With dialog: Off ]
Duplicate Record/Request
Delete Portal Row [ With dialog: Off ]
Delete All Records [ With dialog: Off ]
Show All Records
Show Omitted Only
Omit Record
Omit Multiple Records [ With dialog: Off ; 3 ]
Find Matching Records [ Constrain ; Tasks::Status ]
Modify Last Find
Copy All Records/Requests
Copy Record/Request
Enter Browse Mode [ Pause: Off ]
Enter Preview Mode [ Pause: On ]
Go to Next Field
Go to Previous Field
Select All
Execute FileMaker Data API [ Select ; Target: $out ; \"{}\" ]
Perform Script on Server with Callback [ “Sort by Name” ; Parameter: 1 ; Callback script specified: From list ; Callback script parameter: 2 ; State: Resume ]
Set Allowed Orientations
";

/// A sample value in every location of a parameter: each flag bit set,
/// step value and list choices 2, calculations `"s<slot>"`, texts
/// `t<slot>`, numbers 1, the field `Tasks::Task`, the layout `Task
/// Details`, the script `Sort by Name`, the occurrence `Assignees`, a path
/// `file:probe.txt`, one find request and a one-field sort order.
fn fill(step: &mut ScriptStep, location: &Location, names: &dyn StepNames) {
    let calc_names = names.calc_names();
    let tasks = calc_names.find_occurrence("Tasks").expect("Tasks");
    let task = calc_names.find_field(tasks, "Task").expect("Task");
    let key = |id: u64| StoredKey::from_id(id).expect("key");
    let field = FieldSpec::new(key(tasks), key(task), 1);
    let calc = |text: &str| -> Calculation {
        om_scripts::display::compile(text, names).expect("calculation")
    };
    match *location {
        Location::Flag { bits, .. } => step.set_flag(bits, true),
        Location::Value => {
            step.value = 2;
            step.flags |= flag::MENU_SELECTION;
        }
        Location::Reserved(RefKind::Field) => step.set_field_spec(0, Some(field)),
        Location::Reserved(RefKind::Member) => {
            let layout = names
                .find_layout("Task Details", None)
                .one()
                .expect("layout");
            step.set_member_key(Some(key(layout)));
        }
        Location::Reserved(RefKind::Script) => {
            let script = names.find_script("Sort by Name").one().expect("script");
            step.set_script_key(StoredKey::from_serial_id(script));
        }
        Location::Reserved(RefKind::Key) => {}
        Location::Slot { slot, kind } => match kind {
            SlotKind::Calculation => {
                step.set_calculation(slot, Some(&calc(&format!("\"s{slot}\""))))
            }
            SlotKind::Text => step.set_text(slot, Some(&format!("t{slot}"))),
            SlotKind::Number => step.set_number(slot, Some(1)),
            SlotKind::List => step.set_list_value(slot, Some(2)),
            SlotKind::Field => step.set_field_spec(slot, Some(field)),
            SlotKind::Aliases => {
                let assignees = calc_names.find_occurrence("Assignees").expect("Assignees");
                step.set_slot_child(
                    slot,
                    0x11,
                    Some(OwnedNode::leaf(
                        [0x11],
                        om_formula::Key::from_id(assignees).0,
                    )),
                );
            }
            SlotKind::Path => {
                let mut stream = 1u32.to_be_bytes().to_vec();
                for part in ["file", "", "probe.txt"] {
                    let bytes = om_scsu::encode_scsu(part);
                    stream.push(bytes.len() as u8);
                    stream.extend(bytes);
                }
                step.set_slot_child(slot, 0x82, Some(om_scripts::bytes_node(&[0x82], &stream)));
                step.flags |= flag::FILE_PATH;
            }
        },
        Location::Key(0x06) => {
            let status = calc_names.find_field(tasks, "Status").expect("Status");
            step.set_data_leaf(
                0x06,
                Some(encode_requests(&[StoredRequest {
                    omit: false,
                    criteria: vec![(
                        SpecField {
                            occurrence: Some(tasks),
                            field: status,
                            repetition: 1,
                        },
                        "Pending".into(),
                    )],
                }])),
            );
        }
        Location::Key(0x0a) => {
            let spec = SortSpec {
                keep_sorted: true,
                blanks_last: false,
                fields: vec![SortEntry {
                    field: SpecField {
                        occurrence: Some(tasks),
                        field: task,
                        repetition: 1,
                    },
                    summary: None,
                    value_list: None,
                    descending: true,
                    locale: 0,
                }],
            };
            step.set_data_leaf(0x0a, Some(om_scripts::bytes_node(&[0x0a], &spec.encode())));
        }
        Location::Key(_) => {}
    }
}

/// Raw steps for the cases the one-parameter steps and the step text
/// leave open: find requests and sort orders with Restore, repetitions of
/// variable and field targets, the account steps' values, window choices,
/// layouts with animations, by-name script references and comments with
/// line breaks.
fn targeted_probe_steps(names: &om_data::host::CalcSchema) -> Vec<ScriptStep> {
    let calc_names = names.calc_names();
    let tasks = calc_names.find_occurrence("Tasks").expect("Tasks");
    let assignees = calc_names.find_occurrence("Assignees").expect("Assignees");
    let field = |occurrence: u64, name: &str| calc_names.find_field(occurrence, name).expect(name);
    let key = |id: u64| StoredKey::from_id(id).expect("key");
    let calc = |text: &str| -> Calculation {
        om_scripts::display::compile(text, names).expect("calculation")
    };
    let spec_field = |occurrence: u64, name: &str, repetition: u16| SpecField {
        occurrence: Some(occurrence),
        field: field(occurrence, name),
        repetition,
    };
    let mut out = Vec::new();
    let requests = encode_requests(&[
        StoredRequest {
            omit: false,
            criteria: vec![
                (spec_field(tasks, "Status", 1), "Pending".into()),
                (spec_field(tasks, "Task", 2), "x & \"y\"".into()),
            ],
        },
        StoredRequest {
            omit: true,
            criteria: vec![(spec_field(assignees, "First Name", 1), "==$name".into())],
        },
    ]);
    for id in [22, 28, 126, 127] {
        let mut step = ScriptStep::new(id);
        step.set_data_leaf(0x06, Some(requests.clone()));
        step.set_flag(flag::RESTORE, true);
        out.push(step);
    }
    let status_list = names.value_list_named("Status").expect("Status list").id;
    let sorts = [
        SortSpec {
            keep_sorted: false,
            blanks_last: true,
            fields: vec![
                SortEntry {
                    field: spec_field(tasks, "Task", 1),
                    summary: None,
                    value_list: None,
                    descending: true,
                    locale: 0,
                },
                SortEntry {
                    field: spec_field(tasks, "Status", 1),
                    summary: None,
                    value_list: Some(status_list),
                    descending: false,
                    locale: 0,
                },
                SortEntry {
                    field: spec_field(assignees, "Last Name", 1),
                    summary: None,
                    value_list: None,
                    descending: false,
                    locale: 25,
                },
            ],
        },
        SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: Vec::new(),
        },
    ];
    for spec in &sorts {
        let mut step = ScriptStep::new(39);
        step.set_data_leaf(0x0a, Some(om_scripts::bytes_node(&[0x0a], &spec.encode())));
        step.set_flag(flag::RESTORE | flag::NO_INTERACT, true);
        out.push(step);
    }
    // Variable and field targets with repetitions.
    for (repetition, calculation) in [(1, Some("2")), (1, Some("$i")), (2, None), (0, Some("$i"))] {
        let mut step = ScriptStep::new(61);
        step.set_variable_target(0, "$v");
        let mut spec = FieldSpec::variable(repetition);
        if repetition == 0 {
            spec = FieldSpec::variable(0);
        }
        step.set_field_spec(0, Some(spec));
        if let Some(text) = calculation {
            step.set_calculation(0x20, Some(&calc(text)));
        }
        step.set_text(0, Some("text"));
        out.push(step);
    }
    for (repetition, calculation) in [(2, None), (0, Some("$i")), (0, Some("2"))] {
        let mut step = ScriptStep::new(77);
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                key(tasks),
                key(field(tasks, "Task")),
                repetition,
            )),
        );
        if let Some(text) = calculation {
            step.set_calculation(0x20, Some(&calc(text)));
        }
        step.set_calculation(0, Some(&calc("1")));
        out.push(step);
    }
    for (repetition, calculation) in [(0, Some("2")), (0, Some("$i"))] {
        let mut step = ScriptStep::new(76);
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                key(tasks),
                key(field(tasks, "Task")),
                repetition,
            )),
        );
        if let Some(text) = calculation {
            step.set_calculation(10, Some(&calc(text)));
        }
        out.push(step);
    }
    // The account steps' values.
    for (id, value) in [
        (134, 1),
        (134, 2),
        (134, 4),
        (134, 6),
        (136, 1),
        (136, 2),
        (136, 4),
        (137, 1),
        (137, 2),
        (137, 4),
    ] {
        let mut step = ScriptStep::new(id);
        step.value = value;
        step.flags |= flag::MENU_SELECTION;
        step.set_calculation(0, Some(&calc("\"u\"")));
        out.push(step);
    }
    // Window choices.
    for (id, value) in [(121, 1), (121, 2), (123, 1), (123, 2), (124, 2), (119, 2)] {
        let mut step = ScriptStep::new(id);
        step.value = value;
        step.flags |= flag::MENU_SELECTION;
        step.set_calculation(0, Some(&calc("\"w\"")));
        out.push(step);
    }
    let detail = key(names
        .find_layout("Task Details", None)
        .one()
        .expect("layout"));
    let mut window = ScriptStep::new(122);
    window.value = 5;
    window.set_member_key(Some(detail));
    window.set_number(0, Some(0x0037_0602));
    window.set_calculation(0, Some(&calc("\"w\"")));
    out.push(window);
    let mut window = ScriptStep::new(122);
    window.value = 1;
    window.set_number(0, Some(0x8005_0204_u32 as i32));
    out.push(window);
    let mut related = ScriptStep::new(74);
    related.value = 5;
    related.set_member_key(Some(key(names
        .find_layout("Assignee List", None)
        .one()
        .expect("layout"))));
    related.set_slot_child(
        0,
        0x11,
        Some(OwnedNode::leaf(
            [0x11],
            om_formula::Key::from_id(assignees).0,
        )),
    );
    related.set_flag(flag::TURN_ON | flag::OPTION2 | flag::RESTORE, true);
    related.set_number(0, Some(0x0037_0602));
    for slot in 0..5 {
        related.set_calculation(slot, Some(&calc(&format!("{}", 100 + slot))));
    }
    related.set_list_value(6, Some(3));
    out.push(related);
    for animation in [None, Some(0), Some(7)] {
        let mut step = ScriptStep::new(6);
        step.value = 5;
        step.set_member_key(Some(detail));
        if let Some(a) = animation {
            step.set_list_value(6, Some(a));
        }
        out.push(step);
    }
    // By-name script references.
    let mut psos = ScriptStep::new(164);
    psos.set_flag(0x2004800, true);
    psos.set_list_value(1, Some(1));
    psos.set_calculation(2, Some(&calc("\"Sort by Name\"")));
    out.push(psos);
    let mut psos = ScriptStep::new(164);
    psos.set_flag(0x2004800, true);
    psos.set_calculation(1, Some(&calc("\"Sort by Name\"")));
    out.push(psos);
    let mut psos = ScriptStep::new(164);
    psos.set_list_value(1, Some(1));
    psos.set_calculation(2, Some(&calc("\"Sort by Name\"")));
    out.push(psos);
    let mut callback = ScriptStep::new(210);
    callback.set_script_key(StoredKey::from_serial_id(
        names.find_script("Sort by Name").one().expect("script"),
    ));
    // The callback from the list as the hex literal of its key in slot 3,
    // its parameter in slot 4 (script-steps.md 12.2).
    let status = names.find_script("Sort by Status").one().expect("script");
    let hex: String = om_hbam::serial::encode(status)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    callback.set_calculation(3, Some(&calc(&format!("\"{hex}\""))));
    callback.set_calculation(4, Some(&calc("4")));
    callback.value = 3;
    callback.flags |= flag::MENU_SELECTION | flag::OPTION | flag::OPTION2;
    out.push(callback);
    let mut external = ScriptStep::new(1);
    external.set_script_key(StoredKey::from_serial_id(3));
    external.set_data_leaf(0x20, Some(OwnedNode::leaf([0x20], [1, 1])));
    out.push(external);
    // Comments with line breaks; Insert Text's text slots.
    for text in ["a\nb", "a\r\nb", "tab\tend", "quote \" < & >"] {
        let mut comment = ScriptStep::new(89);
        comment.set_text(0, Some(text));
        out.push(comment);
    }
    for slots in [&[1u16][..], &[0, 1]] {
        let mut insert = ScriptStep::new(61);
        for slot in slots {
            insert.set_text(*slot, Some(&format!("slot {slot}")));
        }
        out.push(insert);
    }
    let mut truncate = ScriptStep::new(182);
    truncate.value = 2;
    truncate.flags |= flag::MENU_SELECTION;
    let table = names.find_table("Tasks").one().expect("table");
    truncate.set_member_key(Some(key(table)));
    out.push(truncate);
    let mut pause = ScriptStep::new(62);
    pause.value = 1;
    out.push(pause);
    // Replace Field Contents with serial numbers and a calculation.
    let mut replace = ScriptStep::new(91);
    replace.set_field_spec(
        0,
        Some(FieldSpec::new(key(tasks), key(field(tasks, "Task")), 1)),
    );
    replace.set_number(1, Some(2));
    replace.set_number(2, Some(5));
    replace.set_text(3, Some("10"));
    replace.set_number_bool(4, Some(true));
    replace.set_number_bool(5, Some(true));
    replace.set_slot_child(0, 0x11, Some(OwnedNode::leaf([0x11], [1, 0])));
    out.push(replace);
    // Show Custom Dialog labels: text only, calculation only, both.
    let mut dialog = ScriptStep::new(87);
    dialog.set_text(0, Some("OK"));
    dialog.set_calculation(6, Some(&calc("\"Two\"")));
    dialog.set_text(2, Some("Three"));
    dialog.set_calculation(7, Some(&calc("\"Other\"")));
    for n in 3..6 {
        dialog.set_number_bool(n, Some(n == 3));
    }
    out.push(dialog);
    out
}

#[test]
#[ignore = "writes a probe file from a sample file"]
fn write_probe_file() {
    let Ok(spec) = std::env::var("OM_XML_PROBE") else {
        panic!("OM_XML_PROBE=<source.fmp12>:<out.fmp12>");
    };
    let (source, target) = spec.split_once(':').expect("<source>:<out>");
    let file = HbamFile::open(Path::new(source)).expect("source");
    let tree = file.key_tree().expect("key tree");
    let catalog = Catalog::from_tree(&tree).expect("catalog");
    let names = om_data::rules::schema::calc_schema(&tree, &catalog, "probe");
    let mut scripts = ScriptCatalog::decode(&tree).expect("scripts");
    let mut steps = Vec::new();
    for schema in step_schemas() {
        if !om_scripts::is_valid_step_id(schema.id)
            || om_scripts::OBSOLETE_STEP_IDS.contains(&schema.id)
        {
            continue;
        }
        steps.push(ScriptStep::new(schema.id));
        for param in &schema.params {
            let mut step = ScriptStep::new(schema.id);
            for location in &param.storage {
                fill(&mut step, location, &names);
            }
            // A slot holds a list choice or children, not both.
            let slots: Vec<u16> = step
                .data
                .iter()
                .flat_map(|d| d.slots().map(|(n, _)| n).collect::<Vec<_>>())
                .collect();
            for n in slots {
                let children = step
                    .data
                    .as_ref()
                    .and_then(|d| d.slot(n))
                    .is_some_and(|s| s.child_keys().next().is_some());
                if children && step.list_value(n).is_some() {
                    step.set_list_value(n, None);
                }
            }
            if step != ScriptStep::new(schema.id) {
                steps.push(step);
            }
        }
    }
    steps.extend(targeted_probe_steps(&names));
    for (pasted, line) in parse_steps(PROBE_LINES, &names, Origin::Unknown)
        .unwrap()
        .into_iter()
        .zip(PROBE_LINES.lines())
    {
        match pasted {
            Pasted::Step(step) => steps.push(step),
            Pasted::Failed { reason, .. } => println!("not a probe step: {line}: {reason}"),
        }
    }
    let script = scripts
        .scripts
        .iter_mut()
        .find(|s| s.kind == ScriptKind::Script)
        .expect("a script");
    script.steps.clear();
    for (n, mut step) in (1u64..).zip(steps) {
        step.data_key = StoredKey::from_segment(&om_hbam::serial::encode(n)).expect("key");
        script.steps.push(step);
    }
    println!(
        "{} probe steps in script {} “{}”",
        script.steps.len(),
        script.id,
        script.name
    );
    let mut root = tree.root.to_owned_node();
    let segment = om_hbam::serial::encode(script.id);
    root.put(&[&[17], &[5], &segment], script.encode());
    std::fs::write(
        target,
        om_hbam::writer::rebuild(&root, file.superblock().raw).expect("rebuild"),
    )
    .expect("write");
}

// ----- the XML round trip -------------------------------------------------

/// Replace the steps of the first script of `source` and write the file.
fn write_steps(source: &Path, target: &Path, steps: Vec<ScriptStep>) -> (u64, String) {
    let file = HbamFile::open(source).expect("source");
    let tree = file.key_tree().expect("key tree");
    let mut scripts = ScriptCatalog::decode(&tree).expect("scripts");
    let script = scripts
        .scripts
        .iter_mut()
        .find(|s| s.kind == ScriptKind::Script)
        .expect("a script");
    script.steps.clear();
    for (n, mut step) in (1u64..).zip(steps) {
        step.data_key = StoredKey::from_segment(&om_hbam::serial::encode(n)).expect("key");
        script.steps.push(step);
    }
    let mut root = tree.root.to_owned_node();
    let segment = om_hbam::serial::encode(script.id);
    root.put(&[&[17], &[5], &segment], script.encode());
    std::fs::write(
        target,
        om_hbam::writer::rebuild(&root, file.superblock().raw).expect("rebuild"),
    )
    .expect("write");
    (script.id, script.name.clone())
}

/// A step's XML without what differs between two exports of the same step:
/// hashes, indexes, UUIDs and the white space between elements.
fn comparable(xml: &str) -> String {
    let mut text = compact(xml);
    for (start, end) in [("<UUID>", "</UUID>"), ("<SourceUUID>", "</SourceUUID>")] {
        while let Some(a) = text.find(start) {
            let Some(b) = text[a..].find(end) else { break };
            text.replace_range(a..a + b + end.len(), "");
        }
    }
    for attr in [" hash=\"", " index=\"", " UUID=\""] {
        while let Some(a) = text.find(attr) {
            let Some(b) = text[a + attr.len()..].find('"') else {
                break;
            };
            text.replace_range(a..a + attr.len() + b + 1, "");
        }
    }
    text
}

#[test]
#[ignore = "writes a file from a FileMaker Server export"]
fn write_xml_round_trip_file() {
    let Ok(spec) = std::env::var("OM_XML_ROUNDTRIP") else {
        panic!("OM_XML_ROUNDTRIP=<source.fmp12>:<export.xml>:<out.fmp12>");
    };
    let parts: Vec<&str> = spec.split(':').collect();
    let [source, export, target] = parts.as_slice() else {
        panic!("OM_XML_ROUNDTRIP=<source.fmp12>:<export.xml>:<out.fmp12>");
    };
    let file = HbamFile::open(Path::new(source)).expect("source");
    let tree = file.key_tree().expect("key tree");
    let catalog = Catalog::from_tree(&tree).expect("catalog");
    let names = om_data::rules::schema::calc_schema(&tree, &catalog, "round trip");
    let xml = std::fs::read_to_string(export).expect("export");
    let mut steps = Vec::new();
    let mut failed = 0;
    for (_, _, _, text) in script_steps(&xml) {
        match parse_xml_steps(&text, &names).unwrap().remove(0) {
            Pasted::Step(step) => steps.push(step),
            Pasted::Failed { .. } => failed += 1,
        }
    }
    let count = steps.len();
    let (id, name) = write_steps(Path::new(source), Path::new(target), steps);
    println!("{count} steps written to script {id} “{name}”; {failed} did not read");
}

#[test]
#[ignore = "requires an export and its round trip's export"]
fn xml_round_trips_through_filemaker() {
    let usage = "OM_XML_ROUNDTRIP_CHECK=<source.fmp12>:<export.xml>:<re-export.xml>";
    let spec = std::env::var("OM_XML_ROUNDTRIP_CHECK").expect(usage);
    let parts: Vec<&str> = spec.split(':').collect();
    let [source, export, again] = parts.as_slice() else {
        panic!("{usage}");
    };
    let file = HbamFile::open(Path::new(source)).expect("source");
    let tree = file.key_tree().expect("key tree");
    let catalog = Catalog::from_tree(&tree).expect("catalog");
    let names = om_data::rules::schema::calc_schema(&tree, &catalog, "round trip");
    let original = std::fs::read_to_string(export).expect("export");
    let again = std::fs::read_to_string(again).expect("re-export");
    // The steps that read, in order, against the first script of the
    // re-export.
    let read: Vec<String> = script_steps(&original)
        .into_iter()
        .map(|(_, _, _, text)| text)
        .filter(|text| matches!(parse_xml_steps(text, &names).unwrap()[0], Pasted::Step(_)))
        .collect();
    let first = script_steps(&again)
        .first()
        .map(|(id, ..)| *id)
        .expect("a script");
    let written: Vec<String> = script_steps(&again)
        .into_iter()
        .filter(|(id, ..)| *id == first)
        .map(|(_, _, _, text)| text)
        .collect();
    let show: usize = std::env::var("OM_XML_ORACLE_SHOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let mut equal = 0;
    let mut differ = BTreeMap::<String, Vec<String>>::new();
    for (a, b) in read.iter().zip(&written) {
        let (a, b) = (comparable(a), comparable(b));
        if a == b {
            equal += 1;
        } else {
            let id = attr(&a, "id").unwrap_or("?").to_owned();
            differ
                .entry(id)
                .or_default()
                .push(format!("\n      was {a}\n      now {b}"));
        }
    }
    println!(
        "{} steps read, {} written back: {equal} with equal XML, {} differ",
        read.len(),
        written.len(),
        differ.values().map(Vec::len).sum::<usize>()
    );
    for (id, lines) in &differ {
        println!("step {id}: {} differ", lines.len());
        for line in lines.iter().take(show) {
            println!("   {line}");
        }
    }
}
