//! Scripts as the window sees them: starting them on the worker with the
//! window's state, following what they change, answering their prompts
//! (error alerts, Show Custom Dialog, confirmations, the Sort dialog, the
//! import and export dialogs), the
//! pause bar, Esc / Cmd-period, and the triggers of the user's own actions
//! (script-runtime.md 7). The runtime itself runs on the provider worker
//! (`crate::scripting`).
//!
//! A pre-event trigger of a user action (leaving a layout, committing,
//! reverting, leaving a mode or a field, a keystroke, closing the file)
//! holds the action under a token until its script ends; the worker
//! reports whether it goes on (`ScriptEvent::Action`).
//!
//! The file's own triggers come from its document preferences
//! (file-metadata.md 3): OnFirstWindowOpen and OnWindowOpen when the file's
//! window has opened, before its first layout's OnLayoutEnter, and
//! OnWindowClose then OnLastWindowClose before the user closes the file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use egui::{CornerRadius, Id, Key, RichText};
use om_data::found::FindRequest;
use om_data::runtime::{AlertChoice, DialogResponse, Mode as RunMode, TriggerEvent, TriggerInfo};
use om_data::ScriptStep;

use super::{Action, FindState, Mode, OpenMakerApp, ViewMode};
use crate::scripting::host::UI_TOKEN;

#[cfg(test)]
mod scroll_drafts;

mod keystrokes;
mod transfer;
use crate::document::{Focused, RelatedEdit, RelatedRow, SelectedRow};
use crate::scripting::{
    Answer, Cells, Prompt, RelatedDraft, ScriptEvent, ScriptJob, SessionContext, Start,
    Uncommitted, View, WindowState, WindowUpdate,
};
use crate::session::RenderState;
use crate::worker::Request;
use crate::{theme, widgets};
pub(super) use keystrokes::{KeyGate, Keys};

/// What a held trigger action does when its script lets it.
#[derive(Clone, Debug)]
pub(super) enum Held {
    /// Go on with the action from this stage of its pre-event triggers;
    /// after the last, perform it.
    Action(Action, usize),
    /// The focus left this field and the object showing it: continue
    /// its validate/save/exit triggers, restoring focus on refusal.
    Exit(
        Focused,
        bool,
        usize,
        Option<ButtonAfterExit>,
        keystrokes::KeyOwner,
        // The portal row or related field the exit left, as it began: the
        // user may leave another row of the same object meanwhile.
        Option<Box<crate::document::RelatedFocus>>,
    ),
    /// Let the keystroke through to its original editor after triggers.
    Key(Box<keystrokes::HeldKey>),
}

/// The click that caused a field to lose focus, run once the field's exit
/// triggers let it go: a button's, or the layout background's commit of
/// the record (#1066). A trigger can change the document before it ends;
/// the old click then no longer identifies a button, or the record, in the
/// current context.
#[derive(Clone, Debug)]
pub(super) struct ButtonAfterExit {
    action: Action,
    epoch: u64,
    window: u32,
    mode: Mode,
    view: ViewMode,
    found_set_revision: u64,
    layout: Option<u32>,
    record: Option<i64>,
    clicked_record: Option<i64>,
}

impl ButtonAfterExit {
    fn capture(app: &OpenMakerApp, action: Action) -> Self {
        let clicked_record = match &action {
            Action::RunButton { record_index, .. } => app
                .w
                .session
                .as_ref()
                .and_then(|s| s.record_at(*record_index).map(|(_, r)| r.id)),
            _ => None,
        };
        Self {
            // A portal click finds its row again after the exit triggers.
            action: action.held(),
            clicked_record,
            epoch: app.epoch,
            window: app.w.id,
            mode: app.w.mode,
            view: app.w.view,
            found_set_revision: app.w.found_set_revision,
            layout: app.w.session.as_ref().and_then(|s| s.layout),
            record: app
                .w
                .session
                .as_ref()
                .and_then(|s| s.current_record().map(|(_, r)| r.id)),
        }
    }

    /// The click commits the record: a click on the layout's background.
    fn commits(&self) -> bool {
        matches!(self.action, Action::CommitRecord)
    }

    fn is_current(&self, app: &OpenMakerApp) -> bool {
        self.epoch == app.epoch
            && self.window == app.w.id
            && self.mode == app.w.mode
            && self.view == app.w.view
            // A button's row is found by its place in the found set; a
            // commit is of the record, wherever the found set put it.
            && (matches!(self.action, Action::CommitRecord)
                || self.found_set_revision == app.w.found_set_revision)
            && app.w.session.as_ref().is_some_and(|s| {
                s.layout == self.layout
                    && s.current_record().map(|(_, r)| r.id) == self.record
                    && match self.action {
                        Action::RunButton { record_index, .. } => {
                            self.clicked_record.is_some()
                                && s.record_at(record_index).map(|(_, r)| r.id)
                                    == self.clicked_record
                        }
                        _ => true,
                    }
            })
    }
}

/// A prompt on screen and what the user has entered so far.
pub(super) struct PromptView {
    id: u64,
    prompt: Prompt,
    inputs: Vec<String>,
    pick: usize,
    /// The export order's selected row (the export dialog).
    order_pick: usize,
    /// The alert's text, for [`OpenMakerApp::error`].
    pub text: String,
    /// [`Prompt::SignIn`]'s dialog, the one a layout's sign-in shows.
    login: Option<crate::open_file::LoginDialog>,
    /// [`Prompt::Sort`]'s dialog: FileMaker's Sort Records.
    sort: Option<Box<crate::script_workspace::SortOrder>>,
    /// The platform's file panel answering an Insert command's
    /// [`Prompt::ContainerFile`] (`file_panel`), in place of the in-app
    /// dialog.
    panel: Option<crate::file_panel::FilePanel>,
    /// When the prompt was shown: a custom dialog's "Close after" counts
    /// from it.
    opened: std::time::Instant,
}

/// The window's script state.
#[derive(Default)]
pub(super) struct ScriptUi {
    /// A script runs: the window takes no input but Esc / Cmd-period.
    pub running: bool,
    pub owner_file: Option<u64>,
    /// Paused, with the pause's length (`None` indefinite).
    pub paused: Option<Option<f64>>,
    pub prompt: Option<PromptView>,
    /// The script `FM.RunScriptIdle` scheduled (the MBS plug-in subset),
    /// until it runs: the window whose script scheduled it, which it runs
    /// in, and the app's idle clock. One for the app, as the plug-in keeps
    /// one [I].
    pub idle: Option<(u32, crate::plugin_ui::IdleClock)>,
    /// The file's `$$` variables, for layout calculations.
    next_token: u64,
    /// Actions held for their triggers, with the window they belong to.
    held: HashMap<u64, (u32, Held)>,
    /// Held actions their trigger scripts let go on (or stopped): they
    /// resume when the job that ran the trigger ends, as FileMaker performs
    /// an event after its pre-event trigger's script; the job's final
    /// window would otherwise undo them.
    resumes: Vec<(u32, Held, bool, Option<Option<i64>>)>,
    /// The user's action applied next already passed its pre-event
    /// triggers (a trigger let it go on, it resumes after its save, or it
    /// is the save of a navigation whose OnRecordCommit ran).
    pub bypass: bool,
    /// The file's window opened and its first layout has not loaded yet:
    /// OnFirstWindowOpen and OnWindowOpen are due.
    pub window_opened: bool,
    /// Install OnTimer Script: interval and next time.
    /// Scripts the window asked for while one ran (timers, callbacks).
    queued: Vec<(u32, Start)>,
    /// The window whose record the worker saves or reverts for the user
    /// (`Start::Commit`, `Start::Revert`) until the job ends.
    saving: Option<u32>,
    saving_record: Option<(u32, i64)>,
    /// Exit Application asked to quit: when its script has ended, every
    /// window of the file closes as Quit closes them.
    quit: bool,
    /// A close trigger's Close File or Exit Application took its close over
    /// (`take_over_close`): the close goes on once the script has ended.
    taken_over: bool,
    /// The file a script's own Close File closes once the script has ended
    /// (`close_after_script`).
    close_file: Option<u64>,
    /// What started each script sent to the worker, oldest first and at
    /// most [`STARTED_KEPT`]: for measures that run a file's scripts and
    /// triggers (`OpenMakerApp::scripts_started`).
    pub started: Vec<String>,
}

/// How many starts [`ScriptUi::started`] keeps.
const STARTED_KEPT: usize = 4096;

/// A start as [`ScriptUi::started`] records it: `Script 12`, `Button
/// Perform Script`, `LayoutEnter 7` (a trigger's event and script id), ...
fn described(start: &Start) -> String {
    match start {
        Start::Script { id } => format!("Script {id}"),
        Start::Button { step, .. } => format!("Button {}", step.name()),
        Start::Trigger { event, script, .. } => format!("{event:?} {script}"),
        Start::WindowTransaction { script, .. } => format!("WindowTransaction {script}"),
        Start::Timer => "Timer".into(),
        Start::Idle => "Idle".into(),
        Start::Callback { .. } => "Callback".into(),
        Start::Commit => "Commit".into(),
        Start::Revert => "Revert".into(),
        Start::WebViewer { .. } => "WebViewer".into(),
    }
}

impl ScriptUi {
    /// The window takes no input from the user.
    pub fn busy(&self) -> bool {
        self.running && self.paused.is_none()
    }

    /// What a script shows and waits on, if anything: the prompt's kind.
    pub fn prompt_kind(&self) -> Option<&'static str> {
        use crate::scripting::Prompt;
        Some(match &self.prompt.as_ref()?.prompt {
            Prompt::Alert { .. } => "alert",
            Prompt::Dialog(_) => "custom dialog",
            Prompt::PluginDialog { .. } => "plug-in alert",
            Prompt::SignIn { .. } => "sign-in",
            Prompt::Confirm { .. } => "confirmation",
            Prompt::Sort { .. } => "sort dialog",
            Prompt::Index { .. } => "index dialog",
            Prompt::Export(_) => "export dialog",
            Prompt::Pdf(_) => "PDF dialog",
            Prompt::PageSetup(_) => "page setup",
            Prompt::Print(_) => "print dialog",
            Prompt::Import(_) => "import dialog",
            Prompt::ImportSource(_) => "import source",
            Prompt::ContainerFile(_) => "file dialog",
            Prompt::ImportSummary { .. } => "import summary",
        })
    }

    /// The text of an error alert on screen.
    pub fn alert_text(&self) -> Option<&str> {
        match self.prompt.as_ref()? {
            PromptView {
                prompt: Prompt::Alert { .. },
                text,
                ..
            } => Some(text),
            _ => None,
        }
    }
}

impl OpenMakerApp {
    /// The viewport identity, without copying record drafts, find requests
    /// or catalog data during every paint.
    pub(super) fn scroll_context(&self) -> crate::scripting::ScrollContext {
        crate::scripting::ScrollContext {
            layout: self.w.session.as_ref().and_then(|s| s.layout),
            index: self.w.session.as_ref().map_or(0, |s| s.current),
            preview_page: self
                .w
                .session
                .as_ref()
                .and_then(crate::session::Session::preview_position)
                .map_or(0, |p| p.0),
            mode: self.w.mode.runtime(),
            view: match self.w.view {
                ViewMode::Form => View::Form,
                ViewMode::List => View::List,
                ViewMode::Table => View::Table,
            },
        }
    }

    /// The window as a script starts from.
    pub(super) fn window_state(&self) -> WindowState {
        let context = self.scroll_context();
        let session = self.w.session.as_ref();
        let find = self
            .w
            .find
            .requests
            .iter()
            .map(|request| FindRequest {
                omit: request.omit,
                criteria: request
                    .criteria
                    .iter()
                    .filter(|(_, c)| !c.trim().is_empty())
                    .cloned()
                    .collect(),
            })
            .collect();
        let drafts = session
            .map(|s| self.w.document.changes(s))
            .unwrap_or_default();
        let layout = session.and_then(|s| s.layout);
        let record = session.and_then(|s| s.current_record()).map(|(_, r)| r.id);
        // What scripts left on the record. While the record's page loads
        // it goes with its record, which the worker checks: never another
        // record's.
        let (record, uncommitted) = match &self.w.uncommitted {
            Some((l, r, open)) if Some(*l) == layout && record.is_none_or(|id| id == *r) => {
                (Some(*r), open.clone())
            }
            _ => (record, Uncommitted::default()),
        };
        WindowState {
            id: self.w.id,
            hidden: self.w.hidden,
            layout,
            popovers: self.w.document.popovers().clone(),
            panels: self.w.document.panels.clone(),
            index: session.map_or(0, |s| s.current),
            preview_page: session
                .and_then(crate::session::Session::preview_position)
                .map_or(0, |p| p.0),
            mode: context.mode,
            view: context.view,
            views: self
                .w
                .views
                .iter()
                .map(|(l, v)| (*l, (*v).into()))
                .collect(),
            find,
            find_index: self.w.find.current,
            record,
            related_versions: self
                .w
                .calcs
                .cache
                .borrow()
                .records
                .iter()
                .filter_map(|(key, read)| {
                    read.as_ref()
                        .filter(|r| !r.no_access && !r.prepared_intent)
                        .and_then(|r| r.mod_count)
                        .map(|version| (*key, version))
                })
                .collect(),
            record_version: session.and_then(|s| {
                let id = record?;
                s.loaded_pages().find_map(|page| {
                    (Some(page.key.layout_id) == layout)
                        .then(|| page.data.versions.get(&id).copied())
                        .flatten()
                })
            }),
            // The user's own typing counts once the field is left. A
            // selected container is not typed into.
            typing: self.w.document.active.clone().filter(|f| {
                drafts.contains_key(f)
                    && session
                        .and_then(|s| self.w.document.selected_container(s))
                        .is_none_or(|c| c.field != *f)
            }),
            drafts,
            focus: if self.w.mode == Mode::Find {
                self.w.find.focused_field.clone()
            } else if self.w.document.active_related.is_some() {
                // A related field answers through its portal row (below).
                None
            } else {
                self.w.document.active.clone()
            },
            field_focus: false,
            object: self.w.document.active_object,
            numeric_focus: self
                .w
                .document
                .active_related
                .map(|focus| (focus.object, focus.repetition)),
            select: false,
            // Find mode has no portal rows, so no active one. The row of
            // the related field the user is in is.
            portal: session
                .filter(|_| self.w.mode != Mode::Find)
                .and_then(|s| Some((s.layout?, s.current_record()?.1.id)))
                .and_then(|(layout, record)| {
                    self.focused_portal_row().or_else(|| {
                        self.w
                            .portal
                            .filter(|(l, r, _)| *l == layout && *r == record)
                            .map(|(_, _, row)| row)
                    })
                }),
            uncommitted,
            title: self.w.title.clone(),
            toolbar: self.w.show_toolbar,
            menubar: self.w.menubar,
            zoom: self.w.document.zoom.percent(),
            zoom_locked: self.w.zoom_locked,
            // The user's selection in the field being typed in.
            selection: (self.w.mode == Mode::Browse)
                .then_some(self.w.document.active.as_deref())
                .flatten()
                .filter(|_| self.w.document.active_related.is_none())
                .and_then(|field| self.w.document.selection(field)),
            requests: self.w.requests,
            sorted: self.w.found_set.is_some_and(|f| f.sorted),
            // Where the window was last drawn (each window's own).
            placement: self.w.placement.clone(),
            uuid: self.w.uuid.clone(),
            modifiers: 0,
            style: self.w.style,
            hidden_named: self.w.session.as_ref().is_some_and(|s| s.hidden_named),
        }
    }

    /// What `Get` reads of the window's focus, edits, find requests and
    /// sort in the layout's calculations: the answers a script
    /// started now reads (`WindowHost::session_info` over
    /// [`Self::window_state`]), without its reads from the provider. The
    /// active field's text is what its editor holds, else the record's
    /// stored text; a related field's editor not drawn yet holds nothing.
    pub(super) fn window_get_state(
        &self,
        session: &crate::session::Session,
        schema: Option<&om_data::host::CalcSchema>,
        info: &mut om_data::host::SessionInfo,
    ) {
        let find = self.w.mode == Mode::Find;
        let document = &self.w.document;
        let current = session.current_record().map(|(_, record)| record.id);
        info.sorted = self.w.found_set.is_some_and(|f| f.sorted);
        info.request_count = if find {
            self.w.find.len() as u64
        } else {
            self.w.requests
        };
        info.request_omit = find && self.w.find.current().omit;
        let objects = match &session.render {
            RenderState::Loaded(loaded) => Some(&loaded.layout.objects),
            _ => None,
        };
        let object_name = |id: u64| {
            objects
                .and_then(|o| o.get(id))
                .map(|o| o.name.clone().unwrap_or_default())
        };
        // Find mode has no portal rows. Otherwise the row of the related
        // field the user is in, else the row last clicked on this record.
        let related = document.active_related.filter(|_| !find);
        let portal = match related {
            _ if find => None,
            Some(focus) if focus.portal.is_some() => focus.row.map(|row| (focus.object, row)),
            _ => self
                .w
                .portal
                .filter(|(l, r, _)| Some(*l) == session.layout && Some(*r) == current)
                .map(|(_, _, row)| (row.field.unwrap_or(row.object), row.row)),
        };
        info.active_portal_row_number = Some(portal.map_or(0, |(_, row)| row));
        if find {
            if let Some(field) = &self.w.find.focused_field {
                (info.active_field_name, info.active_field_table_name) =
                    crate::calc::focus_names(field, &info.layout_table_name);
                info.active_field_contents = self
                    .w
                    .find
                    .current()
                    .get(field)
                    .unwrap_or_default()
                    .to_owned();
            }
        } else if let Some(focus) = related {
            // A portal row's field, a related field outside portals, or a
            // later repetition of the record's own field.
            let names = objects
                .and_then(|o| o.get(focus.object)?.field_spec())
                .zip(schema)
                .and_then(|(spec, schema)| {
                    let occurrence = om_layout::occurrence_id(spec.table_key)?;
                    Some((
                        schema.field(occurrence, spec.field_id()?)?.name.clone(),
                        schema.occurrence(occurrence)?.name.clone(),
                    ))
                });
            if let Some((name, table)) = names {
                info.active_field_name = name;
                info.active_field_table_name = table;
            }
            info.active_repetition_number = Some(u64::from(focus.repetition.max(1)));
            info.active_field_contents = document
                .related_text(&focus.field)
                .unwrap_or_default()
                .to_owned();
            if focus.portal.is_some() {
                info.active_layout_object = object_name(focus.object);
            }
        } else if let Some(name) = &document.active {
            info.active_field_name = name.clone();
            info.active_field_table_name = info.layout_table_name.clone();
            info.active_field_contents = document.value(session, name).unwrap_or_default();
        } else if let Some((object, _)) = portal {
            info.active_layout_object = object_name(object);
        }
        // The record's open state and changed fields.
        let uncommitted = self.uncommitted();
        let drafts = document.changes(session);
        let new = uncommitted.is_some_and(|u| u.new);
        info.record_open_state = Some(if find {
            0
        } else if new {
            1
        } else if !drafts.is_empty()
            || document.related_edited(session)
            || uncommitted.is_some_and(|u| !u.cells.is_empty())
        {
            2
        } else {
            0
        });
        let table = crate::calc::layout_occurrence(session)
            .zip(schema)
            .and_then(|(occurrence, schema)| schema.table_of(occurrence))
            .map(|table| table.id);
        let (Some(table), Some(record), Mode::Browse) = (table, current, self.w.mode) else {
            return;
        };
        // The user's typing counts once the field is left. A selected
        // container is not typed into.
        let typing = document.active.clone().filter(|f| {
            drafts.contains_key(f)
                && document
                    .selected_container(session)
                    .is_none_or(|c| c.field != *f)
        });
        // The record's own later repetitions: a script's cells, and the
        // user's typing into them but for the one being typed into, a
        // cell of this record (another related record's cell of the same
        // id is not it).
        let parent = u64::try_from(record).ok();
        let focused = related
            .filter(|f| {
                f.portal.is_none()
                    && f.field.table == table
                    && parent.is_some_and(|p| f.field.row == RelatedRow::Record(p))
            })
            .map(|f| f.field.field);
        let scripted = uncommitted
            .into_iter()
            .flat_map(|u| u.cells.keys().copied());
        let drafted = uncommitted.into_iter().flat_map(|u| {
            u.related
                .iter()
                .filter(|d| d.table == table && d.record == parent && !d.deleted)
                .flat_map(|d| d.cells.keys().copied())
        });
        let typed = document
            .related_edits(session.layout.unwrap_or_default(), record)
            .into_iter()
            .filter(|edit| {
                edit.field.table == table
                    && parent.is_some_and(|p| edit.field.row == RelatedRow::Record(p))
            })
            .map(|edit| edit.field.field)
            .chain(drafted)
            .filter(|cell| Some(*cell) != focused);
        let entered = match uncommitted {
            Some(u) if u.new => u.entered.clone(),
            _ => Vec::new(),
        };
        info.modified_fields = self.w.calcs.modified_fields(crate::calc::ModifiedInputs {
            table,
            drafts: drafts.into_keys().collect(),
            typing,
            cells: scripted.chain(typed).collect(),
            entered,
        });
    }

    /// The portal row of the related field the user's focus is in, that
    /// field active in it (#1005).
    fn focused_portal_row(&self) -> Option<crate::scripting::PortalRow> {
        let focus = self.w.document.active_related?;
        let identity = match focus.field.row {
            crate::document::RelatedRow::Record(record) => self.row_identity(record, None),
            // The creation row's editor names the record typing into it
            // begins: that draft once there is one (the user typed), else
            // the creation row.
            crate::document::RelatedRow::New { id, .. } => self
                .row_identity(0, Some(super::UnsavedRow::Begun(id)))
                .or(Some(crate::scripting::PortalIdentity::Creation)),
        };
        Some(crate::scripting::PortalRow {
            object: focus.portal?,
            occurrence: focus.occurrence,
            row: focus.row?,
            identity,
            // The row painted for that record: its identity holds in a
            // portal that sorts or filters its rows too (#1027).
            clicked: true,
            field: Some(focus.object),
            repetition: focus.repetition,
        })
    }

    /// What scripts left on the current record that no commit has kept.
    pub(super) fn uncommitted(&self) -> Option<&Uncommitted> {
        let session = self.w.session.as_ref()?;
        let record = session.current_record()?.1.id;
        self.w
            .uncommitted
            .as_ref()
            .filter(|(l, r, _)| Some(*l) == session.layout && *r == record)
            .map(|(_, _, open)| open)
    }

    /// Put the user's edits of related records made through the current
    /// record (`DocumentState::related_edits`) with what the record keeps
    /// uncommitted, where the worker's commit and revert find them beside
    /// what scripts left: a field the user typed holds the typed value,
    /// one the user typed back to its stored value goes. A record's
    /// related records are committed with it (script-runtime.md 13.24).
    pub(super) fn sync_related_edits(&mut self) {
        let Some(session) = &self.w.session else {
            return;
        };
        let (Some(layout), Some((_, record))) = (session.layout, session.current_record()) else {
            return;
        };
        let parent = record.id;
        if self
            .w
            .uncommitted
            .as_ref()
            .is_some_and(|(l, r, _)| (*l, *r) != (layout, parent))
        {
            // Another record's (a held save): not this record's to change.
            return;
        }
        let edits = self.w.document.related_edits(layout, parent);
        if edits.is_empty() && self.w.uncommitted.is_none() {
            return;
        }
        let mut open = self
            .w
            .uncommitted
            .take()
            .map(|(_, _, open)| open)
            .unwrap_or_default();
        for draft in &mut open.related {
            let Some(record) = draft.record else {
                continue;
            };
            let (table, cells) = (draft.table, &mut draft.cells);
            draft.typed.retain(|cell| {
                let kept = edits.iter().any(|edit| {
                    (edit.field.table, edit.field.row, edit.field.field)
                        == (table, RelatedRow::Record(record), *cell)
                });
                if !kept {
                    cells.remove(cell);
                }
                kept
            });
        }
        // Records begun in creation rows, by the window's id for each: one
        // record to create apiece, holding what the user typed.
        let mut rows: BTreeMap<(u64, u64), Vec<&RelatedEdit>> = BTreeMap::new();
        for edit in &edits {
            if let RelatedRow::New { occurrence, id } = edit.field.row {
                rows.entry((occurrence, id)).or_default().push(edit);
            }
        }
        for draft in open.related.iter_mut().filter(|d| d.begun.is_some()) {
            // What the user no longer has typed goes; what a step set stays
            // (a record no field of which is left goes, below).
            let edits = draft
                .begun
                .and_then(|id| rows.get(&(draft.occurrence, id)))
                .map(Vec::as_slice)
                .unwrap_or_default();
            for cell in draft.typed.clone() {
                if !edits.iter().any(|edit| edit.field.field == cell) {
                    draft.typed.remove(&cell);
                    draft.cells.remove(&cell);
                }
            }
        }
        for ((occurrence, id), edits) in &rows {
            let at = match open
                .related
                .iter()
                .position(|d| d.begun == Some(*id) && d.occurrence == *occurrence)
            {
                Some(at) => at,
                None => {
                    open.related.push(RelatedDraft {
                        key: crate::scripting::DraftKey::fresh(),
                        reserved: None,
                        matched_cells: Default::default(),
                        occurrence: *occurrence,
                        table: edits[0].field.table,
                        record: None,
                        cells: Cells::new(),
                        typed: BTreeSet::new(),
                        version: None,
                        begun: Some(*id),
                        matched: false,
                        creation_parent: None,
                        deleted: false,
                        saved: BTreeMap::new(),
                        prepared: None,
                    });
                    open.related.len() - 1
                }
            };
            let draft = &mut open.related[at];
            for edit in edits {
                let value = (!edit.value.is_empty()).then(|| edit.value.clone());
                draft.cells.insert(edit.field.field, value);
                draft.typed.insert(edit.field.field);
            }
        }
        for edit in edits {
            let RelatedRow::Record(record) = edit.field.row else {
                continue;
            };
            let (table, cell) = (edit.field.table, edit.field.field);
            let value = (!edit.value.is_empty()).then_some(edit.value);
            match open
                .related
                .iter_mut()
                .find(|d| d.table == table && d.record == Some(record))
            {
                // A script deleted the record (Delete Portal Row).
                Some(draft) if draft.deleted => {}
                Some(draft) => {
                    draft.cells.insert(cell, value);
                    draft.typed.insert(cell);
                    draft.version = draft.version.or(edit.version);
                }
                None => open.related.push(RelatedDraft {
                    key: crate::scripting::DraftKey::fresh(),
                    reserved: None,
                    matched_cells: Default::default(),
                    occurrence: edit.occurrence,
                    table,
                    record: Some(record),
                    cells: [(cell, value)].into(),
                    typed: [cell].into(),
                    version: edit.version,
                    begun: None,
                    matched: false,
                    creation_parent: None,
                    deleted: false,
                    saved: BTreeMap::new(),
                    prepared: None,
                }),
            }
        }
        // A script's records to create always hold their match fields.
        open.related.retain(|d| d.deleted || !d.cells.is_empty());
        self.w.uncommitted = (!open.is_empty()).then_some((layout, parent, open));
    }

    /// The portal row the user selected on the current record, if any.
    pub(super) fn selected_portal_row(&self) -> Option<SelectedRow> {
        let session = self.w.session.as_ref()?;
        let parent = session.current_record()?.1.id;
        let layout = u64::from(session.layout?);
        self.w
            .document
            .selected_row
            .filter(|row| (row.layout, row.parent) == (layout, parent))
    }

    /// Whether the account may change the file's design (File > Manage,
    /// Layout mode, Script Workspace): every set but the built-in [Data
    /// Entry Only] and [Read-Only Access], whose layouts are view only,
    /// scripts executable only and which may not manage the database
    /// (session-identity.md 2.2, 2.3). A set the host did not verify as
    /// built in is left to the host, which refuses design changes it does
    /// not allow.
    pub(super) fn design_allowed(&self) -> bool {
        self.w
            .calcs
            .schema()
            .and_then(|schema| schema.authenticated_account.set_access.as_ref())
            .is_none_or(om_data::host::security::SetAccess::changes_design)
    }

    /// The actions that change or open the file's design.
    pub(super) fn changes_design(action: &Action) -> bool {
        matches!(
            action,
            Action::ManageDatabase
                | Action::ManageSecurity
                | Action::ManageValueLists
                | Action::ManageLayouts
                | Action::NewLayout
                | Action::CreateLayout(_)
                | Action::DuplicateLayout(_)
                | Action::DeleteLayout(_)
                | Action::LayoutSetup
                | Action::SetUpLayout(_)
                | Action::ConfirmDeleteLayout
                | Action::SaveLayout
                | Action::DeleteLayoutObject
                | Action::ScriptWorkspace
                | Action::SetMode(Mode::Layout)
        )
    }

    /// Whether Delete Record is available: with a portal row selected,
    /// when the portal allows deleting its records (`FMWindowModel`'s
    /// check, layout-model.md 5.3.2), else when the layout's table does.
    pub(super) fn delete_enabled(&self) -> bool {
        !self.w.document.pending
            && match self.selected_portal_row() {
                Some(row) => row.deletes,
                None => self.w.record_capabilities.delete,
            }
    }

    /// Delete Record on portal row `row`: its record is deleted with the
    /// current record's next commit, as Delete Portal Row deletes it
    /// (script-runtime.md 13.24), and Revert Record brings it back; the
    /// user's edits of it go with it.
    pub(super) fn delete_portal_row(&mut self, row: SelectedRow) {
        let Some(session) = &self.w.session else {
            return;
        };
        let Some(layout) = session.layout else {
            return;
        };
        if self.selected_portal_row() != Some(row) {
            return;
        }
        self.sync_related_edits();
        if self
            .w
            .uncommitted
            .as_ref()
            .is_some_and(|(l, r, _)| (*l, *r) != (layout, row.parent))
        {
            return;
        }
        let mut open = self
            .w
            .uncommitted
            .take()
            .map(|(_, _, open)| open)
            .unwrap_or_default();
        open.related
            .retain(|d| !(d.table == row.table && d.record == Some(row.record)));
        open.related.push(RelatedDraft {
            key: crate::scripting::DraftKey::fresh(),
            reserved: None,
            matched_cells: Default::default(),
            occurrence: row.occurrence,
            table: row.table,
            record: Some(row.record),
            cells: Cells::new(),
            typed: BTreeSet::new(),
            version: None,
            begun: None,
            matched: false,
            creation_parent: None,
            deleted: true,
            saved: BTreeMap::new(),
            prepared: None,
        });
        self.w.uncommitted = Some((layout, row.parent, open));
        self.w.document.retain_related(|field| {
            !(field.table == row.table && field.row == RelatedRow::Record(row.record))
        });
        self.w.document.selected_row = None;
    }

    /// Whether the current record has unsaved changes: the user's, or
    /// what scripts left on it.
    pub(super) fn record_modified(&self) -> bool {
        self.w
            .session
            .as_ref()
            .is_some_and(|s| !self.w.document.changes(s).is_empty())
            || self.uncommitted().is_some()
    }

    /// The user saves (`Start::Commit`) or reverts (`Start::Revert`) a
    /// record scripts left changes on: the worker does it, with those
    /// changes; the window waits as for its own save.
    pub(super) fn save_on_worker(&mut self, ctx: &egui::Context, start: Start) {
        self.w.document.pending = true;
        self.w.in_flight = Some(super::recovery::Write::Scripted);
        self.start_script(ctx, start);
    }

    /// The worker's save or revert ended (with its script job): a
    /// navigation the save waited for goes on when the record kept
    /// nothing unsaved; after a failure (its message shown) it is
    /// dropped.
    fn saved_on_worker(&mut self, ctx: &egui::Context) {
        let Some(id) = self.script.saving.take() else {
            return;
        };
        let saved_record = self.script.saving_record.take();
        self.with_window(id, |app| {
            app.w.document.pending = false;
            app.w.in_flight = None;
            let refusal = app.w.worker_save_failure.take();
            let deferred = app.w.deferred_action.take();
            if app.record_modified() {
                // The save failed: the action it held goes (closing the
                // file stops here); a later one runs its own triggers.
                app.closing = None;
                // Saved in some files but not all: the provider keeps it,
                // and the window shows it frozen until it is settled (#1006).
                if let Some((_, _, error)) = &refusal {
                    let partly = error
                        .commit
                        .iter()
                        .any(|file| file.outcome != om_data::FileOutcome::NotSubmitted);
                    if partly {
                        app.w.unsettled = Some(error.commit.clone());
                        app.refresh_capabilities(ctx);
                    }
                }
                if let Some((layout, record, error)) = refusal {
                    if app.w.retained.contains_key(&(layout, record))
                        && app.recheck_refused_save(ctx, layout, record, &error)
                    {
                        app.error = None;
                    }
                }
                return;
            }
            if let Some((layout, record)) = saved_record {
                app.recovered_record_saved(ctx, layout, record);
            }
            // The commit's page refresh fulfills a deferred Refresh Window.
            if let Some(action) = deferred.filter(|a| *a != Action::Refresh) {
                app.resume_saved_action(ctx, action);
            }
        });
    }

    /// Continue the action a successful save held. Its pre-event triggers
    /// (OnRecordCommit, OnLayoutExit, OnModeExit) already let it go on
    /// before the save; they do not run again.
    pub(super) fn resume_saved_action(&mut self, ctx: &egui::Context, action: Action) {
        self.script.bypass = true;
        self.apply(ctx, action);
        self.script.bypass = false;
    }

    /// Where the window is on the desktop, as it was last drawn (in its
    /// own viewport), else as the current viewport reports it.
    pub(super) fn placement(&self, ctx: &egui::Context) -> crate::desktop::Placement {
        self.w.placement.clone().unwrap_or_else(|| {
            crate::desktop::placement(
                ctx,
                self.w.content_rect,
                self.w.show_toolbar,
                self.w.bars,
                self.w.menu_drawn,
                self.w.menu_height,
            )
        })
    }

    fn session_context(&self) -> SessionContext {
        let target = self.w.session.as_ref().map(|s| &s.target);
        SessionContext {
            host: target.map(|t| t.host.clone()).unwrap_or_default(),
            file: target.map(|t| t.file.clone()).unwrap_or_default(),
            account: self.w.calcs.account.clone(),
            local: target.and_then(|t| t.local.clone()),
        }
    }

    /// Run a script on the worker. Nothing starts once the connection to
    /// the host is lost: the worker would never get the job, and the window
    /// would wait for a script that does not run.
    pub(super) fn start_script(&mut self, ctx: &egui::Context, start: Start) {
        if self.w.session.is_none() || self.w.connection_lost.is_some() {
            return;
        }
        if self.script.busy() {
            // Timers and callbacks wait for the running script.
            self.script.queued.push((self.w.id, start));
            return;
        }
        // A queued save has not started yet. Only this job's completion
        // may finish it, never the preceding trigger's Finished event.
        if matches!(&start, Start::Commit | Start::Revert) {
            self.script.saving = Some(self.w.id);
            self.script.saving_record = self
                .w
                .uncommitted
                .as_ref()
                .map(|(layout, record, _)| (*layout, *record))
                .or_else(|| {
                    let session = self.w.session.as_ref()?;
                    Some((session.layout?, session.current_record()?.1.id))
                });
            self.w.worker_save_failure = None;
        }
        self.start_script_now(ctx, start);
    }

    /// Send the job to the worker at once, also while a script runs: a
    /// web viewer's `FileMaker.PerformScript` applies its option to the
    /// running script there (the worker queues it between steps).
    pub(super) fn start_script_now(&mut self, ctx: &egui::Context, start: Start) {
        if self.w.session.is_none() || self.w.connection_lost.is_some() {
            return;
        }
        // The menus' New Record focus is that job's alone (`Action::NewRecord`).
        self.w.new_record_focus = false;
        // Every dispatch path, including focus triggers and callbacks,
        // queues its accepted draft versions before the job carrying edits.
        self.retain_drafts(ctx);
        self.sync_related_edits();
        let mut window = self.window_state();
        if matches!(&start, Start::Revert) {
            if let Some((layout, record, open)) =
                self.w.uncommitted.as_ref().filter(|(layout, record, _)| {
                    self.w.recovery.as_ref().is_some_and(|r| {
                        r.records
                            .get(&(*layout, *record))
                            .is_some_and(|c| c.hold.is_some())
                    })
                })
            {
                // Discard still belongs to the held record if a peer
                // deleted it and the visible page moved to another row.
                window.layout = Some(*layout);
                window.record = Some(*record);
                window.uncommitted = open.clone();
                window.drafts = self
                    .w
                    .document
                    .drafts()
                    .remove(&(*layout, *record))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|d| (d.name, (!d.value.is_empty()).then_some(d.value)))
                    .collect();
            }
        }
        window.placement = Some(self.placement(ctx));
        window.modifiers = ctx.input(|i| modifier_keys(i.modifiers));
        if self.script.started.len() < STARTED_KEPT {
            self.script.started.push(described(&start));
        }
        let job = ScriptJob {
            start,
            window,
            others: self.other_window_states(),
            context: self.session_context(),
        };
        if !self.script.running {
            self.script.owner_file = Some(self.w.file_id);
        }
        self.script.running = true;
        // A job started during a pause runs on top of the paused script:
        // the window takes no input (and queues further jobs) until it
        // ends or pauses again.
        self.script.paused = None;
        self.request(ctx, Request::Script(Box::new(job)));
    }

    /// The next job queued while a script ran, in its own window, now that
    /// the window takes jobs again (the script ended, or paused: a queued
    /// save or trigger runs on top of the paused script). A job whose
    /// window closed meanwhile goes with it.
    fn start_queued(&mut self, ctx: &egui::Context) {
        while !self.script.busy() && !self.script.queued.is_empty() {
            let (id, start) = self.script.queued.remove(0);
            self.with_window(id, |app| app.start_script(ctx, start));
        }
    }

    /// The menus' New Record or Duplicate Record: step `id` with the
    /// desktop's first-field focus once it ends (`new_record_focus`), when
    /// it starts now; one queued behind a running script keeps the step's.
    pub(super) fn run_new_record_step(&mut self, ctx: &egui::Context, id: u16) {
        let queued = self.script.queued.len();
        self.run_step(ctx, id, 0);
        self.w.new_record_focus = self.script.queued.len() == queued && self.script.busy();
    }

    /// The field New Record/Request puts the focus in: the first field
    /// object of the layout's stored tab order, of the window's table, as
    /// the step picks it (scripting `focus_first_field`).
    fn first_tab_field(&self) -> Option<String> {
        let RenderState::Loaded(render) = &self.w.session.as_ref()?.render else {
            return None;
        };
        let alias = render.layout.def.as_ref().map_or(0, |d| d.table_alias_key);
        om_data::host::layout_objects::first_in_tab_order(&render.layout)
            .filter(|o| o.field_spec().is_some_and(|s| s.table_key == alias))
            .and_then(|o| render.field_bindings.get(&o.id).cloned())
    }

    /// A single step through the script engine (menu commands, the
    /// Find mode buttons).
    pub(super) fn run_step(&mut self, ctx: &egui::Context, id: u16, flags: u32) {
        use super::steps::{INSERT_AUDIO_VIDEO, INSERT_FILE, INSERT_PDF, INSERT_PICTURE};
        // Relookup's menu item is checked again here: text typed in the
        // frame of the click, or an edit the step was held behind, makes
        // the record one with something to commit, which the step would
        // commit past the field's triggers (#624). It then does nothing.
        if id == om_data::scripts::relookup::STEP_ID && !self.relookup_available() {
            return;
        }
        // The Insert menu fills the selected container only while its
        // object still takes entry (and PDF and Audio/Video only an
        // interactive one), as when the menu offered it.
        if matches!(
            id,
            INSERT_PICTURE | INSERT_FILE | INSERT_PDF | INSERT_AUDIO_VIDEO
        ) {
            let fillable = self.w.session.as_ref().is_some_and(|session| {
                self.w
                    .document
                    .selected_container(session)
                    .is_some_and(|selection| {
                        selection.allows_entry(session)
                            && (matches!(id, INSERT_PICTURE | INSERT_FILE)
                                || selection.is_interactive(session))
                    })
            });
            if !fillable {
                return;
            }
        }
        let step = ScriptStep {
            opcode: id as i16,
            flags,
            ..Default::default()
        };
        // A related container selected in a portal row: the step runs with
        // that row active and its field focused (the Insert menu fills it).
        let row = self.w.session.as_ref().and_then(|session| {
            let selection = self.w.document.selected_container(session)?;
            let row = selection.row?;
            Some(crate::scripting::PortalRow {
                object: row.portal,
                occurrence: row.occurrence,
                row: row.number as u64,
                identity: self.row_identity(row.record, row.unsaved),
                clicked: false,
                field: Some(selection.object),
                repetition: 1,
            })
        });
        if let Some(row) = row {
            self.start_script(
                ctx,
                Start::Button {
                    step: Box::new(step),
                    formula: None,
                    literal: String::new(),
                    portal: Some(row),
                },
            );
            return;
        }
        self.run_command(ctx, step);
    }

    /// The toolbar's Quick Find: Perform Quick Find (150) with `text`, as a
    /// button runs it, so a no-match reports as the step does.
    pub(super) fn quick_find(&mut self, ctx: &egui::Context, text: String) {
        use om_data::formula::{polish, Expr, Text};
        let mut step = ScriptStep::new(super::steps::PERFORM_QUICK_FIND);
        let literal = polish::encode(&Expr::Text(Text::plain(text)));
        step.set_calculation(0, Some(&om_data::Calculation::new(None, literal)));
        self.run_command(ctx, step);
    }

    /// Run a command that is one script step, as a button runs it.
    pub(super) fn run_command(&mut self, ctx: &egui::Context, step: ScriptStep) {
        self.start_script(
            ctx,
            Start::Button {
                step: Box::new(step),
                formula: None,
                literal: String::new(),
                portal: None,
            },
        );
    }

    // ----- triggers of the user's actions ---------------------------------------

    /// The layout's trigger record for `event` (or the object's); a file
    /// event's from the file's document preferences.
    fn trigger_record(
        &self,
        event: TriggerEvent,
        object: Option<u64>,
    ) -> Option<om_layout::ScriptTrigger> {
        if object.is_none() {
            if let Some(trigger) = self.file_trigger(event) {
                return Some(trigger);
            }
        }
        let RenderState::Loaded(render) = &self.w.session.as_ref()?.render else {
            return None;
        };
        match object {
            Some(object) => {
                crate::scripting::object_trigger(&render.layout, object, event).cloned()
            }
            None => crate::scripting::layout_trigger(&render.layout, event).cloned(),
        }
    }

    /// A file trigger as a trigger record: the script of `event` in the
    /// file's document preferences, in this file, in every mode, without a
    /// parameter.
    fn file_trigger(&self, event: TriggerEvent) -> Option<om_layout::ScriptTrigger> {
        let options = self.w.calcs.schema()?.file_options.as_ref()?;
        Some(om_layout::ScriptTrigger {
            event: event.id(),
            script: Some(options.trigger(event.id())?),
            file_reference: None,
            external: false,
            modes: 7,
            option: 0,
            extra: Vec::new(),
            parameter: None,
        })
    }

    /// The layout object that shows `field`.
    fn field_object(&self, field: &str) -> Option<u64> {
        let RenderState::Loaded(render) = &self.w.session.as_ref()?.render else {
            return None;
        };
        crate::scripting::field_object(render, field)
    }

    /// The object a focus change names: the one painted with the focus,
    /// else the first showing its field. Duplicate objects of one field
    /// have their own triggers.
    fn focused_object(&self, (field, object): &Focused) -> Option<u64> {
        object.or_else(|| self.field_object(field))
    }

    /// When `object` is a checkbox or radio button set, the layout and
    /// current record its keystrokes go to.
    fn option_set_keys(&self, object: u64) -> Option<(u64, i64)> {
        let session = self.w.session.as_ref()?;
        let RenderState::Loaded(render) = &session.render else {
            return None;
        };
        render
            .layout
            .objects
            .get(object)?
            .field_obj
            .as_ref()
            .filter(|field| matches!(field.display_type, 3 | 4))?;
        Some((render.layout.id, session.current_record()?.1.id))
    }

    /// Fire `event`'s trigger, holding `held` until its script ends.
    /// Returns whether a trigger fired. None fires once the connection to
    /// the host is lost: what the window still does then happens at once.
    pub(super) fn fire(
        &mut self,
        ctx: &egui::Context,
        event: TriggerEvent,
        object: Option<u64>,
        held: Option<Held>,
        info: TriggerInfo,
    ) -> bool {
        self.fire_with(ctx, event, object, held, info, false)
    }

    /// [`Self::fire`]; `prepare`: the records begun in portals' creation
    /// rows are previewed before the script reads them (an ordinary
    /// commit's OnObjectSave, `Start::Trigger::prepare`, #1047).
    pub(super) fn fire_with(
        &mut self,
        ctx: &egui::Context,
        event: TriggerEvent,
        object: Option<u64>,
        held: Option<Held>,
        info: TriggerInfo,
        prepare: bool,
    ) -> bool {
        // Nor while a reconnect holds edits: a script could save them.
        if self.w.connection_lost.is_some() || self.recovery_holds() {
            return false;
        }
        let Some(record) = self.trigger_record(event, object) else {
            return false;
        };
        let Some(script) = record.script else {
            return false;
        };
        let action = held.map(|held| {
            self.script.next_token += 1;
            let token = UI_TOKEN | self.script.next_token;
            self.script.held.insert(token, (self.w.id, held));
            token
        });
        // A related field the user is in keeps the focus through its
        // trigger's script (`hold_related_focus`).
        if matches!(
            event,
            TriggerEvent::ObjectEnter | TriggerEvent::ObjectModify
        ) {
            self.w.document.hold_related_focus();
        }
        self.start_script(
            ctx,
            Start::Trigger {
                event,
                script,
                file_reference: record.file_reference,
                parameter: record
                    .parameter
                    .as_ref()
                    .map(crate::scripting::host::parameter),
                modes: record.modes,
                action,
                info: Box::new(info),
                prepare,
            },
        );
        true
    }

    /// Start the OnWindowTransaction of the user's commit, if the file has
    /// one ([`Self::window_transaction`]); it waits for a running script.
    pub(super) fn start_transaction(&mut self, ctx: &egui::Context, transaction: Option<Start>) {
        if let Some(start) = transaction {
            self.start_script(ctx, start);
        }
    }

    /// Whether every field of `values` is a global field of the window's
    /// table: typing into one sets the session's value, not the record's.
    pub(super) fn only_globals(&self, values: &om_data::FieldChanges) -> bool {
        let Some(session) = &self.w.session else {
            return false;
        };
        let Some(schema) = self.w.calcs.schema() else {
            return false;
        };
        let Some(table) = crate::calc::layout_occurrence(session).and_then(|o| schema.table_of(o))
        else {
            return false;
        };
        values
            .keys()
            .all(|name| table.fields.values().any(|f| f.global && f.name == *name))
    }

    /// The user committed (created, changed or deleted) `record` of
    /// `layout`'s table: the file's OnWindowTransaction, to run with the
    /// record (script-runtime.md 13.26). `operation` is New, Modified or
    /// Deleted. The caller starts it after the action's own follow-up (a
    /// navigation the save held, its delayed triggers), so it does not
    /// keep them from running.
    pub(super) fn window_transaction(
        &self,
        layout: u32,
        operation: &str,
        record: &om_data::Record,
    ) -> Option<Start> {
        use crate::scripting::host::transaction::{parameter, Operation};
        let session = self
            .w
            .session
            .as_ref()
            .filter(|s| s.layout == Some(layout))?;
        let schema = self.w.calcs.schema()?;
        let options = schema.file_options.as_ref()?;
        let script = options.window_transaction?;
        let table = crate::calc::layout_occurrence(session)
            .and_then(|o| schema.table_of(o))
            .map(|t| t.name.clone())?;
        let field = Some(options.window_transaction_field.trim())
            .filter(|f| !f.is_empty())
            .unwrap_or("OnWindowTransaction");
        let content = session
            .page
            .as_ref()
            .and_then(|p| p.data.fields.iter().position(|f| f.name == field))
            .and_then(|i| record.values.get(i).cloned().flatten())
            .unwrap_or_default();
        let operation = match operation {
            "New" => Operation::New,
            "Deleted" => Operation::Deleted,
            _ => Operation::Modified,
        };
        let file = om_data::file_refs::file_stem(&session.target.file);
        let parameter = parameter(file, &[(table, operation, record.id, content)]);
        Some(Start::WindowTransaction { script, parameter })
    }

    /// Before the user's `action` happens: a pre-event trigger holds it.
    /// Returns whether it is held.
    pub(super) fn hold_for_trigger(&mut self, ctx: &egui::Context, action: &Action) -> bool {
        if std::mem::take(&mut self.script.bypass) || self.script.busy() {
            return false;
        }
        self.action_stages(ctx, action, 0)
    }

    /// The pre-event triggers of the user's `action`, in FileMaker's order:
    /// OnRecordCommit (or OnRecordRevert) when the record has unsaved
    /// changes, then OnLayoutExit; OnModeExit.
    fn stages_of(&mut self, action: &Action) -> Vec<TriggerEvent> {
        let modified = self.record_modified();
        let layout = self.w.session.as_ref().and_then(|s| s.layout);
        let commit = modified;
        let mut stages = Vec::new();
        match action {
            Action::SelectLayout(id) if Some(*id) != layout => {
                if commit {
                    stages.push(TriggerEvent::RecordCommit);
                }
                stages.push(TriggerEvent::LayoutExit);
            }
            Action::SetMode(mode) if *mode != self.w.mode => {
                if commit && self.w.mode == Mode::Browse {
                    stages.push(TriggerEvent::RecordCommit);
                }
                stages.push(TriggerEvent::ModeExit);
            }
            Action::CommitRecord
            | Action::GoToRecord(_)
            | Action::ExportRecords
            | Action::SaveRecordsAs(_)
            | Action::ImportRecords
                if commit =>
            {
                stages.push(TriggerEvent::RecordCommit)
            }
            Action::RevertRecord if modified => stages.push(TriggerEvent::RecordRevert),
            // The window closes; the file's last window closes the file
            // (its record is committed after these, with its own
            // OnRecordCommit).
            Action::CloseWindow if self.w.session.is_some() => {
                stages.push(TriggerEvent::WindowClose);
                if !self.windows.iter().any(|d| d.file_id == self.w.file_id) {
                    stages.push(TriggerEvent::LastWindowClose);
                }
            }
            _ => {}
        }
        stages
    }

    /// Fire `action`'s pre-event triggers from stage `from`; `true` while
    /// one holds it.
    fn action_stages(&mut self, ctx: &egui::Context, action: &Action, from: usize) -> bool {
        let stages = self.stages_of(action);
        for (i, event) in stages.iter().enumerate().skip(from) {
            let held = Some(Held::Action(action.clone(), i + 1));
            if self.fire(ctx, *event, None, held, TriggerInfo::default()) {
                return true;
            }
        }
        false
    }

    /// Whether `action`'s pre-event triggers include OnRecordCommit: then
    /// the save it leads to was approved with it (`stages_of`).
    pub(super) fn commits_first(action: &Action) -> bool {
        matches!(
            action,
            Action::SelectLayout(_)
                | Action::GoToRecord(_)
                | Action::SetMode(_)
                | Action::ExportRecords
                | Action::SaveRecordsAs(_)
                | Action::ImportRecords
        )
    }

    /// After the user's `action` happened: its delayed triggers. A script
    /// running now (the trigger that let the action go on, the save's
    /// OnWindowTransaction) does not drop them: they wait for it.
    pub(super) fn after_action(&mut self, ctx: &egui::Context, action: &Action) {
        match action {
            Action::GoToRecord(_) => {
                self.fire(
                    ctx,
                    TriggerEvent::RecordLoad,
                    None,
                    None,
                    TriggerInfo::default(),
                );
            }
            Action::SetMode(_) => {
                self.fire(
                    ctx,
                    TriggerEvent::ModeEnter,
                    None,
                    None,
                    TriggerInfo::default(),
                );
            }
            Action::SetView(_) => {
                self.fire(
                    ctx,
                    TriggerEvent::ViewChange,
                    None,
                    None,
                    TriggerInfo::default(),
                );
            }
            Action::SelectLayout(id) => self.w.pending_enter = Some(*id),
            _ => {}
        }
    }

    /// The layout the user went to has loaded: OnLayoutEnter, then
    /// OnRecordLoad.
    pub(super) fn layout_entered(&mut self, ctx: &egui::Context, layout: u32) {
        if self.w.pending_enter != Some(layout) {
            return;
        }
        self.w.pending_enter = None;
        // Each script waits for the one before (a running script too): the
        // file's window opened, then its layout and record loaded.
        let info = TriggerInfo::default;
        if std::mem::take(&mut self.script.window_opened) {
            self.fire(ctx, TriggerEvent::FirstWindowOpen, None, None, info());
            self.fire(ctx, TriggerEvent::WindowOpen, None, None, info());
        }
        self.fire(ctx, TriggerEvent::LayoutEnter, None, None, info());
        self.fire(ctx, TriggerEvent::RecordLoad, None, None, info());
    }

    /// The field with the focus changed from `from` to `to` in the last
    /// frame: OnObjectExit (cancelling gives the focus back), then
    /// OnObjectEnter. A modified field is validated and saved first
    /// (OnObjectValidate, OnObjectSave).
    pub(super) fn focus_changed(
        &mut self,
        ctx: &egui::Context,
        from: Option<Focused>,
        to: Option<Focused>,
        modified: bool,
        button: Option<Action>,
    ) -> bool {
        if self.script.busy() {
            return false;
        }
        let mut button_held = false;
        if let Some(from) = from {
            let after = button.map(|action| ButtonAfterExit::capture(self, action));
            let has_button = after.is_some();
            let left = self.w.document.left_related;
            button_held = self.exit_stages(ctx, from, modified, 0, after, left) && has_button;
        }
        if let Some(to) = &to {
            if let Some(object) = self.focused_object(to) {
                self.fire(
                    ctx,
                    TriggerEvent::ObjectEnter,
                    Some(object),
                    None,
                    TriggerInfo::default(),
                );
            }
        }
        button_held
    }

    /// The exit triggers of the field the focus left, from stage `from`;
    /// `left`: the related field the focus left as the exit began.
    fn exit_stages(
        &mut self,
        ctx: &egui::Context,
        field: Focused,
        modified: bool,
        from: usize,
        after: Option<ButtonAfterExit>,
        left: Option<crate::document::RelatedFocus>,
    ) -> bool {
        let Some(object) = self.focused_object(&field) else {
            return false;
        };
        // A related field's exit triggers see it active in its row, as
        // its enter and modify triggers do (#1005).
        let related = left.filter(|related| related.object == object);
        // Later stages run for this object, whichever shows the field first.
        let field = (field.0, Some(object));
        let stages: &[TriggerEvent] = if modified {
            &[
                TriggerEvent::ObjectValidate,
                TriggerEvent::ObjectSave,
                TriggerEvent::ObjectExit,
            ]
        } else {
            &[TriggerEvent::ObjectExit]
        };
        // The related field's edit is saved once its OnObjectSave stage
        // passes (#1040): no trigger held it, or its trigger let the exit
        // go on (the exit resumes after it). One that refused it saved
        // nothing.
        let resumed_after = from.checked_sub(1).and_then(|i| stages.get(i));
        if let (Some(TriggerEvent::ObjectSave), Some(related)) = (resumed_after, related) {
            self.mark_related_saved(&related);
        }
        // The exit of an ordinary commit (a click on the layout's
        // background): its OnObjectSave script reads the records begun in
        // portals' creation rows with what their rules enter (#1047), as
        // FileMaker Pro 26.0.3's did. A Tab or a click into another field
        // previews nothing [I: unmeasured].
        let commits = after.as_ref().is_some_and(ButtonAfterExit::commits);
        for (i, event) in stages.iter().enumerate().skip(from) {
            let held = Some(Held::Exit(
                field.clone(),
                modified,
                i + 1,
                after.clone(),
                self.key_owner(),
                related.map(Box::new),
            ));
            let prepare = commits && *event == TriggerEvent::ObjectSave;
            let fired = self.as_active(related, &field.0, |app| {
                app.fire_with(
                    ctx,
                    *event,
                    Some(object),
                    held,
                    TriggerInfo::default(),
                    prepare,
                )
            });
            if fired {
                return true;
            }
            if let (TriggerEvent::ObjectSave, Some(related)) = (event, related) {
                self.mark_related_saved(&related);
            }
        }
        false
    }

    /// The draft of the record related field `focus` shows, in what the
    /// window's record keeps uncommitted: a stored record's by its table
    /// and record, a begun one's by its row.
    fn related_draft(
        &mut self,
        focus: &crate::document::RelatedFocus,
    ) -> Option<&mut crate::scripting::RelatedDraft> {
        let (_, _, open) = self.w.uncommitted.as_mut()?;
        open.related.iter_mut().find(|draft| match focus.field.row {
            crate::document::RelatedRow::Record(record) => {
                draft.table == focus.field.table && draft.record == Some(record)
            }
            crate::document::RelatedRow::New { id, .. } => draft.begun == Some(id),
        })
    }

    /// Related field `focus`'s value is saved: its OnObjectSave stage
    /// passed (#1040).
    pub(super) fn mark_related_saved(&mut self, focus: &crate::document::RelatedFocus) {
        let cell = focus.field.field;
        if let Some(draft) = self.related_draft(focus) {
            if draft.typed.contains(&cell) {
                let value = draft.cells.get(&cell).cloned().flatten();
                draft.saved.insert(cell, value);
            }
        }
    }

    /// Whether related field `focus` still holds the value saved last: any
    /// change since, typed or chosen, is unsaved.
    pub(super) fn related_saved(&mut self, focus: &crate::document::RelatedFocus) -> bool {
        let cell = focus.field.field;
        self.related_draft(focus).is_some_and(|draft| {
            draft.saved.get(&cell) == Some(&draft.cells.get(&cell).cloned().flatten())
        })
    }

    /// `run` with the related field `related` (named `name`) the window's
    /// active field, as the script it starts sees it; none: as it is.
    fn as_active<T>(
        &mut self,
        related: Option<crate::document::RelatedFocus>,
        name: &str,
        run: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let Some(related) = related else {
            return run(self);
        };
        let document = &mut self.w.document;
        let saved = (
            document.active.replace(name.to_owned()),
            document.active_object.replace(related.object),
            document.active_related.replace(related),
        );
        let result = run(self);
        let document = &mut self.w.document;
        (
            document.active,
            document.active_object,
            document.active_related,
        ) = saved;
        result
    }

    /// The user changed the field (in `object`, when known): OnObjectModify.
    pub(super) fn field_modified(&mut self, ctx: &egui::Context, field: &str, object: Option<u64>) {
        if self.script.busy() {
            return;
        }
        if let Some(object) = object.or_else(|| self.field_object(field)) {
            self.fire(
                ctx,
                TriggerEvent::ObjectModify,
                Some(object),
                None,
                TriggerInfo::default(),
            );
        }
    }

    // ----- what the worker says ------------------------------------------------

    pub(super) fn on_script_event(&mut self, ctx: &egui::Context, event: ScriptEvent) {
        match event {
            ScriptEvent::FileOpened(_)
            | ScriptEvent::FileClosed(_)
            | ScriptEvent::Globals { .. }
            | ScriptEvent::Unsettled { .. } => {}
            ScriptEvent::Started => {
                self.script.running = true;
                self.script.paused = None;
            }
            ScriptEvent::Window(update) => self.apply_window(ctx, *update),
            ScriptEvent::SaveRefused {
                layout,
                record,
                error,
            } => {
                if self.script.saving == Some(self.w.id) {
                    self.w.worker_save_failure = Some((layout, record, error));
                }
            }
            ScriptEvent::ClosePopover {
                layout,
                popover,
                button,
            } => {
                if self
                    .w
                    .session
                    .as_ref()
                    .and_then(|s| s.layout)
                    .map(u64::from)
                    == Some(layout)
                    && self.w.document.close_popover(layout, popover, button)
                {
                    // The worker already ran the field's exit triggers.
                    // Its disappearance must not fire them again on the UI.
                    self.w.document.active = None;
                }
            }
            ScriptEvent::PaperSettings { file, paper } => {
                for doc in std::iter::once(&mut self.w).chain(&mut self.windows) {
                    if doc.file_id == file {
                        if let Some(session) = &mut doc.session {
                            session.set_paper(paper.clone());
                        }
                    }
                }
            }
            ScriptEvent::Prompt { id, prompt } => {
                let (inputs, text) = match &prompt {
                    Prompt::Dialog(dialog) => (
                        dialog
                            .inputs
                            .iter()
                            .map(|i| {
                                let locale = self
                                    .w
                                    .calcs
                                    .schema()
                                    .map_or_else(om_data::calc::Locale::en_us, |s| {
                                        s.locale.clone()
                                    });
                                let cx = om_data::calc::Ctx::new(
                                    &locale,
                                    om_data::host::session::today(0),
                                );
                                i.value.to_text(&cx).into_string()
                            })
                            .collect(),
                        String::new(),
                    ),
                    Prompt::Alert {
                        message, detail, ..
                    } => (
                        Vec::new(),
                        if detail.is_empty() {
                            message.clone()
                        } else {
                            format!("{message}\n{detail}")
                        },
                    ),
                    Prompt::PluginDialog { dialog, .. } => (Vec::new(), dialog.message.clone()),
                    Prompt::Confirm { .. }
                    | Prompt::Sort { .. }
                    | Prompt::SignIn { .. }
                    | Prompt::Index { .. }
                    | Prompt::Export(_)
                    | Prompt::ImportSource(_)
                    | Prompt::ContainerFile(_)
                    | Prompt::Pdf(_)
                    | Prompt::Print(_)
                    | Prompt::PageSetup(_)
                    | Prompt::Import(_)
                    | Prompt::ImportSummary { .. } => (Vec::new(), String::new()),
                };
                let sort = match &prompt {
                    Prompt::Sort {
                        layout,
                        occurrence,
                        fields,
                        current,
                    } => Some(Box::new(
                        crate::script_workspace::SortOrder::new(
                            current.clone(),
                            self.w.calcs.schema().cloned(),
                        )
                        .browse(
                            layout.clone(),
                            *occurrence,
                            fields.clone(),
                        ),
                    )),
                    _ => None,
                };
                let login = match &prompt {
                    Prompt::SignIn {
                        file,
                        account,
                        error,
                    } => {
                        let mut dialog =
                            crate::open_file::LoginDialog::related(file, account.clone());
                        dialog.error = error.clone();
                        Some(dialog)
                    }
                    _ => None,
                };
                // An Insert command's file comes from the platform's panel
                // where the app has a native window to show one beside.
                let panel = match (&prompt, self.panel_parent) {
                    (Prompt::ContainerFile(dialog), Some(parent)) if !dialog.export => {
                        let ctx = ctx.clone();
                        let message = format!("Into field {}", dialog.field);
                        let request = crate::file_panel::Request::open(&dialog.title, &message);
                        crate::file_panel::FilePanel::show(&request, parent, move || {
                            ctx.request_repaint()
                        })
                    }
                    _ => None,
                };
                self.script.prompt = Some(PromptView {
                    id,
                    prompt,
                    inputs,
                    pick: 0,
                    order_pick: 0,
                    text,
                    login,
                    sort,
                    panel,
                    opened: std::time::Instant::now(),
                });
            }
            ScriptEvent::Paused { seconds, message } => {
                self.script.paused = Some(seconds);
                if let Some(message) = message {
                    self.error = Some(message);
                }
                // A save the user made while paused ran on top of the
                // paused script, as did a trigger of the user's.
                self.saved_on_worker(ctx);
                self.resume_triggered(ctx);
                self.start_queued(ctx);
            }
            ScriptEvent::Finished {
                window,
                globals,
                message,
            } => {
                self.script.running = false;
                self.script.owner_file = None;
                self.script.paused = None;
                self.script.prompt = None;
                if self.w.globals.revision() != globals.revision() {
                    self.w.calcs.forget_results();
                    if let Some(session) = &self.w.session {
                        session.report_data_changed();
                    }
                }
                self.w.globals = *globals;
                self.apply_window(ctx, *window);
                self.w.document.release_related_focus();
                // The menus' New Record: while the focus is the step's own
                // first field (no trigger moved it), the desktop's first
                // field instead, which passes over one its hide condition
                // hides.
                if std::mem::take(&mut self.w.new_record_focus)
                    && message.is_none()
                    && self.w.document.focus == self.first_tab_field()
                {
                    self.w.document.focus = None;
                    self.w.document.focus_first = true;
                }
                if let Some(message) = message {
                    self.error = Some(message);
                }
                self.saved_on_worker(ctx);
                self.resume_triggered(ctx);
                self.close_or_quit_after_script(ctx);
                self.start_queued(ctx);
            }
            ScriptEvent::Action {
                token,
                proceed,
                record,
            } => {
                if let Some((origin, held)) = self.script.held.remove(&token) {
                    self.script.resumes.push((origin, held, proceed, record));
                }
            }
            // A lost connection stopped the timer for good.
            ScriptEvent::Timer(_) if self.w.connection_lost.is_some() => {}
            ScriptEvent::Timer(interval) => {
                self.w.timer = interval
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .map(Duration::from_secs_f64)
                    .map(|d| (d, Instant::now() + d));
            }
            // One idle script for the app, as the plug-in keeps one [I]:
            // this window's replaces any other's, and stopping it from any
            // window stops it ("Stops any idle timer").
            ScriptEvent::Idle(_) if self.w.connection_lost.is_some() => {}
            ScriptEvent::Idle(delay) => {
                let now = Instant::now();
                self.script.idle =
                    delay.map(|d| (self.w.id, crate::plugin_ui::IdleClock::new(d, now)));
            }
            ScriptEvent::Callback { id, error, result } => {
                self.start_script(ctx, Start::Callback { id, error, result });
            }
            ScriptEvent::OpenMenu(step) => {
                let action = if step == 88 {
                    Action::ScriptWorkspace
                } else {
                    Action::ManageDatabase
                };
                self.apply(ctx, action);
            }
            ScriptEvent::WebViewer { id, command } => self.web_viewer_command(ctx, id, command),
            ScriptEvent::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            ScriptEvent::Copy(text) => ctx.copy_text(text),
            ScriptEvent::Viewport(command) => {
                if super::windows::sizes(&command) {
                    self.w.fit = None;
                }
                ctx.send_viewport_cmd(command);
            }
            ScriptEvent::Fit(fit) => self.w.fit = Some(fit),
            // app/windows.rs closes the window.
            ScriptEvent::CloseWindow(_) => {}
            ScriptEvent::Beep => eprint!("\x07"),
            // The worker committed its window and halts: the rest closes
            // once its job has ended (Finished). A close trigger's own
            // Exit Application takes over its close, closing its file
            // first.
            ScriptEvent::Quit => {
                // A command waiting for the close goes, as with Quit.
                self.launch_after_close = None;
                self.take_over_close(None, true);
                self.script.quit = true;
            }
            ScriptEvent::SchemaChanged => self.request(ctx, Request::CalcSchema),
            // The script stays busy until it ends (Finished), when the
            // worker has sent every window's last state.
            ScriptEvent::CloseFile => {
                // A close trigger's own Close File takes over its close;
                // any other script's closes the file. Either goes on when
                // the script has ended.
                if !self.take_over_close(Some(self.w.file_id), false) {
                    self.script.close_file = Some(self.w.file_id);
                }
            }
            ScriptEvent::Failed(message) => {
                self.script.running = false;
                self.script.owner_file = None;
                self.script.paused = None;
                self.script.prompt = None;
                self.error = Some(message);
                // As at Finished: a save the worker was making for the user
                // ended (failed, the window keeping its edits), and the jobs
                // waiting behind the script run now.
                self.saved_on_worker(ctx);
                self.resume_triggered(ctx);
                self.close_or_quit_after_script(ctx);
                self.start_queued(ctx);
            }
        }
    }

    /// A close trigger ran Close File (of its own file, `file`) or Exit
    /// Application (`file` is `None`): it takes over the close it holds.
    /// The halt's veto then finds nothing to stop, and once the script has
    /// ended ([`Self::close_after_script`]) the trigger's file's windows
    /// close without their close triggers, each committing
    /// (`Closing::quiet`); with `quit`, or in a Quit, the other files close
    /// after as Quit closes them. The trigger's own window may have closed
    /// meanwhile. `false` when no such trigger is running.
    pub(super) fn take_over_close(&mut self, file: Option<u64>, quit: bool) -> bool {
        let held = self.script.held.iter().find_map(|(token, (origin, held))| {
            let closes = matches!(held, Held::Action(Action::CloseWindow, _));
            // The trigger's window, or the file it had if it closed.
            let origin_file = self
                .docs()
                .find(|doc| doc.id == *origin)
                .map(|doc| doc.file_id)
                .or_else(|| self.window_files.get(origin).copied())?;
            (closes && file.is_none_or(|file| file == origin_file)).then_some((*token, origin_file))
        });
        let Some((token, file)) = held else {
            return false;
        };
        self.script.held.remove(&token);
        let quit = quit || self.closing.as_ref().is_some_and(|closing| closing.quit);
        self.closing = Some(super::Closing {
            quiet: true,
            ..self.closing_of(file, quit)
        });
        self.script.taken_over = true;
        true
    }

    /// Whether a close under way still waits on something that will carry
    /// it on or stop it: a script, a trigger's held action, or a save.
    pub(super) fn close_waits(&self) -> bool {
        self.script.running
            || self.script.taken_over
            || self.script.close_file.is_some()
            || self.script.saving.is_some()
            || !self.script.held.is_empty()
            || !self.script.resumes.is_empty()
            || self.docs().any(|doc| doc.document.pending)
    }

    /// A close a script's Close File asked for, or a close its trigger
    /// took over ([`Self::take_over_close`]), goes on now that the script
    /// has ended: the worker has sent the windows' last states first, a
    /// frozen window's edits included, so each window commits what the
    /// script left. With `quit` (Exit Application, or Quit chosen
    /// meanwhile), the close is a Quit's. Whether it went on.
    fn close_after_script(&mut self, ctx: &egui::Context, quit: bool) -> bool {
        if let Some(file) = self.script.close_file.take() {
            let window = self
                .docs()
                .find(|doc| doc.file_id == file && doc.session.is_some())
                .map(|doc| doc.id);
            if let Some(window) = window {
                self.with_window(window, |app| {
                    if quit {
                        // Close File keeps a Quit under way.
                        app.closing = Some(app.closing_front(true));
                    }
                    app.apply(ctx, Action::CloseFile);
                });
                return true;
            }
        }
        if !std::mem::take(&mut self.script.taken_over) {
            return false;
        }
        if let Some(closing) = self.closing.as_mut().filter(|_| quit) {
            closing.quit = true;
        }
        let Some(next) = self.next_to_close() else {
            return false;
        };
        self.with_window(next, |app| app.close_window(ctx));
        true
    }

    /// A script ended: a close its Close File asked for, or its trigger
    /// took over, goes on, and Exit Application (or Quit chosen while the
    /// close waited) quits after it, or at once with none.
    pub(super) fn close_or_quit_after_script(&mut self, ctx: &egui::Context) {
        let quit = std::mem::take(&mut self.script.quit);
        if !self.close_after_script(ctx, quit) && quit {
            self.apply(ctx, Action::Quit);
        }
    }

    /// Quit chosen while a script's Close File waits for its script to end
    /// quits after that close. Whether one was waiting.
    pub(super) fn quit_after_script_close(&mut self) -> bool {
        if self.script.close_file.is_none() {
            return false;
        }
        self.script.quit = true;
        true
    }

    /// The job that ran the user's triggers ended: their held actions go on
    /// (or stop) now.
    fn resume_triggered(&mut self, ctx: &egui::Context) {
        for (origin, held, proceed, record) in std::mem::take(&mut self.script.resumes) {
            self.resume_held(ctx, origin, held, proceed, record);
        }
    }

    /// A trigger let the user's held action go on (`proceed`) or not: it
    /// goes on in window `origin`, where the user acted, whichever window
    /// is in front now. When that window has closed since, the action goes
    /// with it; no other window takes it. `record`: the current record of
    /// the trigger's window as its script left it, `Some(None)` none, when
    /// the worker knows it (`ScriptEvent::Action`). A click held by a field's exit goes only on
    /// the record it was made on: a script that went to another record (a
    /// Show All Records) can leave the window's page showing the old one
    /// until it is read again, so the page alone does not tell (#1073).
    pub(super) fn resume_held(
        &mut self,
        ctx: &egui::Context,
        origin: u32,
        held: Held,
        proceed: bool,
        record: Option<Option<i64>>,
    ) {
        if let Held::Exit(_, _, _, Some(after), ..) = &held {
            let moved = record.is_some_and(|record| after.record != record);
            if moved || !after.is_current(self) {
                self.with_window(origin, |app| app.w.document.cancel_pending_focus());
                return;
            }
        }
        let closes = proceed && matches!(held, Held::Action(Action::CloseWindow, _));
        let resumed =
            self.with_window(origin, |app| match (held, proceed) {
                (Held::Action(action, stage), true) => {
                    if !app.action_stages(ctx, &action, stage) {
                        app.script.bypass = true;
                        app.apply(ctx, action);
                        app.script.bypass = false;
                    }
                }
                // Stopped: so is closing the file, the click that began
                // the action, and what a save it vetoed was for (a later
                // save must not run it).
                (Held::Action(..), false) => {
                    app.w.document.cancel_pending_focus();
                    // A close a trigger's Close File or Exit Application
                    // took over is that script's now, not this action's.
                    if !app.script.taken_over {
                        app.closing = None;
                    }
                    app.w.deferred_action = None;
                }
                (Held::Exit(_, _, _, _, owner, _), _) if owner != app.key_owner() => {
                    app.w.document.cancel_pending_focus();
                }
                (Held::Exit(field, modified, stage, after, _, left), true) => {
                    let left = left.map(|left| *left);
                    if !app.exit_stages(ctx, field, modified, stage, after.clone(), left) {
                        if let Some(after) = after.filter(|after| after.is_current(app)) {
                            app.apply(ctx, after.action);
                        }
                    }
                }
                (Held::Exit((field, object), .., left), false) => {
                    app.w.document.cancel_pending_focus();
                    // A portal row's field: the editor of the row it left
                    // (#1050).
                    let related = left.map(|left| *left).filter(|related| {
                        related.portal.is_some() && Some(related.object) == object
                    });
                    if let Some(related) = related {
                        app.w.document.refocus_related(field, related);
                    } else if let Some(object) = object {
                        if let Some((layout, record)) = app.w.session.as_ref().and_then(|session| {
                            Some((session.layout?, session.current_record()?.1.id))
                        }) {
                            app.w
                                .document
                                .restore_object_focus(u64::from(layout), record, object);
                        }
                    } else {
                        app.w.document.focus = Some(field);
                    }
                }
                (Held::Key(key), proceed) => app.resume_keystroke(ctx, *key, proceed),
            });
        if resumed.is_none() {
            // Its window closed meanwhile. A close of it its triggers let
            // go on (their script's Close Window closed it) goes on with
            // the file's next window; otherwise closing the file stops.
            let began_with = self
                .closing
                .as_ref()
                .is_some_and(|closing| closing.windows.contains(&origin));
            if closes && began_with {
                self.close_next(ctx, origin);
            } else if !self.script.taken_over {
                // A close a trigger took over is that script's now.
                self.closing = None;
            }
        }
    }

    /// The connection to the host is gone: no script runs from now on (the
    /// worker stops the one running), so the timer and the scripts waiting
    /// their turn go, and so do the user's actions held for their triggers,
    /// approved or not: one that still works offline (closing a window or
    /// the file) would go on without the save it was to make first, and
    /// discard the record's edits the banner keeps.
    pub(super) fn stop_scripts(&mut self) {
        self.w.timer = None;
        self.script.queued.clear();
        self.script.held.clear();
        self.script.resumes.clear();
        self.script.taken_over = false;
        self.script.close_file = None;
        for id in self.window_ids() {
            self.with_window(id, |app| {
                app.w.pending_enter = None;
                app.drop_keystrokes();
            });
        }
    }

    /// Show what a script left in the window. Once the connection to the
    /// host is lost the window stays as it is: a layout or records the
    /// script went to could not be read.
    fn apply_window(&mut self, ctx: &egui::Context, update: WindowUpdate) {
        if self.w.connection_lost.is_some() {
            return;
        }
        let previous = self.window_state();
        let WindowUpdate {
            file: _,
            state,
            data_changed,
            refreshed,
            objects,
            scroll,
            fronted,
            originals,
            found_set,
            background: _,
        } = update;
        // A pure targeted update (and its unchanged final snapshot) retains
        // completed results outside the requested objects. Other state changes
        // still invalidate all calculation contexts before targets are applied.
        let retain_results = !data_changed
            && !refreshed
            && found_set.is_none()
            && state == previous
            && (!objects.is_empty() || self.w.calcs.retains_targeted_results());
        if !retain_results {
            self.w.calcs.forget_results();
            if let Some(session) = &self.w.session {
                session.report_data_changed();
            }
        }
        if found_set.is_some() {
            self.w.found_set_revision = self.w.found_set_revision.wrapping_add(1);
            self.w.found_set = found_set;
        }
        // The tab panels steps brought to the front (Go to Object); the
        // others stay as the window shows them.
        for (layout, control, panel) in fronted {
            self.w.document.panels.insert((layout, control), panel);
        }
        let Some(session) = self.w.session.as_mut() else {
            return;
        };
        let mode = match state.mode {
            RunMode::Browse => Mode::Browse,
            RunMode::Find => Mode::Find,
            RunMode::Preview => Mode::Preview,
            RunMode::Layout => Mode::Layout,
        };
        // The user's related field a trigger held stays held only while
        // the script leaves the focus, the mode, the layout and the record
        // as they were.
        if let Some(object) = self.w.document.held_related_object() {
            let kept = mode == self.w.mode
                && state.layout == session.layout
                && state.record == session.current_record().map(|(_, record)| record.id)
                && state.focus.is_none()
                && !state.field_focus
                && state.portal.and_then(|row| row.field) == Some(object);
            if !kept {
                self.w.document.drop_related_hold();
            }
        }
        if mode != self.w.mode || state.layout != session.layout || state.focus.is_some() {
            // A trigger that ends with the focus where the user put it
            // (the option set being entered) keeps the set's waiting
            // change; one that moved the focus or the context does not.
            if mode == self.w.mode
                && state.layout == session.layout
                && !state.field_focus
                && state.focus.is_some()
                && state.focus == self.w.document.active
            {
                self.w.document.cancel_pending_focus_keeping_option();
            } else {
                self.w.document.cancel_pending_focus();
            }
            // A later navigation or field step wins over Close Popover's
            // pending return to the button, even in the same worker slice.
            self.w.document.cancel_popover_focus();
        }
        // Entering Find mode puts the focus in the request's first field.
        let entered_find = mode == Mode::Find && self.w.mode != Mode::Find;
        if mode != self.w.mode {
            self.w.mode = mode;
            self.w.calcs.forget_results();
        }
        if let Some(layout) = state.layout.filter(|l| Some(*l) != session.layout) {
            // The table left keeps its record for the window's return; the
            // script's record is the new layout's.
            let schema = self.w.calcs.schema().map(AsRef::as_ref);
            let first = session.layout.is_none();
            let key = session.select_layout_with_schema(layout, schema);
            // On a layout the account may not see, a window a script
            // opened there (New Window) shows the blank page, and Go to
            // Related Record names it in the toolbar, as FileMaker Pro
            // 26.0.3 does (#696).
            session.blank_start = first && !session.shows_layout(layout);
            session.current = state.index;
            self.request_layout(ctx, Some(key));
        } else if data_changed || refreshed {
            session.current = state.index;
            let key = session.refresh();
            self.w.calcs.data_changed();
            self.request_page(ctx, key);
        } else if state.index != session.current {
            let key = session.go_to(state.index);
            self.request_page(ctx, key);
        }
        if let Some(session) = &mut self.w.session {
            session.preview_page = state.preview_page;
            // Also when the layout stays: Go to Related Record to the hidden
            // layout the window opened on names it and leaves the blank
            // page; Go to Layout there drops the name (#696).
            let hidden = session.layout.is_some_and(|id| !session.shows_layout(id));
            session.hidden_named = state.hidden_named && hidden;
            if session.hidden_named {
                session.blank_start = false;
            }
        }
        if data_changed {
            self.refresh_other_windows(ctx);
        }
        if !objects.is_empty() {
            if let Some(session) = &self.w.session {
                session.report_data_changed();
            }
        }
        for refresh in &objects {
            if let Some(session) = self.w.session.as_ref() {
                self.w.calcs.refresh_object(refresh, session);
            }
        }
        // The views the script's window has, the ones it set included:
        // added to the window's, so that an entry the window gained since
        // the script's snapshot (another window's script, paused) stays.
        self.w.view = state.view.into();
        self.w
            .views
            .extend(state.views.iter().map(|(l, v)| (*l, ViewMode::from(*v))));
        // Find mode's requests, by field name.
        self.w.find = FindState {
            requests: state.find.clone(),
            current: state.find_index,
            focused_field: if mode == Mode::Find {
                state.focus.clone()
            } else {
                None
            },
            focus_first: self.w.find.focus_first || entered_find,
            focus_object: self.w.find.focus_object,
        };
        if self.w.find.requests.is_empty() {
            self.w.find = FindState::default();
        }
        self.w.find.current = self.w.find.current.min(self.w.find.requests.len() - 1);
        // The record's unsaved values.
        if let (Some(layout), Some(record)) = (state.layout, state.record) {
            // A refused worker save may have read a newer row. It must not
            // replace the originals of the UI draft whose accepted version
            // is retained: that would turn a real conflict into "unchanged".
            let preserved: HashMap<_, _> = if self.w.retained.contains_key(&(layout, record)) {
                self.w
                    .document
                    .drafts()
                    .remove(&(layout, record))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|draft| (draft.name, draft.original))
                    .collect()
            } else {
                HashMap::new()
            };
            self.w.document.clear_record(layout, record);
            for (field, value) in &state.drafts {
                let original = preserved
                    .get(field)
                    .cloned()
                    .unwrap_or_else(|| originals.get(field).cloned().flatten().unwrap_or_default());
                self.w.document.put_draft(
                    layout,
                    record,
                    field,
                    original,
                    value.clone().unwrap_or_default(),
                );
            }
        }
        self.w.portal = state
            .layout
            .zip(state.record)
            .zip(state.portal)
            .map(|((layout, record), row)| (layout, record, row));
        // A step put the focus in a field of a portal row (Go to Field, Go
        // to Object, Go to Portal Row): that row's editor takes it. A focus
        // that has not landed yet goes when a later update leaves the
        // focus anywhere else.
        match self.w.portal {
            Some((layout, record, row)) if state.field_focus && row.row > 0 => {
                if let Some(field) = row.field {
                    self.w.document.portal_focus = Some(crate::document::PortalFocus {
                        layout: u64::from(layout),
                        parent: record,
                        row: row.row,
                        field,
                        select: state.select,
                    });
                }
            }
            Some((_, _, row)) if row.field.is_some() => {}
            _ => self.w.document.portal_focus = None,
        }
        self.w.document.active_object = state.object;
        self.w.document.script_focus(state.focus.as_deref());
        // A step moved the selection in the field it focuses: Set
        // Selection, or an Insert step, which puts the insertion point after
        // what it inserted (`None`: after the field's text). A selection
        // only reset, with the field unchanged, leaves the editor's. Its
        // length is the text's as the step left it, a draft's or else the
        // editor's; a field whose editor does not have the focus yet takes
        // it when it does.
        if state.selection != previous.selection {
            let at = state
                .layout
                .zip(state.record)
                .map(|(layout, record)| (u64::from(layout), record));
            if let Some((field, at)) = state
                .focus
                .as_deref()
                .filter(|field| {
                    state.selection.is_some()
                        || state.drafts.get(*field) != previous.drafts.get(*field)
                })
                .zip(at)
            {
                let len = state
                    .drafts
                    .get(field)
                    .map(|value| value.as_deref().map_or(0, |value| value.chars().count()));
                self.w
                    .document
                    .place_cursor(ctx, at, field, state.selection, len);
            }
        }
        self.w.uncommitted = state
            .layout
            .zip(state.record)
            .filter(|_| !state.uncommitted.is_empty())
            .map(|(layout, record)| (layout, record, state.uncommitted.clone()));
        // A record a step created through a relationship (Set Field into a
        // creation row, an Insert there) shows as a begun row.
        if let Some((_, _, open)) = &mut self.w.uncommitted {
            for draft in open
                .related
                .iter_mut()
                .filter(|d| d.record.is_none() && d.begun.is_none() && !d.deleted)
            {
                draft.begun = Some(self.w.document.adopt_begun());
            }
        }
        // The user's edits of related records the record still keeps stay;
        // a commit or a revert ended the others, or a script replaced them.
        // A record begun in a creation row stays while the record keeps it,
        // showing the values the record holds (a script's Set Field too).
        let at = state.layout.zip(state.record);
        self.w.document.settle_related(|field| {
            let open = &state.uncommitted.related;
            if !at.is_some_and(|(layout, record)| {
                (u64::from(layout), record) == (field.layout, field.parent)
            }) {
                return None;
            }
            match field.row {
                RelatedRow::Record(record) => open
                    .iter()
                    .any(|d| {
                        !d.deleted
                            && d.table == field.table
                            && d.record == Some(record)
                            && d.typed.contains(&field.field)
                    })
                    .then_some(None),
                RelatedRow::New { occurrence, id } => open
                    .iter()
                    .find(|d| d.begun == Some(id) && d.occurrence == occurrence)
                    .map(|d| {
                        d.cells
                            .get(&field.field)
                            .map(|v| v.clone().unwrap_or_default())
                    }),
            }
        });
        if let Some(focus) = &state.focus {
            if state.field_focus || self.w.document.active.as_ref() != Some(focus) {
                self.w.document.focus = Some(focus.clone());
                self.w.document.select_all = state.select;
                if state.field_focus {
                    self.w.document.focus_first = false;
                }
            }
        }
        self.w.document.scroll.queue(scroll, &state);
        self.w.hidden = state.hidden;
        self.w.title = state.title;
        self.w.show_toolbar = state.toolbar;
        self.w.menubar = state.menubar;
        self.w.document.zoom.set_percent(state.zoom);
        self.w.zoom_locked = state.zoom_locked;
        self.w.requests = state.requests;
        self.w.uuid = state.uuid;
        self.w.style = state.style;
    }

    /// The user's input in the viewport being drawn, any window of the
    /// app's, starts the idle script's count again.
    pub(super) fn note_idle_activity(&mut self, ctx: &egui::Context) {
        if let Some((_, clock)) = &mut self.script.idle {
            if crate::plugin_ui::user_active(ctx) {
                clock.note_activity(Instant::now());
            }
        }
    }

    /// The script `FM.RunScriptIdle` scheduled. The app's activity, in any
    /// window, starts its count again (the plug-in counts FileMaker's user
    /// activity, docs/re/mbs-plugin.md 2). Once due, it starts once, when
    /// no script runs, in the window that scheduled it; with that window
    /// closed it is dropped.
    pub(super) fn tick_idle(&mut self, ctx: &egui::Context) {
        let Some((owner, clock)) = &mut self.script.idle else {
            return;
        };
        let owner = *owner;
        let now = Instant::now();
        if crate::plugin_ui::user_active(ctx) {
            clock.note_activity(now);
        }
        let open = self.w.id == owner || self.windows.iter().any(|d| d.id == owner);
        if !open {
            self.script.idle = None;
            return;
        }
        if !clock.due(now) {
            ctx.request_repaint_after(clock.until_due(now));
            return;
        }
        if self.script.running || self.script.prompt.is_some() {
            return;
        }
        self.script.idle = None;
        self.with_window(owner, |app| app.start_script(ctx, Start::Idle));
    }

    /// The time until OnTimer is due; starts it when it is.
    pub(super) fn tick_timer(&mut self, ctx: &egui::Context) {
        let ids: Vec<_> = std::iter::once(&self.w)
            .chain(&self.windows)
            .filter(|d| d.timer.is_some())
            .map(|d| d.id)
            .collect();
        let now = Instant::now();
        for id in ids {
            self.with_window(id, |app| {
                let Some((interval, next)) = app.w.timer else {
                    return;
                };
                if now >= next {
                    app.w.timer = Some((interval, now + interval));
                    if !app.script.running {
                        app.start_script(ctx, Start::Timer);
                    }
                }
                if let Some((_, next)) = app.w.timer {
                    ctx.request_repaint_after(next.saturating_duration_since(now));
                }
            });
        }
    }

    /// Esc / Cmd-period while a script runs; Esc in a pause cancels it.
    pub(super) fn abort_keys(&mut self, ctx: &egui::Context) {
        // The host's message over the window, or its alert that a file was
        // disconnected, takes Escape for itself.
        if !self.script.running
            || self.script.prompt.is_some()
            || self.host_message_shown()
            || self.forced_alert_shown()
        {
            return;
        }
        let abort = ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, Key::Escape)
                || input.consume_key(egui::Modifiers::COMMAND, Key::Period)
        });
        if !abort {
            return;
        }
        if self.script.paused.is_some() {
            self.answer(ctx, 0, Answer::CancelPause);
        } else {
            self.worker.abort_script();
        }
    }

    pub(super) fn answer(&mut self, ctx: &egui::Context, prompt: u64, answer: Answer) {
        self.request(ctx, Request::Answer { prompt, answer });
    }

    // ----- drawing ---------------------------------------------------------------

    /// The pause bar: Continue and Cancel.
    pub(super) fn pause_bar(&mut self, ui: &mut egui::Ui) {
        let Some(seconds) = self.script.paused else {
            return;
        };
        let ctx = ui.ctx().clone();
        let mut answer = None;
        egui::Panel::top("script_paused").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(match seconds {
                    Some(s) => format!("Script paused for {s} seconds"),
                    None => "Script paused".to_owned(),
                }));
                if ui.button("Continue").clicked() {
                    answer = Some(Answer::Resume(Box::new(self.window_state())));
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::CancelPause);
                }
            });
        });
        if let Some(answer) = answer {
            self.script.paused = None;
            self.answer(&ctx, 0, answer);
        }
    }

    /// The prompt on screen, as a modal.
    pub(super) fn prompt(&mut self, ctx: &egui::Context) {
        // A script's dialog that came while the host's alert that a file was
        // disconnected shows waits for its OK: one dialog takes the keys.
        if self.forced_alert_shown() {
            return;
        }
        let Some(view) = self.script.prompt.as_mut() else {
            return;
        };
        let frame = egui::Frame::new()
            .fill(theme::DIALOG_BG)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(12))
            .shadow(ctx.global_style().visuals.window_shadow)
            .inner_margin(egui::Margin::same(16));
        let mut answer = None;
        // A sign-in shows the dialog a layout's sign-in shows.
        if let Some(dialog) = view.login.as_mut() {
            use crate::open_file::LoginAction;
            answer = match dialog.show(ctx) {
                Some(LoginAction::Open) => Some(Answer::SignIn(Some(crate::scripting::SignIn {
                    account: dialog.account.trim().to_owned(),
                    password: std::mem::take(&mut dialog.password),
                }))),
                Some(LoginAction::Cancel) => Some(Answer::SignIn(None)),
                None => None,
            };
        }
        if let Some(dialog) = view.sort.as_mut() {
            answer = dialog.show_browse(ctx).map(Answer::Sort);
        }
        if let (Some(panel), Prompt::ContainerFile(dialog)) = (&view.panel, &view.prompt) {
            use crate::file_panel::Answer as Panel;
            match panel.answer() {
                Some(Panel::Chosen { path, .. }) => {
                    answer = Some(Answer::ContainerFile(Some(
                        crate::scripting::ContainerFileDialog {
                            path: path.display().to_string(),
                            chosen: Some(path),
                            ..dialog.clone()
                        },
                    )));
                }
                Some(Panel::Canceled) => answer = Some(Answer::ContainerFile(None)),
                // No panel after all (no portal): the dialog asks.
                Some(Panel::Unavailable) => view.panel = None,
                None => {}
            }
        }
        if let Some(answer) = answer {
            let id = view.id;
            self.script.prompt = None;
            self.answer(ctx, id, answer);
            return;
        }
        if view.login.is_some() || view.sort.is_some() || view.panel.is_some() {
            return;
        }
        egui::Modal::new(Id::unique(("script_prompt", view.id)))
            .frame(frame)
            .backdrop_color(theme::SCRIM)
            .show(ctx, |ui| {
                ui.set_width(match view.prompt {
                    Prompt::Export(_)
                    | Prompt::ImportSource(_)
                    | Prompt::ContainerFile(_)
                    | Prompt::Import(_)
                    | Prompt::Pdf(_)
                    | Prompt::Print(_)
                    | Prompt::PageSetup(_) => 520.0,
                    _ => 320.0,
                });
                answer = show_prompt(ui, view);
            });
        if let Some(answer) = answer {
            let id = view.id;
            self.script.prompt = None;
            self.answer(ctx, id, answer);
        }
    }
}

/// One prompt's contents; the answer once the user chose.
fn show_prompt(ui: &mut egui::Ui, view: &mut PromptView) -> Option<Answer> {
    let enter = ui.input(|i| i.key_pressed(Key::Enter));
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    let heading = |ui: &mut egui::Ui, text: &str| {
        ui.add(
            egui::Label::new(
                RichText::new(text)
                    .font(egui::FontId::new(theme::BODY, theme::semibold()))
                    .color(theme::TEXT),
            )
            .wrap(),
        );
    };
    match &view.prompt {
        // Shown as a `LoginDialog` (`prompt`).
        Prompt::SignIn { .. } => None,
        Prompt::Alert {
            message,
            detail,
            confirm,
            can_cancel,
        } => {
            heading(ui, message);
            if !detail.is_empty() {
                ui.label(RichText::new(detail).font(theme::small()));
            }
            ui.add_space(12.0);
            let mut choice = None;
            ui.horizontal(|ui| {
                if *confirm
                    && *can_cancel
                    && widgets::dialog_button(ui, "Cancel", false, true).clicked()
                {
                    choice = Some(AlertChoice::Cancel);
                }
                let label = if *confirm { "Continue" } else { "OK" };
                if widgets::dialog_button(ui, label, true, true).clicked() || enter {
                    choice = Some(AlertChoice::Continue);
                }
            });
            if escape && choice.is_none() {
                choice = Some(if *can_cancel {
                    AlertChoice::Cancel
                } else {
                    AlertChoice::Continue
                });
            }
            choice.map(Answer::Alert)
        }
        Prompt::Confirm {
            title,
            message,
            ok,
            default,
        } => {
            // Without a title, the message is the alert's one line, as
            // FileMaker Pro's alerts show it.
            if title.is_empty() {
                heading(ui, message);
            } else {
                heading(ui, title);
                ui.label(message);
            }
            ui.add_space(12.0);
            let mut answer = None;
            ui.horizontal(|ui| {
                if widgets::dialog_button(ui, "Cancel", false, true).clicked() || escape {
                    answer = Some(Answer::Confirm(false));
                }
                if widgets::dialog_button(ui, ok, *default, true).clicked() || (enter && *default) {
                    answer = Some(Answer::Confirm(true));
                }
            });
            answer
        }
        Prompt::PluginDialog { dialog, deadline } => {
            crate::plugin_ui::alert(ui, dialog, *deadline, enter).map(Answer::PluginDialog)
        }
        Prompt::Dialog(dialog) => {
            // "Close after (seconds)": once the time is up the dialog
            // closes as no button, `Get ( LastMessageChoice )` 0 (Claris).
            let closes = dialog
                .close_after
                .and_then(|seconds| std::time::Duration::try_from_secs_f64(seconds).ok())
                .and_then(|after| view.opened.checked_add(after));
            if let Some(closes) = closes {
                let now = std::time::Instant::now();
                if now >= closes {
                    return Some(Answer::Dialog(DialogResponse {
                        button: 0,
                        inputs: view
                            .inputs
                            .iter()
                            .map(|t| om_data::calc::Value::text(t.clone()))
                            .collect(),
                    }));
                }
                ui.ctx().request_repaint_after(closes - now);
            }
            if !dialog.title.is_empty() {
                heading(ui, &dialog.title);
            }
            if !dialog.message.is_empty() {
                ui.add(egui::Label::new(&dialog.message).wrap());
            }
            for (input, value) in dialog.inputs.iter().zip(view.inputs.iter_mut()) {
                ui.add_space(6.0);
                let label = ui.label(&input.label);
                let edit = ui
                    .add(
                        egui::TextEdit::singleline(value)
                            .password(input.password)
                            .desired_width(f32::INFINITY),
                    )
                    .labelled_by(label.id);
                // An input method's deletions are counted against it
                // (OpenMaker Go); a password is not copied anywhere.
                if !input.password {
                    crate::document::note_focused_text(ui, edit.id, value);
                }
            }
            ui.add_space(12.0);
            let mut chosen = None;
            ui.horizontal(|ui| {
                // FileMaker puts the default button last (rightmost).
                for (i, button) in dialog.buttons.iter().enumerate().rev() {
                    if widgets::dialog_button(ui, &button.label, i == 0, true).clicked() {
                        chosen = Some(i);
                    }
                }
            });
            if enter && chosen.is_none() {
                chosen = Some(0);
            }
            if escape && chosen.is_none() && dialog.buttons.len() > 1 {
                // Esc chooses the second button, FileMaker's Cancel.
                chosen = Some(1);
            }
            chosen.map(|i| {
                Answer::Dialog(DialogResponse {
                    button: i as u8 + 1,
                    inputs: view
                        .inputs
                        .iter()
                        .map(|t| om_data::calc::Value::text(t.clone()))
                        .collect(),
                })
            })
        }
        // Shown as a `SortOrder` (`prompt`).
        Prompt::Sort { .. } => None,
        Prompt::Export(_) => {
            let PromptView {
                prompt: Prompt::Export(dialog),
                pick,
                order_pick,
                ..
            } = view
            else {
                return None;
            };
            transfer::export(ui, dialog, pick, order_pick)
        }
        Prompt::PageSetup(_) => {
            let Prompt::PageSetup(settings) = &mut view.prompt else {
                return None;
            };
            crate::page_setup::dialog(ui, settings)
        }
        Prompt::Print(_) => {
            let Prompt::Print(dialog) = &mut view.prompt else {
                return None;
            };
            crate::printing::dialog(ui, dialog)
        }
        Prompt::Pdf(_) => {
            let Prompt::Pdf(dialog) = &mut view.prompt else {
                return None;
            };
            transfer::pdf(ui, dialog)
        }
        Prompt::ImportSource(_) => {
            let Prompt::ImportSource(dialog) = &mut view.prompt else {
                return None;
            };
            transfer::import_source(ui, dialog)
        }
        Prompt::ContainerFile(_) => {
            let Prompt::ContainerFile(dialog) = &mut view.prompt else {
                return None;
            };
            transfer::container_file(ui, dialog)
        }
        Prompt::Import(_) => {
            let Prompt::Import(dialog) = &mut view.prompt else {
                return None;
            };
            transfer::import(ui, dialog)
        }
        Prompt::ImportSummary {
            total,
            added,
            updated,
            skipped,
        } => transfer::summary(ui, *total, *added, *updated, *skipped),
        Prompt::Index { field, values } => {
            heading(ui, &format!("View Index for \u{201c}{field}\u{201d}"));
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for (i, value) in values.iter().enumerate() {
                        ui.selectable_value(&mut view.pick, i, value);
                    }
                });
            ui.add_space(12.0);
            let mut answer = None;
            ui.horizontal(|ui| {
                if widgets::dialog_button(ui, "Cancel", false, true).clicked() || escape {
                    answer = Some(Answer::Index(None));
                }
                let enabled = view.pick < values.len();
                if (widgets::dialog_button(ui, "Paste", true, enabled).clicked()
                    || enter && enabled)
                    && answer.is_none()
                {
                    answer = Some(Answer::Index(values.get(view.pick).cloned()));
                }
            });
            answer
        }
    }
}

/// `Get ( ActiveModifierKeys )`: Shift 1, Caps Lock 2, Control 4, Option or
/// Alt 8, Command 16 (the Claris reference; Caps Lock is not reported by
/// the window system here).
pub(super) fn modifier_keys(modifiers: egui::Modifiers) -> i64 {
    let mut keys = 0;
    if modifiers.shift {
        keys |= 1;
    }
    if modifiers.ctrl {
        keys |= 4;
    }
    if modifiers.alt {
        keys |= 8;
    }
    if modifiers.mac_cmd {
        keys |= 16;
    }
    keys
}

#[cfg(test)]
mod modifier_tests {
    use super::modifier_keys;
    #[test]
    fn control_is_reported_when_egui_aliases_command_to_control() {
        assert_eq!(
            modifier_keys(egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            }),
            4
        );
        assert_eq!(
            modifier_keys(egui::Modifiers {
                ctrl: true,
                ..Default::default()
            }),
            4
        );
        assert_eq!(
            modifier_keys(egui::Modifiers {
                shift: true,
                ctrl: true,
                alt: true,
                mac_cmd: true,
                command: true
            }),
            1 | 4 | 8 | 16
        );
    }
}

#[cfg(test)]
mod queued_window_tests {
    use super::*;
    use crate::{
        app::windows::{Doc, FIRST},
        session::{Session, Target, DEFAULT_PAGE_SIZE},
    };

    #[test]
    fn finishing_a_job_skips_closed_windows_in_the_queue() {
        for paused in [false, true] {
            let ctx = egui::Context::default();
            let mut app = OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()));
            app.w.session = Some(Session::new(
                Target {
                    host: "host".into(),
                    file: "Tasks.fmp12".into(),
                    local: None,
                },
                DEFAULT_PAGE_SIZE,
            ));
            app.windows.push(Doc::new(2));
            app.script.running = true;
            app.script.queued = vec![(2, Start::Timer), (FIRST, Start::Timer)];
            // The running script closed window 2 before yielding. Its
            // queued callback/timer goes with it; the front window's job
            // must still start when this script ends or pauses.
            app.remove_window(2);
            let event = if paused {
                ScriptEvent::Paused {
                    seconds: None,
                    message: None,
                }
            } else {
                ScriptEvent::Finished {
                    window: Box::new(WindowUpdate {
                        file: 0,
                        state: app.window_state(),
                        data_changed: false,
                        refreshed: false,
                        objects: Vec::new(),
                        scroll: Vec::new(),
                        fronted: Vec::new(),
                        originals: Default::default(),
                        found_set: None,
                        background: false,
                    }),
                    globals: Box::default(),
                    message: None,
                }
            };
            app.on_script_event(&ctx, event);
            assert!(
                app.script.busy(),
                "the surviving window's job starts; paused={paused}"
            );
            assert!(
                app.script.queued.is_empty(),
                "no job stays stranded; paused={paused}"
            );
        }
    }

    /// The script `FM.RunScriptIdle` scheduled starts in the window that
    /// scheduled it, not the current one; it waits while a script runs,
    /// and goes with its window.
    #[test]
    fn the_idle_script_starts_only_in_the_window_that_scheduled_it() {
        use std::time::Instant;
        let ctx = egui::Context::default();
        let mut app = OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()));
        let session = || {
            let target = Target {
                host: "host".into(),
                file: "Tasks.fmp12".into(),
                local: None,
            };
            Session::new(target, DEFAULT_PAGE_SIZE)
        };
        app.w.session = Some(session());
        // Window 2 shows another file, so the job's file tells its window.
        let mut second = Doc::new(2);
        second.session = Some(session());
        second.file_id = 77;
        app.windows.push(second);
        let due = || crate::plugin_ui::IdleClock::new(0.0, Instant::now());
        // Its window closed: it goes, and nothing starts.
        app.script.idle = Some((9, due()));
        app.tick_idle(&ctx);
        assert!(app.script.idle.is_none() && app.script.started.is_empty());
        // A running script holds it.
        app.script.running = true;
        app.script.idle = Some((2, due()));
        app.tick_idle(&ctx);
        assert!(app.script.idle.is_some() && app.script.started.is_empty());
        // Then it starts once, as window 2's job, window 1 being current.
        app.script.running = false;
        app.tick_idle(&ctx);
        assert!(app.script.idle.is_none());
        assert_eq!(app.script.started, ["Idle"]);
        assert_eq!(app.w.id, FIRST);
        assert_eq!(app.script.owner_file, Some(77), "the job was window 2's");
    }

    /// A record of two, the first current with a draft, in window 1 of
    /// two.
    fn a_dirty_window_of_two() -> OpenMakerApp {
        let mut app = OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()));
        let mut session = Session::new(
            Target {
                host: "host".into(),
                file: "Tasks.fmp12".into(),
                local: None,
            },
            DEFAULT_PAGE_SIZE,
        );
        let key = session.select_layout(1);
        let record = |id: i64, name: &str| om_data::Record {
            id,
            values: vec![Some(name.into())],
        };
        let page = om_data::RecordPage {
            versions: Default::default(),
            fields: vec![om_data::Field {
                name: "Name".into(),
                kind: om_data::FieldKind::Text,
            }],
            records: vec![record(1, "original"), record(2, "second")],
            total: 2,
        };
        session.accept_page(key, Ok(page)).unwrap();
        app.w.session = Some(session);
        app.w.record_capabilities.create = true;
        app.add_window(2);
        app.w
            .document
            .put_draft(1, 1, "Name", "original".into(), "keep".into());
        app
    }

    /// A find that replaces the found set with the same counts is another
    /// context for the window's calculation reads (#987): the reads asked
    /// for the found set before it stop, as after any other find.
    #[test]
    fn a_find_keeping_the_counts_stops_the_reads_before_it() {
        let mut app = a_dirty_window_of_two();
        let find = |app: &mut OpenMakerApp| {
            let state = app.window_state();
            app.apply_window(
                &egui::Context::default(),
                WindowUpdate {
                    file: app.w.file_id,
                    state,
                    data_changed: true,
                    refreshed: false,
                    objects: Vec::new(),
                    scroll: Vec::new(),
                    fronted: Vec::new(),
                    originals: Default::default(),
                    found_set: Some(om_data::found::FoundSet {
                        found: 2,
                        total: 5,
                        sorted: false,
                    }),
                    background: false,
                },
            );
        };
        find(&mut app);
        let context = app.w.read_context();
        let reads = app.w.calcs.reads_for(context);
        find(&mut app);
        let after = app.w.read_context();
        app.w.calcs.reads_for(after);
        assert_ne!(after, context, "another found set, the same counts");
        assert!(reads.cancelled(), "the reads for the one before stop");
        // Not only through the page the find reloads: the found set's
        // revision alone is another context.
        let reads = app.w.calcs.reads_for(after);
        app.w.found_set_revision = app.w.found_set_revision.wrapping_add(1);
        app.w.calcs.reads_for(app.w.read_context());
        assert!(reads.cancelled(), "the revision alone stops the reads");
    }

    #[test]
    fn refresh_window_reloads_its_page_and_preserves_other_windows_and_drafts() {
        let mut app = a_dirty_window_of_two();
        app.w
            .document
            .put_draft(1, 1, "Name", "original".into(), String::new());
        app.w.document.active = Some("Name".into());
        let old = app.w.session.as_ref().unwrap().page.as_ref().unwrap().key;
        let other_page = app
            .w
            .session
            .as_ref()
            .unwrap()
            .page
            .as_ref()
            .unwrap()
            .data
            .clone();
        app.with_window(2, |a| {
            let session = a.w.session.as_mut().unwrap();
            let key = session.select_layout(1);
            session.accept_page(key, Ok(other_page)).unwrap();
        });
        let mut state = app.window_state();
        state.selection = Some((2, 3));
        let drafts = state.drafts.clone();
        app.apply_window(
            &egui::Context::default(),
            WindowUpdate {
                file: app.w.file_id,
                state,
                data_changed: false,
                refreshed: true,
                objects: Vec::new(),
                scroll: Vec::new(),
                fronted: Vec::new(),
                originals: [("Name".into(), Some("original".into()))].into(),
                found_set: None,
                background: false,
            },
        );
        assert_eq!(app.window_state().drafts, drafts);
        assert_eq!(app.w.document.active.as_deref(), Some("Name"));
        assert!(app.w.session.as_ref().unwrap().is_loading_records());
        assert!(!app
            .with_window(2, |a| a.w.session.as_ref().unwrap().is_loading_records())
            .unwrap());
        let session = app.w.session.as_mut().unwrap();
        assert!(
            session
                .accept_page(
                    old,
                    Ok(om_data::RecordPage {
                        versions: Default::default(),
                        fields: vec![],
                        records: vec![],
                        total: 0,
                    })
                )
                .unwrap()
                .is_none(),
            "a page in flight before refresh cannot replace the new page"
        );
    }

    #[test]
    fn targeted_handoff_retains_unrelated_repetition_through_script_completion() {
        use crate::calc::{Calculations, Evaluator};
        use om_data::calc::{Locale, Value, ValueType};
        use om_data::host::{CalcData, CalcNeed, CalcSchema, FieldDef, RecordValues, SessionInfo};
        use om_layout::{FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject, ObjectStore};
        use std::sync::Arc;
        const TABLE: u64 = 0x8001;
        const OCCURRENCE: u64 = 0xd00001;
        const LAYOUT_OCCURRENCE: u64 = 1065089;
        let mut app = a_dirty_window_of_two();
        app.w.document.clear_record(1, 1);
        app.w.globals.set("$$Context", 1, Value::text("unchanged"));
        let field = |id, repetition| LayoutObject {
            id,
            object_type: 1,
            field_obj: Some(FieldObject {
                field_spec: Some(FieldSpec {
                    table_key: LAYOUT_OCCURRENCE,
                    field_key: 1,
                    repetition,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let target = field(10, 1);
        let other = field(20, 2);
        app.w.session.as_mut().unwrap().render =
            crate::session::RenderState::Loaded(Box::new(om_data::RenderLayout {
                layout: Layout {
                    id: 1,
                    def: Some(LayoutDef {
                        table_alias_key: LAYOUT_OCCURRENCE,
                        ..Default::default()
                    }),
                    objects: ObjectStore {
                        objects: vec![target.clone(), other.clone()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                field_bindings: Default::default(),
            }));
        let mut schema = CalcSchema::new("Review", Locale::en_us());
        schema.add_table(TABLE, "Jobs");
        schema.add_occurrence(OCCURRENCE, "Jobs", TABLE);
        let mut def = FieldDef::data(1, "Title", ValueType::Text);
        def.repetitions = 2;
        schema.add_field(TABLE, def);
        app.w.calcs = Calculations::default();
        app.w.calcs.set_schema(Some(Arc::new(schema)));
        let data = |generation, rep2: &str| {
            let mut data = CalcData {
                generation: Some(generation),
                ..Default::default()
            };
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text("First"));
            values.set(1, 2, Value::text(rep2));
            data.records.insert((TABLE, 1), Some(Arc::new(values)));
            data
        };
        let needs = [CalcNeed::Record {
            table: TABLE,
            record: 1,
        }];
        app.w
            .calcs
            .accept_data(&needs, Ok(data(1, "Old unrelated repetition")));
        let read = |app: &OpenMakerApp| {
            let eval = Evaluator::new(
                &app.w.calcs,
                app.w.session.as_ref().unwrap(),
                &app.w.document,
                SessionInfo::default(),
                &app.w.globals,
            )
            .unwrap();
            eval.field_value_in_portal(other.id, other.field_spec().unwrap(), 1, None)
                .map(|value| eval.display(&value))
        };
        assert_eq!(read(&app).as_deref(), Some("Old unrelated repetition"));
        app.apply_window(
            &egui::Context::default(),
            WindowUpdate {
                file: app.w.file_id,
                state: app.window_state(),
                data_changed: false,
                refreshed: false,
                objects: vec![crate::scripting::ObjectRefresh {
                    layout: 1,
                    objects: vec![10],
                    repetition: 1,
                    join: None,
                    generation: Some(2),
                }],
                scroll: vec![],
                fronted: Vec::new(),
                originals: Default::default(),
                found_set: None,
                background: false,
            },
        );
        app.w
            .calcs
            .accept_data(&needs, Ok(data(2, "New unrelated repetition")));
        assert_eq!(
            read(&app).as_deref(),
            Some("Old unrelated repetition"),
            "GUI handoff must preserve an unrelated object's warm result"
        );
        let ctx = egui::Context::default();
        app.on_response(
            &ctx,
            crate::worker::Response {
                epoch: app.epoch,
                window: app.w.id,
                reply: crate::worker::Reply::Script(ScriptEvent::Globals {
                    file: app.w.file_id,
                    globals: Box::new(app.w.globals.clone()),
                }),
            },
        );
        app.on_script_event(
            &ctx,
            ScriptEvent::Finished {
                window: Box::new(WindowUpdate {
                    file: app.w.file_id,
                    state: app.window_state(),
                    data_changed: false,
                    refreshed: false,
                    objects: vec![],
                    scroll: vec![],
                    fronted: Vec::new(),
                    originals: Default::default(),
                    found_set: None,
                    background: false,
                }),
                globals: Box::new(app.w.globals.clone()),
                message: None,
            },
        );
        assert_eq!(
            read(&app).as_deref(),
            Some("Old unrelated repetition"),
            "unchanged Globals/Finished snapshots retain selective results"
        );
        let mut changed = app.w.globals.clone();
        changed.set("$$Context", 1, Value::text("changed"));
        app.on_response(
            &ctx,
            crate::worker::Response {
                epoch: app.epoch,
                window: app.w.id,
                reply: crate::worker::Reply::Script(ScriptEvent::Globals {
                    file: app.w.file_id,
                    globals: Box::new(changed),
                }),
            },
        );
        app.w
            .calcs
            .accept_data(&needs, Ok(data(2, "New unrelated repetition")));
        assert_eq!(
            read(&app).as_deref(),
            Some("New unrelated repetition"),
            "a changed variable context still invalidates completed results"
        );
    }

    #[test]
    fn targeted_refresh_keeps_pages_drafts_focus_and_other_windows() {
        let mut app = a_dirty_window_of_two();
        app.w.document.active = Some("Name".into());
        let before = app.window_state();
        let key = app.w.session.as_ref().unwrap().page.as_ref().unwrap().key;
        app.apply_window(
            &egui::Context::default(),
            WindowUpdate {
                file: app.w.file_id,
                state: before.clone(),
                data_changed: false,
                refreshed: false,
                objects: vec![crate::scripting::ObjectRefresh {
                    layout: 1,
                    objects: vec![10],
                    repetition: 0,
                    join: None,
                    generation: Some(1),
                }],
                scroll: vec![],
                fronted: Vec::new(),
                originals: [("Name".into(), Some("original".into()))].into(),
                found_set: None,
                background: false,
            },
        );
        assert_eq!(
            app.w.session.as_ref().unwrap().page.as_ref().unwrap().key,
            key
        );
        assert!(!app.w.session.as_ref().unwrap().is_loading_records());
        assert_eq!(app.window_state().drafts, before.drafts);
        assert_eq!(app.window_state().focus, before.focus);
        assert!(!app
            .with_window(2, |a| a.w.session.as_ref().unwrap().is_loading_records())
            .unwrap());
    }

    #[test]
    fn a_script_field_focus_reselects_an_instance_even_when_the_name_is_unchanged() {
        for scripted in [false, true] {
            let mut app = a_dirty_window_of_two();
            app.w.document.active = Some("Name".into());
            app.w.document.focus = None;
            let mut state = app.window_state();
            assert!(
                !state.field_focus,
                "UI snapshots represent the actual clicked object"
            );
            app.w.document.focus_first = scripted;
            state.field_focus = scripted;
            app.apply_window(
                &egui::Context::default(),
                WindowUpdate {
                    file: app.w.file_id,
                    state,
                    data_changed: false,
                    refreshed: false,
                    objects: Vec::new(),
                    scroll: Vec::new(),
                    fronted: Vec::new(),
                    originals: Default::default(),
                    found_set: None,
                    background: false,
                },
            );
            assert!(
                !app.w.document.focus_first,
                "an explicit focus request must take priority over a pending default first field"
            );
            assert_eq!(app.w.document.focus.as_deref(), scripted.then_some("Name"),
                "new scripted focus must be painted, while an unchanged captured focus stays on its object");
        }
    }

    #[test]
    fn a_button_held_for_field_exit_requires_its_original_document_context() {
        for change in [
            "none",
            "epoch",
            "window",
            "layout",
            "record",
            "closed",
            "mode",
            "view",
            "order",
            "worker",
            "worker-empty",
        ] {
            let mut app = a_dirty_window_of_two();
            let origin = app.w.id;
            let visible = app.w.show_toolbar;
            // The record the worker says the trigger's script left current:
            // another than the page still shows (#1073).
            let shown = app
                .w
                .session
                .as_ref()
                .and_then(|s| s.current_record())
                .map(|(_, r)| r.id);
            // Or none at all: the script left an empty found set.
            let worker = match change {
                "worker" => Some(Some(shown.unwrap_or_default() + 1)),
                "worker-empty" => Some(None),
                _ => None,
            };
            let after = ButtonAfterExit::capture(&app, Action::ToggleToolbar);
            let owner = app.key_owner();
            match change {
                "epoch" => app.epoch += 1,
                "mode" => app.w.mode = Mode::Find,
                "view" => app.w.view = ViewMode::List,
                "order" => {
                    let state = app.window_state();
                    app.apply_window(
                        &egui::Context::default(),
                        WindowUpdate {
                            file: app.w.file_id,
                            state,
                            data_changed: true,
                            refreshed: false,
                            objects: Vec::new(),
                            scroll: Vec::new(),
                            fronted: Vec::new(),
                            originals: Default::default(),
                            found_set: Some(om_data::found::FoundSet {
                                found: 2,
                                total: 2,
                                sorted: true,
                            }),
                            background: false,
                        },
                    );
                    assert!(app.w.session.as_ref().unwrap().is_loading_records());
                }
                "window" => {
                    app.bring_to_front(2);
                }
                "layout" => app.w.session.as_mut().unwrap().layout = Some(2),
                "record" => app.w.session.as_mut().unwrap().current = 1,
                "closed" => app.w.session = None,
                _ => {}
            }
            app.resume_held(
                &egui::Context::default(),
                origin,
                Held::Exit(("Name".into(), None), true, 3, Some(after), owner, None),
                true,
                worker,
            );
            let expected = if change == "none" { !visible } else { visible };
            assert_eq!(
                app.with_window(origin, |a| a.w.show_toolbar),
                Some(expected),
                "{change}"
            );
            if change == "window" {
                assert_eq!(app.w.id, 2);
                assert_eq!(
                    app.w.show_toolbar, visible,
                    "the other window takes no click"
                );
            }
        }
    }

    #[test]
    fn a_held_list_button_checks_the_clicked_record_after_a_page_refresh() {
        let mut app = a_dirty_window_of_two();
        let page = |second, third| om_data::RecordPage {
            versions: Default::default(),
            fields: vec![om_data::Field {
                name: "Name".into(),
                kind: om_data::FieldKind::Text,
            }],
            records: [1, second, third]
                .into_iter()
                .map(|id| om_data::Record {
                    id,
                    values: vec![Some(id.to_string())],
                })
                .collect(),
            total: 3,
        };
        let session = app.w.session.as_mut().unwrap();
        let key = session.refresh().unwrap();
        session.accept_page(key, Ok(page(22, 33))).unwrap();
        let after = ButtonAfterExit::capture(
            &app,
            Action::RunButton {
                object_id: 7,
                record_index: 1,
                portal: None,
            },
        );
        assert!(after.is_current(&app));
        let session = app.w.session.as_mut().unwrap();
        let key = session.refresh().unwrap();
        session.accept_page(key, Ok(page(33, 22))).unwrap();
        assert_eq!(
            app.w
                .session
                .as_ref()
                .unwrap()
                .current_record()
                .unwrap()
                .1
                .id,
            1
        );
        assert!(
            !after.is_current(&app),
            "the target row changed while the current record did not"
        );
    }

    /// A script's Close File closes the file once the script has ended,
    /// when the worker has sent every window's last state: the script
    /// stays busy until then, and a Quit chosen meanwhile, or the script's
    /// own Exit Application, quits after the close.
    #[test]
    fn a_script_close_file_waits_for_the_script_and_quits_after() {
        for exit_application in [false, true] {
            let ctx = egui::Context::default();
            let mut app = crate::app::tests::two_record_windows();
            app.windows[0].file_id = app.w.file_id;
            app.script.running = true;
            app.on_script_event(&ctx, ScriptEvent::CloseFile);
            if exit_application {
                app.on_script_event(&ctx, ScriptEvent::Quit);
            } else {
                app.apply(&ctx, Action::Quit);
            }
            assert_eq!(app.window_ids(), [FIRST, 2], "{exit_application}");
            assert!(app.script.busy(), "{exit_application}");
            let update = WindowUpdate {
                file: 0,
                state: app.window_state(),
                data_changed: false,
                refreshed: false,
                objects: Vec::new(),
                scroll: Vec::new(),
                fronted: Vec::new(),
                originals: Default::default(),
                found_set: None,
                background: false,
            };
            app.on_script_event(
                &ctx,
                ScriptEvent::Finished {
                    window: Box::new(update),
                    globals: Box::default(),
                    message: None,
                },
            );
            assert!(
                app.docs().all(|doc| doc.session.is_none()),
                "{exit_application}"
            );
            assert!(
                app.quit_ready || app.quit_waiting.is_some(),
                "{exit_application}"
            );
        }
    }

    /// A close trigger's Close File or Exit Application takes over the close
    /// it holds even after its script closed the trigger's window, and
    /// another held action's veto (the halt's, say) leaves it.
    #[test]
    fn a_close_takeover_survives_its_window_and_other_vetoes() {
        let ctx = egui::Context::default();
        let mut app = a_dirty_window_of_two();
        app.script
            .held
            .insert(7, (FIRST, Held::Action(Action::CloseWindow, 1)));
        app.remove_window(FIRST);
        assert!(app.take_over_close(None, true));
        let closing = app.closing.clone().expect("taken over");
        assert!(closing.quiet && closing.quit);
        assert_eq!(closing.windows, [2]);
        assert!(app.script.held.is_empty() && app.script.taken_over);
        app.resume_held(&ctx, 2, Held::Action(Action::GoToRecord(1), 1), false, None);
        assert_eq!(app.closing.as_ref(), Some(&closing));
        // A veto from a window that has closed since leaves it too.
        app.resume_held(
            &ctx,
            FIRST,
            Held::Action(Action::GoToRecord(1), 1),
            false,
            None,
        );
        assert_eq!(app.closing, Some(closing));
    }

    /// An action of the user's held for its trigger script does not go on
    /// once the connection is lost, whether the trigger let it before or
    /// after the loss: closing (a window's close box, File > Close, Quit)
    /// would close the window without its save and discard its edit, and
    /// the others are refused offline.
    #[test]
    fn a_lost_connection_drops_the_actions_held_for_triggers() {
        let closing = |quit| (Action::CloseWindow, quit);
        for (action, quit) in [
            closing(None),
            closing(Some(false)),
            closing(Some(true)),
            (Action::GoToRecord(1), None),
            (Action::SelectLayout(2), None),
            (Action::NewRecord, None),
        ] {
            for approved_before_loss in [true, false] {
                let what = format!("{action:?}, closing {quit:?}, before {approved_before_loss}");
                let ctx = egui::Context::default();
                let mut app = a_dirty_window_of_two();
                app.script.running = true;
                app.closing = quit.map(|quit| app.closing_front(quit));
                app.script
                    .held
                    .insert(1, (FIRST, Held::Action(action.clone(), 1)));
                let approve = ScriptEvent::Action {
                    token: 1,
                    proceed: true,
                    record: None,
                };
                if approved_before_loss {
                    app.on_script_event(&ctx, approve);
                    app.lose_connection(&om_data::DataError::connection_lost("gone"));
                } else {
                    app.lose_connection(&om_data::DataError::connection_lost("gone"));
                    app.on_script_event(&ctx, approve);
                }
                // The trigger script ends, stopped by the loss.
                let update = WindowUpdate {
                    file: 0,
                    state: app.window_state(),
                    data_changed: false,
                    refreshed: false,
                    objects: Vec::new(),
                    scroll: Vec::new(),
                    fronted: Vec::new(),
                    originals: Default::default(),
                    found_set: None,
                    background: false,
                };
                app.on_script_event(
                    &ctx,
                    ScriptEvent::Finished {
                        window: Box::new(update),
                        globals: Box::default(),
                        message: None,
                    },
                );
                assert_eq!(app.window_ids(), [FIRST, 2], "{what}");
                assert!(!app.quit_ready, "{what}");
                let session = app.w.session.as_ref().expect("the file is open");
                assert_eq!((session.layout, session.current), (Some(1), 0), "{what}");
                assert_eq!(app.w.document.unsaved(), 1, "the edit went; {what}");
            }
        }
    }
}
