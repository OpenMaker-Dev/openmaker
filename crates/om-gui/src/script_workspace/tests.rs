use super::*;

fn loaded() -> ScriptWorkspace {
    let mut workspace = ScriptWorkspace::new("Test (host)".into());
    workspace.accept_catalog(Ok(ScriptCatalog {
        scripts: vec![
            Script {
                id: 2,
                name: "Original".into(),
                kind: ScriptKind::Script,
                ..Default::default()
            },
            Script {
                id: 3,
                name: "Other".into(),
                kind: ScriptKind::Script,
                ..Default::default()
            },
        ],
        writable: true,
        unavailable_reason: None,
    }));
    workspace
}

/// For the app's tests: a step added to the active script, unsaved.
pub(crate) fn edit(workspace: &mut ScriptWorkspace) {
    workspace.editor_mut().unwrap().insert_new(75);
}

/// For the app's tests: file `file`'s loaded workspace, its first script
/// ("Original", id 2) with an unsaved step.
pub(crate) fn drafted(file: u64) -> ScriptWorkspace {
    let mut workspace = loaded().owned_by(file);
    edit(&mut workspace);
    workspace
}

/// For the app's tests: save the active script's draft.
pub(crate) fn save_draft(workspace: &mut ScriptWorkspace) -> Vec<ScriptWorkspaceAction> {
    workspace.save(Then::Stay)
}

/// For the app's tests: whether the workspace waits for a save, whether
/// its active script has unsaved steps, and the alert it shows.
pub(crate) fn state(workspace: &ScriptWorkspace) -> (bool, bool, Option<String>) {
    let alert = match &workspace.dialog {
        Some(Dialog::Alert(text)) => Some(text.clone()),
        _ => None,
    };
    (
        workspace.busy,
        workspace.editor().is_some_and(Editor::dirty),
        alert,
    )
}

#[test]
fn the_title_names_the_document_window() {
    assert_eq!(loaded().title(), "Script Workspace (Test (host))");
}

#[test]
fn the_first_script_opens_in_the_preview_tab_and_edits_keep_it() {
    let mut workspace = loaded();
    assert_eq!(workspace.tabs.len(), 1);
    assert!(workspace.tabs[0].preview);
    // Another script replaces an unedited preview.
    workspace.preview(3);
    assert_eq!(workspace.tabs.len(), 1);
    assert_eq!(workspace.editor().unwrap().id(), 3);
    edit(&mut workspace);
    assert!(!workspace.tabs[0].preview);
    // A further preview opens a second tab; the edited one stays.
    workspace.preview(2);
    assert_eq!(workspace.tabs.len(), 2);
    assert!(workspace.tabs[0].dirty());
}

#[test]
fn a_failed_save_keeps_the_draft_and_says_why() {
    let mut workspace = loaded();
    edit(&mut workspace);
    let actions = workspace.save(Then::Stay);
    assert!(matches!(actions.as_slice(), [ScriptWorkspaceAction::Save(s)] if s.steps.len() == 1));
    assert!(workspace.busy);
    workspace.accept_saved(Err(DataError::new("Server rejected the edit")));
    assert!(workspace.editor().unwrap().dirty());
    assert!(!workspace.busy);
    assert!(matches!(&workspace.dialog, Some(Dialog::Alert(m))
        if m.contains("“Original” could not be saved") && m.contains("Server rejected")));
    assert!(workspace
        .catalog
        .as_ref()
        .unwrap()
        .script(2)
        .unwrap()
        .steps
        .is_empty());
}

#[test]
fn closing_asks_to_save_each_unsaved_script() {
    let mut workspace = loaded();
    edit(&mut workspace);
    assert!(workspace.request_close().is_empty());
    assert!(matches!(
        workspace.dialog,
        Some(Dialog::SaveChanges {
            id: 2,
            then: Then::CloseWorkspace
        })
    ));
    // Save: the save goes out, and once confirmed the workspace closes.
    workspace.dialog = None;
    let actions = workspace.save(Then::CloseWorkspace);
    let ScriptWorkspaceAction::Save(saved) = &actions[0] else {
        panic!("{actions:?}");
    };
    let after = workspace.accept_saved(Ok(saved.clone()));
    assert_eq!(after, [ScriptWorkspaceAction::Close]);
    assert!(!workspace.tabs.iter().any(|t| t.dirty()));
    assert_eq!(
        workspace
            .catalog
            .as_ref()
            .unwrap()
            .script(2)
            .unwrap()
            .steps
            .len(),
        1
    );
}

#[test]
fn running_an_edited_script_asks_to_save_first() {
    let mut workspace = loaded();
    assert_eq!(workspace.run(), [ScriptWorkspaceAction::Run(2)]);
    edit(&mut workspace);
    assert!(workspace.run().is_empty());
    assert!(matches!(
        workspace.dialog,
        Some(Dialog::SaveChanges {
            id: 2,
            then: Then::Run(2)
        })
    ));
    // Don't Save reverts the draft, then runs.
    workspace.tabs[0].revert();
    assert_eq!(
        workspace.proceed(Then::Run(2)),
        [ScriptWorkspaceAction::Run(2)]
    );
    assert!(!workspace.tabs.iter().any(|t| t.dirty()));
}

#[test]
fn a_refused_catalog_change_keeps_the_catalog_and_alerts() {
    let mut workspace = loaded();
    let before = workspace.catalog.clone();
    workspace.busy = true;
    workspace.accept_catalog_saved(Err(DataError::new("not available")));
    assert_eq!(workspace.catalog, before);
    assert!(!workspace.busy);
    assert!(matches!(&workspace.dialog, Some(Dialog::Alert(m)) if m.contains("not available")));
}

#[test]
fn renames_and_deletes_reach_open_tabs() {
    let mut workspace = loaded();
    let mut next = workspace.catalog.clone().unwrap();
    next.scripts[0].name = "Renamed".into();
    workspace.accept_catalog_saved(Ok(next.clone()));
    assert_eq!(workspace.editor().unwrap().script.name, "Renamed");
    next.scripts.remove(0);
    workspace.accept_catalog_saved(Ok(next));
    assert!(workspace.tabs.is_empty());
}

#[test]
fn calculations_compile_to_their_stored_streams_or_filemakers_error() {
    let calc = calculation::compile("Get ( ScriptParameter )", None).unwrap();
    let mut step = ScriptStep::new(145);
    let target = CalculationTarget::for_option(&step, 0, None).unwrap();
    set_parameter_calculation(&mut step, target, Some(&calc)).unwrap();
    assert_eq!(
        step.argument_text(0).as_deref(),
        Some("Get ( ScriptParameter )")
    );
    assert_eq!(parameter_calculation(&step, target), Some(calc));
    let error = calculation::compile("1 +", None).unwrap_err();
    assert!(!error.is_empty());
}

fn enter_in_workspace(workspace: &mut ScriptWorkspace) -> Vec<ScriptWorkspaceAction> {
    let context = egui::Context::default();
    let mut actions = Vec::new();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events: vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        },
        |ui| {
            workspace.contents(ui, &mut actions);
            workspace.dialogs(ui.ctx(), &mut actions);
        },
    );
    output.drop_without_applying_deltas();
    actions
}

#[test]
fn accepting_a_save_prompt_does_not_insert_the_steps_pane_selection() {
    let mut workspace = loaded();
    edit(&mut workspace);
    workspace.focus = Focus::Steps;
    workspace.steps.selected = Some(93); // Beep, selected only to read its description.
    workspace.dialog = Some(Dialog::SaveChanges {
        id: 2,
        then: Then::Stay,
    });
    let actions = enter_in_workspace(&mut workspace);
    let [ScriptWorkspaceAction::Save(script)] = actions.as_slice() else {
        panic!("{actions:?}");
    };
    assert_eq!(
        script.steps.iter().map(ScriptStep::id).collect::<Vec<_>>(),
        [75]
    );
}

#[test]
fn a_pending_save_does_not_accept_steps_pane_keyboard_insertions() {
    let mut workspace = loaded();
    edit(&mut workspace);
    let _ = workspace.save(Then::Stay);
    workspace.focus = Focus::Steps;
    workspace.steps.selected = Some(1);
    assert!(enter_in_workspace(&mut workspace).is_empty());
    assert_eq!(
        workspace
            .editor()
            .unwrap()
            .script
            .steps
            .iter()
            .map(ScriptStep::id)
            .collect::<Vec<_>>(),
        [75]
    );
}

#[test]
fn displayed_calculations_read_and_replace_only_their_own_slot() {
    use om_data::scripts::display::{step_line, NoStepNames};
    for id in [119, 122, 211] {
        let slots: &[u16] = if id == 211 { &[0, 1, 2] } else { &[1, 2, 3, 4] };
        let mut step = ScriptStep::new(id);
        for slot in slots {
            let calc = calculation::compile(&(100 + slot).to_string(), None).unwrap();
            step.set_calculation(*slot, Some(&calc));
        }
        let before = step.clone();
        let line = step_line(&step, &NoStepNames);
        for span in line
            .options
            .iter()
            .filter(|span| span.calculation_slot.is_some())
        {
            let target = CalculationTarget::for_option(
                &step,
                span.parameter.unwrap(),
                span.calculation_slot,
            )
            .unwrap();
            assert_eq!(
                parameter_calculation(&step, target),
                before.calculation(target.slot)
            );
            let replacement = calculation::compile("999", None).unwrap();
            set_parameter_calculation(&mut step, target, Some(&replacement)).unwrap();
            assert_eq!(step.calculation(target.slot), Some(replacement));
            for sibling in slots.iter().copied().filter(|slot| *slot != target.slot) {
                assert_eq!(
                    step.calculation(sibling),
                    before.calculation(sibling),
                    "step {id} slot {sibling}"
                );
            }
            set_parameter_calculation(&mut step, target, None).unwrap();
            assert!(step.calculation(target.slot).is_none());
            step = before.clone();
        }
        let parameter = line
            .options
            .iter()
            .find_map(|span| span.calculation_slot.and(span.parameter))
            .unwrap();
        let invalid = CalculationTarget {
            parameter,
            slot: 99,
        };
        assert!(set_parameter_calculation(&mut step, invalid, None).is_err());
        assert_eq!(step, before);
    }
}

#[test]
fn inline_calculations_set_and_clear_the_options_dialog_companion_flags() {
    let mut step = ScriptStep::new(193);
    let parameter = step
        .schema()
        .unwrap()
        .params
        .iter()
        .position(|p| p.name == "Amount")
        .unwrap();
    let target = CalculationTarget::for_option(&step, parameter, None).unwrap();
    step.set_flag(0x200, true);
    let calc = calculation::compile("256", None).unwrap();
    set_parameter_calculation(&mut step, target, Some(&calc)).unwrap();
    assert!(step.has_flag(0x8000));
    assert!(step.has_flag(0x200));
    set_parameter_calculation(&mut step, target, None).unwrap();
    assert!(!step.has_flag(0x8000));
    assert!(step.has_flag(0x200));
}

#[test]
fn a_failed_inline_compile_keeps_the_typed_draft() {
    let mut workspace = loaded();
    let mut step = ScriptStep::new(145);
    let original = calculation::compile("42", None).unwrap();
    step.set_calculation(0, Some(&original));
    workspace.editor_mut().unwrap().script.steps = vec![step.clone()];
    let target = CalculationTarget::for_option(&step, 0, None).unwrap();
    workspace.editor_request(EditorRequest::CompileInline {
        index: 0,
        target,
        text: "1 +".into(),
        original: "42".into(),
    });
    assert!(matches!(workspace.dialog, Some(Dialog::Alert(_))));
    assert!(
        matches!(&workspace.editor().unwrap().inline, Some(editor::Inline::Calculation { text, original, .. }) if text == "1 +" && original == "42")
    );
    assert_eq!(workspace.editor().unwrap().script.steps[0], step);
}

#[test]
fn catalog_options_survive_saving_or_reverting_an_open_draft() {
    for revert in [false, true] {
        let mut workspace = loaded();
        edit(&mut workspace);
        let mut catalog = workspace.catalog.clone().unwrap();
        catalog.scripts[0].set_runs_with_full_access(true);
        catalog.scripts[0].set_in_scripts_menu(false);
        let metadata = catalog.scripts[0].metadata.clone();
        workspace.accept_catalog_saved(Ok(catalog));
        assert!(workspace.editor().unwrap().dirty());
        assert_eq!(workspace.editor().unwrap().script.steps.len(), 1);
        if revert {
            workspace.editor_mut().unwrap().revert();
            assert!(workspace.editor().unwrap().script.steps.is_empty());
        } else {
            let actions = workspace.save(Then::Stay);
            let ScriptWorkspaceAction::Save(script) = &actions[0] else {
                panic!("{actions:?}")
            };
            assert_eq!(script.metadata, metadata);
            workspace.accept_saved(Ok(script.clone()));
        }
        assert_eq!(workspace.editor().unwrap().script.metadata, metadata);
        assert!(workspace.editor().unwrap().script.runs_with_full_access());
        assert!(!workspace.editor().unwrap().script.in_scripts_menu());
    }
}

#[test]
fn catalog_changes_refresh_step_names_and_pickers_without_changing_document_schema() {
    let mut workspace = loaded();
    let source = Arc::new(CalcSchema::new("Test", Default::default()));
    workspace.set_names(Some(source.clone()));
    assert_eq!(
        workspace.names.as_ref().unwrap().script_name(2),
        Some("Original")
    );
    let mut catalog = workspace.catalog.clone().unwrap();
    catalog.scripts[0].name = "Renamed".into();
    catalog.scripts.remove(1);
    catalog.scripts.push(Script {
        id: 4,
        name: "New Script".into(),
        ..Default::default()
    });
    workspace.accept_catalog_saved(Ok(catalog));
    let names = workspace.names.as_ref().unwrap();
    assert_eq!(names.script_name(2), Some("Renamed"));
    assert_eq!(names.script_name(3), None);
    assert_eq!(names.script_name(4), Some("New Script"));
    assert_eq!(
        names.scripts().iter().map(|s| s.id).collect::<Vec<_>>(),
        [2, 4]
    );
    assert!(source.scripts().is_empty());
    let revision = workspace.names_revision;
    workspace.set_names(Some(source));
    assert_eq!(workspace.names_revision, revision);
    assert_eq!(
        workspace.names.as_ref().unwrap().script_name(2),
        Some("Renamed")
    );
}

#[test]
fn a_read_only_catalog_keeps_local_drafts_but_never_sends_writes() {
    let mut workspace = loaded();
    let mut catalog = workspace.catalog.clone().unwrap();
    catalog.writable = false;
    workspace.accept_catalog(Ok(catalog.clone()));
    assert_eq!(workspace.run(), [ScriptWorkspaceAction::Run(2)]);
    edit(&mut workspace);
    assert!(workspace.save(Then::Stay).is_empty());
    assert!(workspace.change_catalog(catalog.clone()).is_empty());
    assert!(!workspace.busy);
    assert!(workspace.editor().unwrap().dirty());
    let mut requests = Vec::new();
    workspace
        .pane
        .create(&catalog, ScriptKind::Script, true, &mut requests);
    workspace.pane.rename(&catalog, 2);
    assert!(requests.is_empty());
    assert!(workspace.pane.renaming.is_none());
}

#[test]
fn closing_a_workspace_or_tab_waits_for_the_pending_save() {
    let mut workspace = loaded();
    edit(&mut workspace);
    let actions = workspace.save(Then::Stay);
    let ScriptWorkspaceAction::Save(saved) = &actions[0] else {
        panic!("{actions:?}")
    };
    assert!(workspace.request_close().is_empty());
    assert!(workspace.close_tab(0).is_empty());
    assert!(workspace.dialog.is_none());
    assert_eq!(workspace.tabs.len(), 1);
    workspace.accept_saved(Ok(saved.clone()));
    assert_eq!(workspace.request_close(), [ScriptWorkspaceAction::Close]);
}

#[test]
fn cut_copies_exactly_the_deleted_block_partners_and_can_paste_them() {
    let context = egui::Context::default();
    clipboard::activate(&context, 1, None);
    let mut script = Script {
        steps: [68, 1, 69, 1, 70, 75]
            .into_iter()
            .map(ScriptStep::new)
            .collect(),
        ..Default::default()
    };
    script.update_partner_indexes().unwrap();
    let mut editor = Editor::new(script, false);
    editor.selection.insert(0);
    clipboard::put(&context, &editor, true);
    let copied = clipboard::get(&context, "If [ ]\nElse\nEnd If\n");
    assert_eq!(
        copied.iter().map(|s| s.opcode).collect::<Vec<_>>(),
        [68, 69, 70]
    );
    assert!(editor.delete());
    assert_eq!(
        editor
            .script
            .steps
            .iter()
            .map(|s| s.opcode)
            .collect::<Vec<_>>(),
        [1, 1, 75]
    );
    assert!(editor.paste(&copied));
}

#[test]
fn stale_text_and_another_workspace_cannot_paste_local_step_ids() {
    let context = egui::Context::default();
    let mut editor = Editor::new(
        Script {
            steps: vec![ScriptStep::new(93)],
            ..Default::default()
        },
        false,
    );
    editor.selection.insert(0);
    clipboard::activate(&context, 1, None);
    clipboard::put(&context, &editor, false);
    assert_eq!(clipboard::get(&context, "Beep\n").len(), 1);
    assert!(clipboard::get(&context, "unrelated clipboard text").is_empty());
    clipboard::activate(&context, 2, None);
    assert!(clipboard::get(&context, "Beep\n").is_empty());
}

#[test]
fn a_script_selector_label_never_edits_its_hidden_name_calculation() {
    use om_data::scripts::display::{step_line, NoStepNames};
    for id in [1, 164] {
        let mut step = ScriptStep::new(id);
        let line = step_line(&step, &NoStepNames);
        let selector = line
            .options
            .iter()
            .find(|s| line.text[s.range.clone()].contains("Specified: From list"))
            .unwrap();
        assert!(selector.calculation_slot.is_none());
        assert!(CalculationTarget::for_span(&step, selector).is_none());
        step.set_list_value(1, Some(1));
        let calculation = calculation::compile("\"Target\"", None).unwrap();
        step.set_calculation(2, Some(&calculation));
        let line = step_line(&step, &NoStepNames);
        let label = line
            .options
            .iter()
            .find(|s| line.text[s.range.clone()].contains("Specified: By name"))
            .unwrap();
        assert!(CalculationTarget::for_span(&step, label).is_none());
        let name = line
            .options
            .iter()
            .find(|s| s.calculation_slot == Some(2))
            .unwrap();
        assert_eq!(CalculationTarget::for_span(&step, name).unwrap().slot, 2);
    }
}

/// A table occurrence of a test file, on a base table of the same name.
struct Occurrence<'a> {
    id: u64,
    name: &'a str,
    table: u64,
    fields: &'a [(u64, &'a str)],
}

/// A workspace on a file of its own: `scripts` (the first opens), the
/// `occurrences` and `layouts` of the first occurrence.
fn file(
    scripts: &[(u64, &str)],
    occurrences: &[Occurrence],
    layouts: &[(u32, &str)],
) -> ScriptWorkspace {
    use om_data::calc::{Locale, ValueType};
    use om_data::host::FieldDef;
    let mut workspace = ScriptWorkspace::new("File (host)".into());
    workspace.accept_catalog(Ok(ScriptCatalog {
        scripts: scripts
            .iter()
            .map(|(id, name)| Script {
                id: *id,
                name: (*name).into(),
                kind: ScriptKind::Script,
                ..Default::default()
            })
            .collect(),
        writable: true,
        unavailable_reason: None,
    }));
    let mut schema = CalcSchema::new("File", Locale::en_us());
    for o in occurrences {
        schema.add_table(o.table, o.name);
        for (id, field) in o.fields {
            schema.add_field(o.table, FieldDef::data(*id, *field, ValueType::Text));
        }
        schema.add_occurrence(o.id, o.name, o.table);
    }
    for (id, name) in layouts {
        schema.add_layout(*id, *name, Some(occurrences[0].id));
    }
    workspace.set_names(Some(Arc::new(schema)));
    workspace
}

/// A key with non-zero filler, so the step's text carries the bytes as a
/// `Stored reference`.
fn with_filler(mut key: om_data::scripts::StoredKey) -> om_data::scripts::StoredKey {
    key.0[5] = 0x77;
    key
}

#[test]
fn steps_copied_from_another_file_paste_by_name_never_by_colliding_ids() {
    use om_data::scripts::display::Pasted;
    use om_data::scripts::{FieldSpec, StoredKey};
    let context = egui::Context::default();
    // File A: Contacts (0xd00002 on 0x8002) with Last Name 2 and Nickname
    // 3, layout Detail 3, scripts Original 2 and Helper 3.
    let mut a = file(
        &[(2, "Original"), (3, "Helper")],
        &[Occurrence {
            id: 0xd00002,
            name: "Contacts",
            table: 0x8002,
            fields: &[(1, "First Name"), (2, "Last Name"), (3, "Nickname")],
        }],
        &[(3, "Detail")],
    );
    // File B: the same names under other ids, and A's ids for other
    // objects: 0xd00002 is People, its fields 2 Phone and 3 Fax; layout 3
    // Elsewhere, script 2 Cleanup; no Nickname.
    let mut b = file(
        &[(2, "Cleanup"), (5, "Original")],
        &[
            Occurrence {
                id: 0xd00007,
                name: "Contacts",
                table: 0x8009,
                fields: &[(5, "First Name"), (6, "Last Name")],
            },
            Occurrence {
                id: 0xd00002,
                name: "People",
                table: 0x8002,
                fields: &[(2, "Phone"), (3, "Fax")],
            },
        ],
        &[(9, "Detail"), (3, "Elsewhere")],
    );
    // A's steps, each reference with filler bytes its text must store.
    let field = |id: u8| {
        let mut step = ScriptStep::new(76);
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                with_filler(StoredKey([4, 3, 0xd0, 0, 2, 0])),
                StoredKey([2, 1, id, 0, 0, 0]),
                1,
            )),
        );
        step
    };
    let mut layout = ScriptStep::new(6);
    layout.value = 5;
    layout.set_member_key(Some(with_filler(StoredKey::from_id(3).unwrap())));
    let mut perform = ScriptStep::new(1);
    perform.set_script_key(Some(with_filler(StoredKey::from_serial_id(2).unwrap())));
    let steps = vec![field(2), layout, perform, field(3)];
    let editor = a.editor_mut().unwrap();
    editor.script.steps = steps;
    editor.selection = (0..4).collect();
    clipboard::activate(&context, a.clipboard_scope, a.names.clone());
    clipboard::put(&context, a.editor_mut().unwrap(), false);
    let copied = a.editor_mut().unwrap().script.steps.clone();
    let text = om_data::scripts::display::steps_text(&copied, a.names.as_deref().unwrap());
    assert_eq!(text.matches("Stored reference").count(), 4, "{text}");
    // Pasted in B, whose workspace did not copy them: read by name.
    clipboard::activate(&context, b.clipboard_scope, b.names.clone());
    let before = b.editor_mut().unwrap().script.steps.len();
    b.paste_steps(&context, 0, &text);
    let pasted = &b.editor_mut().unwrap().script.steps[before..];
    let spec = pasted[0].field_spec().unwrap();
    assert_eq!(
        (spec.table().id(), spec.field().id()),
        (Some(0xd00007), Some(6))
    );
    assert_eq!(pasted[1].member_key().and_then(|k| k.id()), Some(9));
    assert_eq!(pasted[2].script_id(), Some(5));
    // Nickname is not in B: A's bytes (People::Fax here) are not taken.
    assert_eq!(pasted[3].id(), 89);
    assert!(!pasted[3].is_enabled());
    assert!(b
        .editor_mut()
        .unwrap()
        .marked
        .values()
        .any(|why| why.contains("Nickname")));
    assert!(matches!(
        om_data::scripts::display::parse_steps(&text, b.names.as_deref().unwrap(), Origin::Unknown)
            .unwrap()[3],
        Pasted::Failed { .. }
    ));
}

#[test]
fn pasted_steps_get_their_own_uuids() {
    let context = egui::Context::default();
    let mut workspace = loaded();
    // Text, XML holding another step's UUID, and a line that does not
    // read: each a new step with a UUID of its own.
    let xml = "<Step enable=\"True\" id=\"93\" name=\"Beep\">\
               <UUID>00112233-4455-6677-8899-AABBCCDDEEFF</UUID></Step>";
    for text in ["Beep\n", xml, "invalid step\n"] {
        workspace.paste_steps(&context, 0, text);
    }
    // This workspace's own copy of a step stored without one.
    let editor = workspace.editor_mut().unwrap();
    assert!(editor.insert_at(0, vec![ScriptStep::new(93)]));
    clipboard::activate(&context, workspace.clipboard_scope, None);
    clipboard::put(&context, workspace.editor_mut().unwrap(), false);
    workspace.paste_steps(&context, 0, "Beep\n");
    let steps = &workspace.editor_mut().unwrap().script.steps;
    let ids: Vec<u16> = steps.iter().map(|s| s.id()).collect();
    assert_eq!(ids, [93, 93, 93, 93, 89]);
    assert_eq!(steps[0].unique_id(), None);
    let xml_uuid = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let uuids: BTreeSet<[u8; 16]> = steps[1..].iter().filter_map(|s| s.unique_id()).collect();
    assert_eq!(uuids.len(), 4, "{steps:?}");
    assert!(!uuids.contains(&xml_uuid));
}

#[test]
fn this_workspaces_own_copy_pastes_only_within_the_limits() {
    use om_data::scripts::display::{MAX_PASTE_BYTES, MAX_PASTE_STEPS};
    let context = egui::Context::default();
    let mut workspace = loaded();
    clipboard::activate(&context, workspace.clipboard_scope, None);
    // Copy steps `from..to` of the script and paste them after the last.
    let copy_paste = |workspace: &mut ScriptWorkspace, from: usize, to: usize| {
        let editor = workspace.editor_mut().unwrap();
        editor.selection = (from..to).collect();
        clipboard::put(&context, editor, false);
        let text = context.output(|o| {
            o.commands.iter().rev().find_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
        });
        workspace.paste_steps(&context, 0, &text.unwrap());
        workspace.editor_mut().unwrap().refusal.take()
    };
    // Install Menu Set by id: only the workspace's own copy pastes it.
    let mut menus = ScriptStep::new(142);
    menus.set_member_key(om_data::scripts::StoredKey::from_id(5));
    let editor = workspace.editor_mut().unwrap();
    let count = MAX_PASTE_STEPS + 1;
    editor
        .script
        .insert_steps(0, vec![menus.clone(); count])
        .unwrap();
    let refused = copy_paste(&mut workspace, 0, count);
    assert_eq!(
        refused.as_deref(),
        Some(
            format!(
                "Nothing was pasted. The clipboard holds {count} steps; \
                 a paste inserts at most {MAX_PASTE_STEPS}."
            )
            .as_str()
        )
    );
    assert_eq!(workspace.editor_mut().unwrap().script.steps.len(), count);
    assert_eq!(copy_paste(&mut workspace, 1, count), None);
    let steps = &workspace.editor_mut().unwrap().script.steps;
    assert_eq!(steps.len(), 2 * count - 1);
    assert!(steps.iter().all(|s| s.member_key() == menus.member_key()));
    // A comment whose text is past the size bound.
    let mut comment = ScriptStep::new(89);
    comment.set_text(0, Some(&"x".repeat(MAX_PASTE_BYTES)));
    let editor = workspace.editor_mut().unwrap();
    editor.script.steps.clear();
    editor.script.insert_steps(0, [comment]).unwrap();
    let refused = copy_paste(&mut workspace, 0, 1).unwrap();
    assert!(refused.contains("KB of text"), "{refused}");
    assert_eq!(workspace.editor_mut().unwrap().script.steps.len(), 1);
}

#[test]
fn scripts_copy_in_the_order_the_pane_shows_them() {
    let context = egui::Context::default();
    let script = |id: u64, name: &str, step: u16| {
        let mut script = Script {
            id,
            name: name.into(),
            kind: ScriptKind::Script,
            ..Default::default()
        };
        script.insert_steps(0, [ScriptStep::new(step)]).unwrap();
        script
    };
    let mut workspace = ScriptWorkspace::new("File (host)".into());
    // Zeta (id 3, Beep) shows before Alpha (id 2, Select All).
    workspace.accept_catalog(Ok(ScriptCatalog {
        scripts: vec![script(3, "Zeta", 93), script(2, "Alpha", 50)],
        writable: true,
        unavailable_reason: None,
    }));
    clipboard::activate(&context, workspace.clipboard_scope, None);
    workspace.copy_scripts(&context, &[2, 3]);
    let copied = clipboard::get(&context, "Beep\nSelect All\n");
    assert_eq!(copied.iter().map(|s| s.id()).collect::<Vec<_>>(), [93, 50]);
}
