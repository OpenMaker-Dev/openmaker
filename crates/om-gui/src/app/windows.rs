//! The file's document windows (script-runtime.md "Implementation notes
//! (desktop client)", Windows). Each window has its own layout, found set,
//! mode, view, current record and unsaved values, Find mode requests,
//! toolbars and title: a [`Doc`]. The app works on one at a time
//! (`OpenMakerApp::w`); the others wait in `OpenMakerApp::windows` and are
//! swapped in to be drawn (each in a viewport of its own) or to take a
//! reply to a request they made. Scripts open, select and close windows
//! on the worker, which tells the window by its id.

use super::{content, Action, FindState, LayoutTool, Mode, OpenMakerApp, ViewMode};
use crate::scripting::{PortalRow, Uncommitted, WindowStyle};
use crate::session::Session;
use crate::toolbar::ToolbarState;
use om_data::RecordCapabilities;

/// The id of the file's first window.
pub(crate) const FIRST: u32 = 1;

/// Native presentation is separate from the worker's current file/window.
/// A delivered request stays through Finished so its caller snapshot cannot
/// raise the opener again. Later user input supersedes it. `last` marks a
/// one-shot presentation, gone once delivered: the one a finished script
/// leaves, so a script started meanwhile does not inherit it, and a prompt's
/// or pause's, after which the script's windows come forward as it works
/// in them.
#[derive(Clone, Copy)]
pub(super) enum Presentation {
    Window { id: u32, pending: bool, last: bool },
    User,
}

/// Whether a window has its file's whole script catalog (`Doc::scripts`).
/// It is read the first time the Scripts menu shows it, not at open: a
/// script a trigger or button runs is read alone (`DataProvider::script`),
/// and FileMaker Pro 26 reads no whole catalog at open (#1053).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ScriptsRead {
    #[default]
    Unread,
    Asked,
    Read,
}

/// One document window.
pub(crate) struct Doc {
    pub id: u32,
    pub file_id: u64,
    pub hidden: bool,
    pub scripts: om_data::ScriptCatalog,
    pub scripts_read: ScriptsRead,
    pub globals: om_data::calc::eval::Variables,
    pub connection_lost: Option<String>,
    pub timer: Option<(std::time::Duration, std::time::Instant)>,
    pub session: Option<Session>,
    pub mode: Mode,
    pub view: ViewMode,
    pub find: FindState,
    pub toolbar: ToolbarState,
    pub show_toolbar: bool,
    pub show_formatting: bool,
    pub tool: LayoutTool,
    pub inspector: bool,
    pub objects: bool,
    pub layout_workspace: content::LayoutWorkspace,
    pub record_capabilities: RecordCapabilities,
    /// The shown layout's capabilities while another layout loads, put
    /// back with it if the connection is lost meanwhile.
    pub shown_capabilities: Option<RecordCapabilities>,
    pub deferred_action: Option<Action>,
    /// The menus' New Record or Duplicate Record runs as its step: once
    /// that job ends with the focus still on the step's first field, the
    /// focus is the first field object painted in the tab order
    /// (`DocumentState::focus_first`), a hidden one passed over. Any later
    /// job of the window clears it.
    pub new_record_focus: bool,
    /// The record write the window waits for (`document.pending`), and the
    /// one a lost connection left unanswered: whether the host has it is
    /// unknown until the file reconnects (recovery.rs).
    pub in_flight: Option<super::recovery::Write>,
    pub unanswered: Option<super::recovery::Write>,
    /// What the window tells the user after its file reconnected, and the
    /// edits it checked (and holds until the user keeps or discards them).
    pub recovery: Option<super::recovery::Recovery>,
    /// The window's save that may be partly applied, as the provider
    /// reports it (`DataProvider::unsettled_save`, #1006): the record is
    /// frozen until it is checked, resumed or discarded.
    pub unsettled: Option<Vec<om_data::FileCommit>>,
    /// Why the window's new record a reconnect carried could not be made
    /// again (#152): kept as it was, frozen, until the user tries again or
    /// discards it.
    pub unrecreated: Option<String>,
    /// The records the window holds unsaved edits of, whose saves answer
    /// to the version it showed when the edits began
    /// (`DataProvider::retain_record`), with the values it showed then by
    /// field name (when it had the record's page): what a refused save
    /// compares the host's copy with (recovery.rs).
    pub retained: std::collections::BTreeMap<(u32, i64), Option<super::recovery::Values>>,
    /// Its file reconnected: it reloads its layout and records once the
    /// file's fresh layout list is in.
    pub reconnected: bool,
    /// The save on its way sets only global fields, the session's values:
    /// it writes no record (no OnWindowTransaction).
    pub globals_only: bool,
    pub delete_record: bool,
    /// The layout Delete Layout asks to delete, while its alert is open.
    pub delete_layout: Option<u32>,
    /// The selected portal row Delete Record asks to delete.
    pub delete_portal_row: Option<crate::document::SelectedRow>,
    /// The found set's size and sort after a find, omit or sort.
    pub found_set: Option<om_data::found::FoundSet>,
    /// A worker find, omit or sort invalidates captured row positions before
    /// the replacement record page arrives, even when the counts are equal.
    pub found_set_revision: u64,
    pub document: crate::document::DocumentState,
    /// Set Window Title (a new window's name) and Show/Hide Menubar. Set
    /// Zoom Level sets the window's document zoom.
    pub title: Option<String>,
    pub menubar: bool,
    /// The view the window last showed each layout in, the current one's
    /// included (FileMaker Pro 26 keeps a view per layout in each window,
    /// #805): going back to a layout shows it in its entry; a layout not
    /// shown before takes the view its definition stores when it loads.
    /// Window state only: nothing of it is written to the file.
    pub views: std::collections::BTreeMap<u32, ViewMode>,
    /// OnLayoutEnter is due when this layout has loaded.
    pub pending_enter: Option<u32>,
    /// A layout this window added: it opens in Layout mode once the
    /// window's layout list has it.
    pub new_layout: Option<u32>,
    /// The layout to show once the one Delete Layout deletes is gone.
    pub after_delete: Option<u32>,
    /// That layout, being opened: Layout mode follows once it is shown.
    pub layout_mode_for: Option<u32>,
    /// The active portal row a script left, on its layout and record.
    pub portal: Option<(u32, i64, PortalRow)>,
    /// What scripts left on a record of the window that no commit has
    /// kept, on its layout and record: the next script starts from it,
    /// the user's save commits it and the user's revert drops it (on the
    /// worker, `Start::Commit` and `Start::Revert`).
    pub uncommitted: Option<(u32, i64, Uncommitted)>,
    /// Structured refusal of a user-invoked worker commit, processed only
    /// after that job's final window state has been applied.
    pub worker_save_failure: Option<(u32, i64, om_data::DataError)>,
    /// The file's calculation schema and the data the window's
    /// calculations read (its found sets are the window's).
    pub calcs: crate::calc::Calculations,
    /// Find mode's requests when the window last left it (`Get (
    /// RequestCount )`).
    pub requests: u64,
    /// The window's `Get ( WindowUUID )`.
    pub uuid: String,
    /// Where the layout showed in the last frame (window coordinates), the
    /// height of the status toolbar and layout bar above it, and where the
    /// window was on the desktop when it was last drawn in its viewport
    /// (the window selectors, calc-functions.md 6.3 O1).
    pub content_rect: Option<egui::Rect>,
    pub bars: f32,
    /// The menu bar was drawn in the last frame, and its height when it
    /// last was (kept while it is hidden).
    pub menu_drawn: bool,
    pub menu_height: f32,
    /// Adjust Window [Resize to Fit] that waits for the content area to be
    /// measured (#1115).
    pub fit: Option<crate::scripting::Fit>,
    pub placement: Option<crate::desktop::Placement>,
    /// Document, dialog or card (window_styles.rs).
    pub style: WindowStyle,
    /// Keys on their way through the keystroke triggers.
    pub(super) keys: super::scripts::Keys,
}

impl Doc {
    /// What this window's calculation reads are for: its layout and page
    /// revision, its view, and its found set: its counts and each find,
    /// omit or sort that replaced it (`found_set_revision`), also one that
    /// keeps the counts. When it changes, the reads asked before stop
    /// (#987).
    pub fn read_context(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // The revision of the page asked for or shown: a layout switch or a
        // refresh moves it, scrolling to another page of it does not.
        self.session
            .as_ref()
            .map(|session| {
                let revision = session
                    .pending
                    .iter()
                    .chain(session.page.as_ref().map(|page| &page.key))
                    .map(|key| key.revision)
                    .max();
                (revision, session.layout)
            })
            .hash(&mut hasher);
        (self.view as u8).hash(&mut hasher);
        self.found_set
            .map(|found| (found.found, found.total, found.sorted))
            .hash(&mut hasher);
        // Not only through the page the change reloads: a found set replaced
        // is another context whatever else the window keeps.
        self.found_set_revision.hash(&mut hasher);
        hasher.finish()
    }

    /// What the window asks the provider as it closes: a local file keeps
    /// the found set and record it shows (`Request::KeepFoundSet`).
    pub(super) fn found_set_to_keep(&self) -> Option<(u32, crate::worker::Request)> {
        let session = self.session.as_ref()?;
        let layout_id = session.layout?;
        let record = session.current_record().map(|(_, r)| r.id);
        Some((
            self.id,
            crate::worker::Request::KeepFoundSet { layout_id, record },
        ))
    }

    pub fn new(id: u32) -> Doc {
        Doc {
            id,
            file_id: 0,
            hidden: false,
            scripts: Default::default(),
            scripts_read: ScriptsRead::Unread,
            globals: Default::default(),
            connection_lost: None,
            timer: None,
            session: None,
            mode: Mode::Browse,
            view: ViewMode::default(),
            find: FindState::default(),
            toolbar: ToolbarState::default(),
            show_toolbar: true,
            show_formatting: false,
            tool: LayoutTool::default(),
            inspector: true,
            objects: false,
            layout_workspace: content::LayoutWorkspace::default(),
            record_capabilities: RecordCapabilities::default(),
            shown_capabilities: None,
            deferred_action: None,
            new_record_focus: false,
            in_flight: None,
            unanswered: None,
            recovery: None,
            unsettled: None,
            unrecreated: None,
            retained: Default::default(),
            reconnected: false,
            globals_only: false,
            delete_record: false,
            delete_layout: None,
            delete_portal_row: None,
            found_set: None,
            found_set_revision: 0,
            document: crate::document::DocumentState::for_window(id),
            title: None,
            menubar: true,
            views: Default::default(),
            pending_enter: None,
            new_layout: None,
            after_delete: None,
            layout_mode_for: None,
            portal: None,
            uncommitted: None,
            worker_save_failure: None,
            calcs: crate::calc::Calculations::default(),
            requests: 0,
            uuid: om_data::host::window::new_window_uuid(),
            content_rect: None,
            bars: 0.0,
            menu_drawn: false,
            menu_height: 0.0,
            fit: None,
            placement: None,
            style: WindowStyle::Document,
            keys: Default::default(),
        }
    }

    /// Keep the window's view as its current layout's entry in
    /// [`Doc::views`]: View As chose it.
    pub fn remember_view(&mut self) {
        if let Some(layout) = self.session.as_ref().and_then(|s| s.layout) {
            self.views.insert(layout, self.view);
        }
    }

    /// Whether the window holds changes made on layout `layout` that the
    /// host has not saved: the layout's own draft in Layout mode, or a
    /// record's edits made through it, the user's or what scripts left.
    /// They are kept by layout: deleting it would strand them.
    pub fn unsaved_on_layout(&self, layout: u32) -> bool {
        let shown = self.session.as_ref().and_then(|s| s.layout) == Some(layout);
        (shown && self.layout_workspace.has_changes())
            || self
                .document
                .drafted_records()
                .iter()
                .any(|(l, _)| *l == layout)
            || self.document.related_edited_on(layout)
            || self
                .uncommitted
                .as_ref()
                .is_some_and(|(l, _, _)| *l == layout)
    }
}

/// The viewport that shows window `id` (the file's first window is the
/// root).
pub(crate) fn viewport(id: u32) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("openmaker-window", id))
}

impl OpenMakerApp {
    /// The file's windows' ids: the front one first, then the others from
    /// the most recently in front.
    pub(super) fn window_ids(&self) -> Vec<u32> {
        std::iter::once(&self.w)
            .chain(self.windows.iter().rev())
            .filter(|d| !d.hidden)
            .map(|d| d.id)
            .collect()
    }

    /// Run `f` with window `id`'s state as the app's; afterwards the
    /// window that was in front is again, found by its id. `f` may bring
    /// another window to the front (the Window menu), close windows or
    /// close the file: when the window it works on is no longer the app's,
    /// what `f` left stands, and a front window `f` closed is not looked
    /// for. `None` when the file has no window `id`.
    pub(super) fn with_window<R>(&mut self, id: u32, f: impl FnOnce(&mut Self) -> R) -> Option<R> {
        if id == self.w.id {
            return Some(f(self));
        }
        let front = self.w.id;
        let index = self.windows.iter().position(|d| d.id == id)?;
        std::mem::swap(&mut self.w, &mut self.windows[index]);
        let result = f(self);
        if self.w.id == id {
            if let Some(index) = self.windows.iter().position(|d| d.id == front) {
                std::mem::swap(&mut self.w, &mut self.windows[index]);
            }
        }
        Some(result)
    }

    /// Bring window `id` to the front: the app works on it from now on.
    pub(super) fn bring_to_front(&mut self, id: u32) {
        if id == self.w.id {
            return;
        }
        let Some(index) = self.windows.iter().position(|d| d.id == id) else {
            return;
        };
        if self.windows[index].file_id != self.w.file_id {
            self.workspace = None;
            self.database = None;
            self.security = None;
            self.value_lists = None;
            self.manage_layouts = None;
            self.workspace_followups.clear();
        }
        let front = self.windows.remove(index);
        let previous = std::mem::replace(&mut self.w, front);
        self.windows.push(previous);
    }

    /// A script opened window `id` (New Window): it starts as a window of
    /// the file on its first layout, and the script's update sets it.
    pub(super) fn add_window(&mut self, id: u32) {
        let mut doc = Doc::new(id);
        doc.file_id = self.w.file_id;
        doc.scripts = self.w.scripts.clone();
        doc.scripts_read = self.w.scripts_read;
        doc.globals = self.w.globals.clone();
        doc.connection_lost = self.w.connection_lost.clone();
        if let Some(session) = &self.w.session {
            let mut new = Session::new(session.target.clone(), session.page_size);
            new.paper = session.paper.clone();
            new.layouts = session.layouts.clone();
            new.layout_menu = session.layout_menu.clone();
            new.hidden_layouts = session.hidden_layouts.clone();
            new.layouts_loading = false;
            doc.session = Some(new);
        }
        doc.show_toolbar = self.w.show_toolbar;
        doc.calcs.share_schema(&self.w.calcs);
        doc.calcs.share_reports(&self.w.calcs);
        doc.calcs.account.clone_from(&self.w.calcs.account);
        doc.calcs.privileges.clone_from(&self.w.calcs.privileges);
        doc.calcs
            .host_version
            .clone_from(&self.w.calcs.host_version);
        self.window_files.insert(doc.id, doc.file_id);
        self.windows.push(doc);
    }

    fn add_file_window(&mut self, id: u32, file: u64) {
        if self.closed_windows.contains(&id) {
            return;
        }
        let owner = std::iter::once(&self.w)
            .chain(&self.windows)
            .find(|d| d.file_id == file)
            .map(|d| d.id);
        if let Some(owner) = owner {
            self.with_window(owner, |app| app.add_window(id));
        }
    }

    /// Close Window closed window `id`: another window comes to the front
    /// when it was the front one, a card's parent or a dialog's opener
    /// first. A card left without its parent goes with it, so nothing it
    /// does can reach another window.
    pub(super) fn remove_window(&mut self, id: u32) {
        if matches!(self.presentation, Some(Presentation::Window { id: target, .. }) if target == id)
        {
            self.presentation = None;
        }
        if let Some(doc) = std::iter::once(&self.w)
            .chain(&self.windows)
            .find(|d| d.id == id)
        {
            self.window_files.insert(id, doc.file_id);
            // Its calculation reads stop with it (#987); other windows keep
            // theirs.
            doc.calcs.cancel_reads();
        }
        self.closed_windows.insert(id);
        if id == self.w.id {
            let parent = self.w.style.parent();
            let next = parent
                .and_then(|p| self.windows.iter().position(|d| d.id == p))
                .or_else(|| {
                    self.windows
                        .iter()
                        .rposition(|d| d.file_id == self.w.file_id && d.style.card().is_none())
                })
                .or_else(|| {
                    self.windows
                        .iter()
                        .rposition(|d| d.file_id == self.w.file_id)
                })
                .or_else(|| self.windows.iter().rposition(|d| !d.hidden))
                .map(|i| self.windows.remove(i));
            let Some(next) = next else {
                return;
            };
            self.w = next;
            if self
                .security
                .as_ref()
                .is_some_and(|s| !s.owns(self.w.file_id))
            {
                self.security = None;
            }
            if self
                .value_lists
                .as_ref()
                .is_some_and(|d| !d.owns(self.w.file_id))
            {
                self.value_lists = None;
            }
            if self
                .workspace
                .as_ref()
                .is_some_and(|w| !w.owns(self.w.file_id))
            {
                self.workspace = None;
                self.workspace_followups.clear();
            }
        } else {
            self.windows.retain(|d| d.id != id);
        }
        if let Some(card) = self.card_of(id) {
            self.remove_window(card);
        }
    }

    /// Every window but the front one, least recently in front first, as
    /// a script finds them.
    pub(super) fn other_window_states(&mut self) -> Vec<crate::scripting::WindowState> {
        let ids: Vec<u32> = self
            .windows
            .iter()
            .filter(|d| d.file_id == self.w.file_id)
            .map(|d| d.id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.with_window(id, |app| app.window_state()))
            .collect()
    }

    /// Records changed through the window the app works on: the file's
    /// other windows read their records and calculations again, as they
    /// may show the same ones.
    pub(super) fn refresh_other_windows(&mut self, ctx: &egui::Context) {
        let current = self.w.id;
        for id in self.window_ids().into_iter().filter(|id| *id != current) {
            let file = self.w.file_id;
            self.with_window(id, |app| {
                if app.w.file_id != file {
                    return;
                }
                app.w.calcs.data_changed();
                let key = app.w.session.as_mut().and_then(Session::refresh);
                app.request_page(ctx, key);
            });
        }
    }

    /// [`Self::record_edit_state`] of the file's windows, the front one
    /// first.
    pub fn record_edit_states(&mut self) -> Vec<Option<bool>> {
        self.window_ids()
            .into_iter()
            .filter_map(|id| self.with_window(id, |app| app.record_edit_state()))
            .collect()
    }

    /// The titles of the file's windows, the front one first.
    pub fn window_titles(&mut self) -> Vec<String> {
        self.window_ids()
            .into_iter()
            .filter_map(|id| self.with_window(id, |app| app.window_title()))
            .collect()
    }

    /// The window's name as `Get ( WindowName )` and `WindowNames` give
    /// it: Set Window Title's or New Window's, else the file's name.
    pub(super) fn window_name_for_get(&self) -> String {
        self.w.title.clone().unwrap_or_else(|| {
            let file = self
                .w
                .session
                .as_ref()
                .map(|s| s.target.file.as_str())
                .unwrap_or_default();
            om_data::file_refs::file_stem(file).to_owned()
        })
    }

    /// The file's first remaining window, which the root viewport shows
    /// (never a card: it shows on its parent).
    pub(super) fn root_window(&self) -> u32 {
        self.window_ids()
            .into_iter()
            .filter(|id| !self.is_card(*id))
            .min()
            .unwrap_or(FIRST)
    }

    /// A reply to a window's request goes to that window; a script's
    /// events about its windows open, update, bring to the front and close
    /// them. Only the window the script works in comes to the front: a
    /// change to another one (its title) is shown where that window is.
    pub(super) fn on_response(&mut self, ctx: &egui::Context, response: crate::worker::Response) {
        use crate::scripting::ScriptEvent;
        use crate::worker::Reply;
        let window = response.window;
        match response.reply {
            // Related files opened on demand grew the file's schema while
            // the provider answered: every window of the file takes it
            // before the answer, which may read the new files' tables.
            Reply::Grown { grown, then } => {
                let target = if self.has_window(window) {
                    window
                } else {
                    let file = self.window_files.get(&window);
                    std::iter::once(&self.w)
                        .chain(&self.windows)
                        .find(|d| Some(&d.file_id) == file)
                        .map_or(window, |d| d.id)
                };
                self.with_window(target, |app| {
                    app.accept_calc_schema(grown.result, grown.warnings, grown.sign_ins, true)
                });
                self.on_response(
                    ctx,
                    crate::worker::Response {
                        epoch: response.epoch,
                        window,
                        reply: *then,
                    },
                );
            }
            // The file's connection is lost for every window, whichever
            // window's request or script noticed it (perhaps one closed
            // since).
            Reply::ConnectionLost(err) => {
                self.with_window(window, |app| app.lose_connection(&err));
            }
            // The reconnect's file may have more windows than the one that
            // asked (perhaps one closed since).
            Reply::Reconnected(result) => self.reconnected(ctx, *result),
            Reply::Script(ScriptEvent::FileOpened(opened)) => self.opened_peer(ctx, *opened),
            Reply::Script(ScriptEvent::FileClosed(file)) => {
                let ids: Vec<_> = std::iter::once(&self.w)
                    .chain(&self.windows)
                    .filter(|d| d.file_id == file)
                    .map(|d| d.id)
                    .collect();
                for id in ids {
                    self.remove_window(id);
                }
            }
            Reply::Script(ScriptEvent::Unsettled { files, saves }) => {
                self.unsettled_after_script(ctx, &files, &saves)
            }
            Reply::Script(ScriptEvent::Globals { file, globals }) => {
                for doc in std::iter::once(&mut self.w)
                    .chain(&mut self.windows)
                    .filter(|d| d.file_id == file)
                {
                    if doc.globals.revision() != globals.revision() {
                        doc.globals = (*globals).clone();
                        doc.calcs.data_changed();
                        if let Some(session) = &doc.session {
                            session.report_data_changed();
                        }
                    }
                }
            }
            Reply::Script(ScriptEvent::Window(update)) => {
                if update.data_changed {
                    for doc in std::iter::once(&mut self.w)
                        .chain(&mut self.windows)
                        .filter(|d| d.file_id != update.file)
                    {
                        doc.calcs.data_changed();
                        doc.document.clear_layout_cache();
                    }
                }
                let id = update.state.id;
                if self.closed_windows.contains(&id) {
                    return;
                }
                if !self.has_window(id) && self.w.connection_lost.is_none() {
                    self.add_file_window(id, update.file);
                }
                // Once the connection is lost the windows stay as they are
                // (`apply_window`), in front or behind.
                if update.background
                    || self.presentation.is_some()
                    || self.window_hidden(id)
                    || self.w.connection_lost.is_some()
                {
                    self.with_window(id, |app| {
                        app.on_script_event(ctx, ScriptEvent::Window(update))
                    });
                } else {
                    // The script works in this window: it is in front.
                    self.bring_to_front(id);
                    self.on_script_event(ctx, ScriptEvent::Window(update));
                }
            }
            Reply::Script(ScriptEvent::Finished {
                window: update,
                globals,
                message,
            }) => {
                let id = update.state.id;
                if self.closed_windows.contains(&id) {
                    self.script.running = false;
                    self.script.paused = None;
                    self.script.prompt = None;
                    self.presentation = None;
                    return;
                }
                if !self.has_window(id) && self.w.connection_lost.is_none() {
                    self.add_file_window(id, update.file);
                }
                match self.presentation {
                    Some(Presentation::User) => self.presentation = None,
                    None if !self.window_hidden(id) => {
                        self.presentation = Some(Presentation::Window {
                            id,
                            pending: true,
                            last: true,
                        });
                    }
                    Some(Presentation::Window {
                        id, pending: false, ..
                    }) => {
                        if !self.window_hidden(id) && self.has_window(id) {
                            self.bring_to_front(id);
                        }
                        self.presentation = None;
                    }
                    Some(Presentation::Window {
                        id, pending: true, ..
                    }) => {
                        self.presentation = Some(Presentation::Window {
                            id,
                            pending: true,
                            last: true,
                        });
                    }
                    None => {}
                }
                self.with_window(id, |app| {
                    app.on_script_event(
                        ctx,
                        ScriptEvent::Finished {
                            window: update,
                            globals,
                            message,
                        },
                    )
                });
            }
            Reply::Script(ScriptEvent::CloseWindow(id)) => self.remove_window(id),
            // A script that stopped, or closed its file, after its window
            // went: the app's script state ends all the same (else every
            // window's actions would wait behind it), and the front window
            // says why it stopped (#1247).
            Reply::Script(ScriptEvent::Failed(message)) if !self.has_window(window) => {
                self.on_script_event(ctx, ScriptEvent::Failed(message));
            }
            Reply::Script(ScriptEvent::CloseFile) if !self.has_window(window) => {
                self.script.running = false;
                self.script.owner_file = None;
                self.script.paused = None;
                self.script.prompt = None;
            }
            Reply::Script(event @ ScriptEvent::ClosePopover { .. }) => {
                self.with_window(window, |app| app.on_script_event(ctx, event));
            }
            Reply::Script(event @ ScriptEvent::Prompt { .. }) => {
                // A modal belongs to its requesting window, which comes to
                // the front so it can be answered: in front of the Script
                // Workspace a Run started from, of a peer a visible
                // Open File presented, and of a window the user brought
                // forward while the script ran. Not over another file's
                // window with that file's Script Workspace or Manage
                // dialogs open: bringing the window forward would close
                // them unsaved, so it waits there, as it did before, unless
                // the script itself presented a window.
                self.with_window(window, |app| app.on_script_event(ctx, event));
                let presented = matches!(self.presentation, Some(Presentation::Window { .. }));
                if presented || !self.presenting_closes_dialogs(window) {
                    // X11 does not focus a minimized window.
                    let viewport = self.viewport_of(window);
                    if ctx.input_for(viewport, |i| i.viewport().minimized == Some(true)) {
                        ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Minimized(false));
                    }
                    self.present_owner(window);
                }
            }
            Reply::Script(event @ ScriptEvent::Paused { .. }) => {
                // The pause bar is the front window's, and Continue goes on
                // in the window it was clicked in: after a visible Open File
                // the paused script's own window comes forward, or the rest
                // of the caller would run in the opened file.
                self.with_window(window, |app| app.on_script_event(ctx, event));
                if matches!(self.presentation, Some(Presentation::Window { .. })) {
                    self.present_owner(window);
                }
            }
            Reply::Script(ScriptEvent::Viewport(egui::ViewportCommand::Focus)) => {
                self.presentation = Some(Presentation::Window {
                    id: window,
                    pending: true,
                    last: false,
                });
            }
            // A card has no window of its own to move (its parent's would).
            Reply::Script(ScriptEvent::Viewport(command)) if !self.is_card(window) => {
                if sizes(&command) {
                    self.with_window(window, |app| app.w.fit = None);
                }
                ctx.send_viewport_cmd_to(self.viewport_of(window), command);
            }
            Reply::Script(ScriptEvent::Viewport(_)) => {}
            Reply::Script(event) => {
                self.with_window(window, |app| app.on_script_event(ctx, event));
            }
            reply => {
                let file_dialog = matches!(
                    reply,
                    Reply::Scripts { .. }
                        | Reply::ScriptSaved { .. }
                        | Reply::ScriptCatalogSaved { .. }
                        | Reply::Schema { .. }
                        | Reply::ValueLists { .. }
                        | Reply::CalcSchema { .. }
                        // A related file signed in to: the file's windows
                        // take the schema with its tables.
                        | Reply::RelatedSignIn { .. }
                        // Its answer makes every window of the file read its
                        // layouts again, also after the sender closed.
                        | Reply::LayoutSetUp(_)
                        | Reply::PartOptionsSet(..)
                        | Reply::LayoutPartsSet(..)
                );
                let window = if file_dialog && !self.has_window(window) {
                    let file = self.window_files.get(&window);
                    std::iter::once(&self.w)
                        .chain(&self.windows)
                        .find(|d| Some(&d.file_id) == file)
                        .map_or(window, |d| d.id)
                } else {
                    window
                };
                self.with_window(window, |app| app.on_reply(ctx, reply));
            }
        }
    }

    fn has_window(&self, id: u32) -> bool {
        self.w.id == id || self.windows.iter().any(|d| d.id == id)
    }
    fn window_hidden(&self, id: u32) -> bool {
        if self.w.id == id {
            self.w.hidden
        } else {
            self.windows.iter().any(|d| d.id == id && d.hidden)
        }
    }

    fn opened_peer(&mut self, ctx: &egui::Context, opened: crate::scripting::OpenedFile) {
        let id = opened.window.id;
        if self.has_window(id) {
            for doc in std::iter::once(&mut self.w)
                .chain(&mut self.windows)
                .filter(|d| d.file_id == opened.id)
            {
                doc.hidden = opened.hidden;
            }
            return;
        }
        let mut doc = Doc::new(id);
        doc.file_id = opened.id;
        doc.hidden = opened.hidden;
        if let Some(catalog) = opened.catalog {
            doc.scripts = catalog;
            doc.scripts_read = ScriptsRead::Read;
        }
        // The worker read the file's schema before opening it: none means
        // it has no calculations, not that they are still on their way.
        match opened.schema {
            Some(schema) => doc.calcs.set_schema(Some(schema)),
            None => doc.calcs.schema_unavailable(),
        }
        doc.calcs.account = opened.context.account;
        doc.calcs.privileges = opened.privileges;
        doc.calcs.host_version = opened.host_version;
        let mut session = Session::new(
            crate::session::Target {
                host: opened.context.host,
                file: opened.context.file,
                local: None,
            },
            crate::session::DEFAULT_PAGE_SIZE,
        );
        session.set_layouts_for(opened.layouts, doc.calcs.schema().map(AsRef::as_ref));
        // The layout the file opened on, the blank page when the account
        // may not see it (#696).
        session.open_on(opened.window.layout);
        doc.session = Some(session);
        self.window_files.insert(doc.id, doc.file_id);
        self.windows.push(doc);
        self.with_window(id, |app| {
            let key = app.w.session.as_ref().and_then(|s| s.pending);
            app.request_layout(ctx, key);
        });
    }

    /// A prompt or pause of window `window` after a presentation request:
    /// the window comes forward once so the user answers it there. Nothing
    /// is left after: the script's windows then come forward as it works in
    /// them, as without a request.
    /// Whether bringing window `id` forward would close dialogs of the
    /// front window's file (`bring_to_front` closes them when another
    /// file's window comes forward): its Script Workspace, Manage Database,
    /// Manage Security, Manage Value Lists or Manage Layouts.
    fn presenting_closes_dialogs(&self, id: u32) -> bool {
        let base = self.base_window(id);
        let file = std::iter::once(&self.w)
            .chain(&self.windows)
            .find(|d| d.id == base)
            .map(|d| d.file_id);
        file.is_some_and(|file| file != self.w.file_id)
            && (self.workspace.is_some()
                || self.database.is_some()
                || self.security.is_some()
                || self.value_lists.is_some()
                || self.manage_layouts.is_some())
    }

    fn present_owner(&mut self, window: u32) {
        self.presentation = Some(Presentation::Window {
            id: window,
            pending: true,
            last: true,
        });
    }

    /// Once all visible viewports have been created/drawn, deliver the last
    /// presentation request once. State-only worker replies never repeat it.
    pub(super) fn present_window(&mut self, ctx: &egui::Context) {
        let Some(Presentation::Window {
            id,
            pending: true,
            last,
        }) = self.presentation
        else {
            return;
        };
        if !self.has_window(id) {
            // New Window can announce selection before its first snapshot.
            if self.closed_windows.contains(&id) || !self.script.running {
                self.presentation = None;
            } else {
                ctx.request_repaint();
            }
            return;
        }
        if self.window_hidden(id) || self.w.connection_lost.is_some() {
            self.presentation = None;
            return;
        }
        let id = self.blocking_dialog(id).unwrap_or(id);
        let id = *self.card_chain(id).last().expect("one window");
        self.bring_to_front(id);
        ctx.send_viewport_cmd_to(self.viewport_of(id), egui::ViewportCommand::Focus);
        self.presentation = (self.script.running && !last).then_some(Presentation::Window {
            id,
            pending: false,
            last: false,
        });
    }

    /// The viewport window `id` is drawn in: a card's is its parent's.
    pub(super) fn viewport_of(&self, id: u32) -> egui::ViewportId {
        let id = self.base_window(id);
        if id == self.root_window() {
            egui::ViewportId::ROOT
        } else {
            viewport(id)
        }
    }
}

/// A window command that gives the window its size, which replaces an
/// Adjust Window [Resize to Fit] still waiting for a paint, as a later step
/// replaces an earlier one's size (#1115).
pub(super) fn sizes(command: &egui::ViewportCommand) -> bool {
    matches!(
        command,
        egui::ViewportCommand::InnerSize(_)
            | egui::ViewportCommand::Maximized(true)
            | egui::ViewportCommand::Fullscreen(true)
    )
}
