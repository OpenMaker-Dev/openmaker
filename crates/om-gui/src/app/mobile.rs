//! OpenMaker Go (`mobile/`): a file's window inside a phone or tablet
//! app's own chrome. The window draws the layout as on the desktop; the
//! platform's toolbars and menus take the place of its menu bar and status
//! toolbar (docs/mobile-client.md).
//!
//! The chrome's menus are FileMaker's own: the layout menu, Browse's
//! Records or Find's Requests, a View menu of the modes and views FileMaker
//! Go has, and Scripts (`menubar::go_menus`). They are recorded each frame
//! through `menu_record.rs`, as the macOS system menu bar records them, so
//! that a row reads, enables and runs as it would in the window's own menu.
//! The chrome's controls that are not menu rows (the record arrows, Find
//! mode's buttons, Quick Find, Done and Close) are [`ChromeCommand`]s, each
//! with the availability its control shows. FileMaker Go's chrome is
//! described in docs/re/go-client.md.

use super::*;
use crate::menu_record::{Recorder, Row};

/// A control of OpenMaker Go's chrome that is not a row of FileMaker's
/// menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeCommand {
    /// The previous record, or in Find mode the previous request.
    Previous,
    /// The next record, or in Find mode the next request.
    Next,
    /// Browse's record of this number in the found set, from 1.
    GoToRecord(u64),
    /// Find mode (Go's Create New Find).
    Find,
    /// Find mode's Search, FileMaker's Perform Find.
    PerformFind,
    /// Back to Browse mode without finding.
    CancelFind,
    /// The current request's Omit Matching.
    ToggleOmit,
    /// Quick Find for these words.
    QuickFind(String),
    /// The editing toolbar's Done: the field is left and the record's
    /// changes are saved, as a tap outside every field saves them
    /// (go-client.md, "Field editing").
    Done,
    /// Close File.
    Close,
}

/// A row of one of the chrome's menus. The rows after a submenu row whose
/// depth is greater than its own are that submenu's.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuRow {
    /// What choosing the row sends back ([`OpenMakerApp::choose_menu_row`]).
    pub id: String,
    pub depth: u32,
    pub kind: MenuRowKind,
    pub label: String,
    pub enabled: bool,
    pub checked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuRowKind {
    Command,
    Submenu,
    Separator,
}

/// One of the chrome's menus.
#[derive(Clone, Debug, PartialEq)]
pub struct ChromeMenu {
    pub title: String,
    pub rows: Vec<MenuRow>,
}

/// What the chrome shows of the window.
#[derive(Clone, Debug, PartialEq)]
pub struct ChromeState {
    pub title: String,
    pub mode: Mode,
    pub layout: String,
    /// Browse's record (from 0) of `count` found, or Find's request of
    /// `count` requests.
    pub position: u64,
    pub count: u64,
    /// Find's current request omits what it matches.
    pub omitting: bool,
    /// A script runs, or a record or page is on its way.
    pub busy: bool,
    /// The record has changes not yet saved, or their save is on its way,
    /// or what was typed still waits for the record (its lock), or a window
    /// that lost its connection holds changes
    /// or one held for the user's choice ([`OpenMakerApp::changes_held`]).
    pub unsaved: bool,
    /// What was typed waits for the record: nothing can save it yet.
    pub awaiting_record: bool,
}

/// The chrome's own state, kept between frames.
#[derive(Default)]
pub(crate) struct Chrome {
    /// A command for the next frame, with the window it was chosen for.
    command: Option<(u32, ChromeCommand)>,
    /// A menu row chosen since the menus were recorded, with its window.
    chosen: Option<(u32, String)>,
    /// The field the menus were last recorded for: a field command chosen
    /// in them runs only while it is still the one entered.
    shown_target: Option<FieldMenuTarget>,
    /// A read of the Scripts menu's catalog failed: the menu asks again
    /// only after the user's next choice, not every frame.
    pub(super) scripts_failed: bool,
    menus: Vec<ChromeMenu>,
}

impl OpenMakerApp {
    /// The app as OpenMaker Go's window, with no menu bar or status toolbar:
    /// the chrome around it shows [`Self::chrome_menus`] and
    /// [`Self::chrome_state`] instead.
    pub fn mobile_document(mut self) -> Self {
        self.mobile = Some(Box::default());
        self
    }

    /// Open a file on the device, with the account in `file` if any.
    pub fn open_device_file(&mut self, ctx: &egui::Context, file: om_data::LocalFile) {
        self.open_local(ctx, file);
    }

    /// The file being opened asks for an account and password.
    pub fn needs_account(&self) -> bool {
        self.local_login.is_some()
    }

    /// The error the window shows, with FileMaker's error code when the
    /// failure that raised it had one.
    pub fn failure(&self) -> Option<(Option<i32>, &str)> {
        let shown = self.error()?;
        Some((self.shown_failure().and_then(|failure| failure.code), shown))
    }

    /// The failure the window shows is a host whose certificate could not
    /// be verified: what to ask the user before connecting anyway.
    pub fn untrusted(&self) -> Option<&om_data::UntrustedHost> {
        self.shown_failure()?.untrusted.as_deref()
    }

    /// The last failure reported, while the window still shows it.
    fn shown_failure(&self) -> Option<&om_data::DataError> {
        let shown = self.error()?;
        self.failure
            .as_ref()
            .filter(|failure| failure.message == shown)
    }

    /// A window holds changes not saved to the host that nothing saves
    /// until the user acts: its connection was lost with them (its fields',
    /// its related records', a script's uncommitted record or a new record
    /// not yet recreated), and they wait for a reconnect; a reconnect found
    /// the host's copy changed and asks to keep or discard them; or the
    /// host refused the record to text typed into it, which waits for
    /// Discard Input. Close File would discard them.
    pub fn changes_held(&self) -> bool {
        std::iter::once(&self.w).chain(&self.windows).any(|window| {
            let offline = window.connection_lost.is_some()
                && (window.document.unsaved() > 0
                    || !window.document.all_related_edits().is_empty()
                    || window.uncommitted.is_some()
                    || window.unrecreated.is_some());
            offline
                || window.document.failed_edit()
                || window
                    .recovery
                    .as_ref()
                    .is_some_and(recovery::Recovery::holds)
        })
    }

    /// Whether any window, shown or hidden, holds changes not saved to the
    /// host: a record's changes or their save on its way
    /// ([`Self::draft_state`]), what was typed waiting for its record, or
    /// changes held for the user ([`Self::changes_held`]). A window behind
    /// the front one can keep a draft whose save the host refused.
    pub fn unsaved_anywhere(&mut self) -> bool {
        if self.changes_held() {
            return true;
        }
        let ids: Vec<u32> = std::iter::once(self.w.id)
            .chain(self.windows.iter().map(|window| window.id))
            .collect();
        ids.into_iter().any(|id| {
            self.with_window(id, |app| {
                app.draft_state().is_some() || app.w.document.preparing_edit()
            })
            .unwrap_or(false)
        })
    }

    /// The text of the layout field being edited, as the last pass left
    /// it; `None` when the keyboard focus is in no layout field (a dialog's
    /// field, or none).
    pub fn focused_field_text(&self, ctx: &egui::Context) -> Option<String> {
        crate::document::focused_field_text(ctx)
    }

    /// A Tab or a Return that goes on to the next field is still moving
    /// the front window's focus: the field it goes to takes it in a later
    /// pass (bounded, for a portal row's field that has yet to draw), or a
    /// keystroke trigger has yet to let it through.
    pub fn focus_moving(&self) -> bool {
        self.w.document.tab_in_progress() || self.navigation_key_pending()
    }

    /// The visible rectangles of the scroll areas inside the layout (a
    /// portal's rows) the last pass drew: a screen reader pages the one
    /// it is in by its own size.
    pub fn scroll_areas(&self, ctx: &egui::Context) -> Vec<egui::Rect> {
        crate::document::scroll_areas(ctx)
    }

    /// The file the front window shows, by its id in this app.
    pub fn front_file(&self) -> u64 {
        self.front_doc().file_id
    }

    /// Whether a window of file `file` is still open.
    pub fn file_open(&self, file: u64) -> bool {
        std::iter::once(&self.w)
            .chain(&self.windows)
            .any(|window| window.file_id == file && window.session.is_some())
    }

    /// Whether any file is open, in a window hidden or shown: a script may
    /// have opened one hidden, which stays open when the shown ones close.
    pub fn any_file_open(&self) -> bool {
        std::iter::once(&self.w)
            .chain(&self.windows)
            .any(|window| window.session.is_some())
    }

    /// Bring a window of an open file to the front when the front one has
    /// none (a hidden file left after the shown ones closed), so that it
    /// can be closed as Close File closes it. False when no file is open.
    pub fn bring_open_file_forward(&mut self) -> bool {
        if self.w.session.is_some() {
            return true;
        }
        let Some(id) = self
            .windows
            .iter()
            .find(|window| window.session.is_some())
            .map(|window| window.id)
        else {
            return false;
        };
        self.bring_to_front(id)
    }

    /// The window's record edit as OpenMaker Go saves it: `Some(false)`
    /// while the record has changes not committed, `Some(true)` while their
    /// save is on its way, `None` when there are none or nothing can save
    /// them yet ([`Self::changes_held`]). Unlike
    /// [`Self::record_edit_state`], a reconnect's note that only reports
    /// (the host's copy had not changed: save or revert as usual) does not
    /// hide a record that saves as usual.
    pub fn draft_state(&self) -> Option<bool> {
        let held = self
            .w
            .recovery
            .as_ref()
            .is_some_and(recovery::Recovery::holds);
        (self.w.session.is_some()
            && self.w.connection_lost.is_none()
            && !held
            && !self.w.document.failed_edit()
            && (self.record_modified() || self.w.document.pending))
            .then_some(self.w.document.pending)
    }

    /// Something the window does or asks for is under way: a save on its
    /// way in any window, or what [`Self::lifecycle_blocked`] names.
    /// OpenMaker Go's save and close wait until none is before reading
    /// what they left.
    pub fn lifecycle_busy(&self) -> bool {
        self.lifecycle_blocked()
            || std::iter::once(&self.w)
                .chain(&self.windows)
                .any(|window| window.document.pending)
    }

    /// No window can be asked to save now: a script (paused included) or
    /// its prompt, a dialog over the window, an edit waiting for its
    /// record, a close or a reconnect is under way. Saves on their way do
    /// not block another window's: the host answers each on its own.
    pub fn lifecycle_blocked(&self) -> bool {
        self.dialog_over_document() || self.lifecycle_held()
    }

    /// What blocks OpenMaker Go's save now is only the app's alert: one
    /// window's save the host refused (a validation failure) says so and
    /// waits for the user. Windows behind the front one can still be asked
    /// ([`Self::save_window`]), so that it strands no other window's
    /// changes.
    pub fn lifecycle_blocked_by_alert(&self) -> bool {
        self.error.is_some() && !self.dialog_open_besides_error() && !self.lifecycle_held()
    }

    fn lifecycle_held(&self) -> bool {
        self.script.running
            || self.script.prompt.is_some()
            || self.w.document.preparing_edit()
            || self.closing.is_some()
            || self.reconnect_pending()
    }

    /// The windows whose record has changes not committed that can be
    /// saved now ([`Self::draft_state`]), the front one first: one behind
    /// it may hold a record a script set a field in before selecting
    /// another window. Hidden windows are included.
    pub fn windows_with_drafts(&mut self) -> Vec<u32> {
        let front = self.front_doc().id;
        let mut ids: Vec<u32> = std::iter::once(front)
            .chain(
                self.windows
                    .iter()
                    .map(|window| window.id)
                    .filter(|id| *id != front),
            )
            .collect();
        ids.retain(|id| {
            self.with_window(*id, |app| app.draft_state() == Some(false))
                .unwrap_or(false)
        });
        ids
    }

    /// Save window `id`'s record as Done in it does: a shown front
    /// window's on the next frame, where what its field holds is taken
    /// first; one behind it, or a hidden one (it draws no frame), at once,
    /// without bringing it forward, also while the app's alert says another
    /// window's save was refused ([`Self::lifecycle_blocked_by_alert`]).
    /// False when it cannot now (a dialog, a script, a save on its way).
    pub fn save_window(&mut self, ctx: &egui::Context, id: u32) -> bool {
        let front = self.front_doc();
        if id == front.id && !front.hidden {
            return self.run_chrome_command(ChromeCommand::Done);
        }
        self.with_window(id, |app| {
            let action = if app.lifecycle_blocked_by_alert() {
                let idle = app.w.session.is_some()
                    && !app.w.document.pending
                    && !app.script.busy()
                    && matches!(app.w.mode, Mode::Browse | Mode::Find);
                idle.then_some(Action::CommitRecord)
            } else {
                app.chrome_action(&ChromeCommand::Done)
            };
            let Some(action) = action else {
                return false;
            };
            app.apply(ctx, action);
            true
        })
        .unwrap_or(false)
    }

    /// Close the front window's file as File > Close does (its triggers,
    /// the record's commit): a shown window's on the next frame, where what
    /// its field holds is taken first; a hidden one's at once, as it draws
    /// no frame (a script opened its file hidden). False when it cannot
    /// now.
    pub fn close_front_file(&mut self, ctx: &egui::Context) -> bool {
        if !self.w.hidden {
            return self.run_chrome_command(ChromeCommand::Close);
        }
        let Some(action) = self.chrome_action(&ChromeCommand::Close) else {
            return false;
        };
        self.apply(ctx, action);
        true
    }

    /// Open the file again after its connection was lost, signing in with
    /// `password` as the banner's Reconnect does: unsaved changes stay in
    /// their windows and are checked against the host's records. The
    /// password goes only to `host` and `port` for `account`, where it was
    /// typed; a file opened elsewhere asks for its own. `false` when the
    /// window cannot start it yet (a script runs).
    pub fn reconnect_lost(
        &mut self,
        ctx: &egui::Context,
        host: &str,
        port: u16,
        account: &str,
        password: &str,
    ) -> bool {
        self.reconnect_with(ctx, (host.to_owned(), port, account.to_owned()), password)
    }

    /// The chrome's menus as of the last frame.
    pub fn chrome_menus(&self) -> &[ChromeMenu] {
        self.mobile.as_ref().map_or(&[], |chrome| &chrome.menus)
    }

    pub fn chrome_state(&self) -> ChromeState {
        let session = self.w.session.as_ref();
        let find = self.w.mode == Mode::Find;
        let (position, count) = if find {
            (self.w.find.current_index() as u64, self.w.find.len() as u64)
        } else {
            session.map_or((0, 0), |s| (s.current, s.total().unwrap_or(0)))
        };
        ChromeState {
            title: self.window_title(),
            mode: self.w.mode,
            layout: session
                .and_then(Session::layout_name)
                .unwrap_or_default()
                .to_owned(),
            position,
            count,
            omitting: find && self.w.find.current().omit,
            busy: self.script.busy()
                || self.w.document.pending
                || session.is_some_and(|s| s.pending.is_some()),
            unsaved: self.draft_state().is_some()
                || self.w.document.preparing_edit()
                || self.changes_held(),
            awaiting_record: self.w.document.preparing_edit(),
        }
    }

    /// Whether `command` can run now: its control is enabled.
    pub fn chrome_command_available(&self, command: &ChromeCommand) -> bool {
        self.chrome_action(command).is_some()
    }

    /// Run `command` on the next frame, as its control does. False when it
    /// cannot run now.
    pub fn run_chrome_command(&mut self, command: ChromeCommand) -> bool {
        if !self.chrome_command_available(&command) {
            return false;
        }
        let window = self.w.id;
        if let Some(chrome) = &mut self.mobile {
            chrome.command = Some((window, command));
            chrome.scripts_failed = false;
        }
        true
    }

    /// Choose the row of the chrome's menus with this id: it runs on the
    /// next frame if it is still there and enabled, in the window whose
    /// menus showed it. A row's id names its window: one chosen from a
    /// window's menus after another came to the front (a script selected
    /// it) is dropped, though the same row is there.
    pub fn choose_menu_row(&mut self, id: String) {
        let Some((window, row)) = id
            .split_once(':')
            .and_then(|(window, row)| Some((window.parse().ok()?, row.to_owned())))
        else {
            return;
        };
        if let Some(chrome) = &mut self.mobile {
            chrome.chosen = Some((window, row));
            chrome.scripts_failed = false;
        }
    }

    fn chrome_action(&self, command: &ChromeCommand) -> Option<Action> {
        self.w.session.as_ref()?;
        // As the window's menus and toolbar: nothing while a dialog is over
        // the window, a script runs or a record is being saved. Close waits
        // too while what was typed waits for the record: File > Close would
        // cancel that edit, and the app closes its file when it goes away,
        // which must keep what was typed.
        if self.dialog_over_document()
            || self.script.busy()
            || self.script.prompt.is_some()
            || self.w.document.pending
            || self.w.document.preparing_edit()
        {
            return None;
        }
        let state = self.chrome_state();
        let (position, count) = (state.position, state.count);
        let browse = self.w.mode == Mode::Browse;
        let find = self.w.mode == Mode::Find;
        // Mode changes and Perform Find, as View and Requests enable them.
        let full = self.full_menus();
        Some(match command {
            ChromeCommand::Previous if find && position > 0 => {
                Action::GoToRequest(position as usize - 1)
            }
            ChromeCommand::Next if find && position + 1 < count => {
                Action::GoToRequest(position as usize + 1)
            }
            ChromeCommand::Previous if browse && position > 0 => Action::GoToRecord(position - 1),
            ChromeCommand::Next if browse && position + 1 < count => {
                Action::GoToRecord(position + 1)
            }
            ChromeCommand::GoToRecord(number) if browse && (1..=count).contains(number) => {
                Action::GoToRecord(number - 1)
            }
            ChromeCommand::Find if browse && full => Action::SetMode(Mode::Find),
            ChromeCommand::PerformFind if find && full => Action::PerformFind,
            ChromeCommand::CancelFind if find && full => Action::SetMode(Mode::Browse),
            ChromeCommand::ToggleOmit if find => Action::SetOmit(!state.omitting),
            ChromeCommand::QuickFind(words)
                if browse
                    && full
                    && !words.trim().is_empty()
                    && self
                        .w
                        .session
                        .as_ref()
                        .is_some_and(crate::toolbar::layout_quick_find) =>
            {
                Action::QuickFind(words.clone())
            }
            // In Find mode the commit has no record: only the field is left.
            ChromeCommand::Done if browse || find => Action::CommitRecord,
            // Not while a window holds changes only the user can let go
            // of: closing would discard them. The lost banner's own Close
            // File does, when the user chooses it there.
            ChromeCommand::Close if !self.changes_held() => Action::CloseFile,
            _ => return None,
        })
    }

    /// Record the chrome's menus for this frame, and the actions of a
    /// command or menu row chosen since the last.
    pub(super) fn mobile_menus(&mut self, ui: &mut Ui, input: bool, actions: &mut Vec<Action>) {
        // The chrome's state is moved out while the menus are recorded, but
        // `self.mobile` stays set: the menus read it to leave out what Go
        // does not have (Script Workspace).
        let Some(mut chrome) = self.mobile.as_deref_mut().map(std::mem::take) else {
            return;
        };
        let window = self.w.id;
        // Every window is drawn in the root's pass, a window's cards on it:
        // the chrome shows the front window's menus (its card's, when one is
        // in front) and runs its commands. Another window drawn after it
        // leaves them be.
        if window != self.front_doc().id {
            if let Some(kept) = self.mobile.as_deref_mut() {
                *kept = chrome;
            }
            return;
        }
        let ctx = ui.ctx().clone();
        let ran = actions.len();
        // A command or row sent for a window no longer in front is dropped,
        // as a click in a menu that has closed.
        if let Some((_, command)) = chrome.command.take().filter(|(at, _)| *at == window) {
            // Checked again: the window may have changed since it was sent.
            actions.extend(self.chrome_action(&command));
        }
        let blocked = !input || self.menus_blocked(ui);
        let target = self.field_menu_target(&ctx);
        let editor = self
            .current_layout_record()
            .and_then(|(layout, record)| self.w.document.entered_editor(layout, record))
            .filter(|id| target.focused == Some(*id));
        let (mut chosen, shown) = match chrome.chosen.take() {
            Some((at, id)) if at == window => (Some(id), chrome.shown_target.take()),
            _ => (None, None),
        };
        let may_ask = !chrome.scripts_failed;
        let mut recorded = Vec::new();
        let mut picked = Vec::new();
        self.with_menu_context(&ctx, input, true, editor, |context| {
            menubar::go_menus(context, &mut picked, &mut |title, rows| {
                let mut recorder = Recorder::new(title.to_owned(), &mut chosen, may_ask, blocked);
                rows(&mut recorder);
                let mut flat = Vec::new();
                flatten(&recorder.rows, 0, window, &mut flat);
                recorded.push(ChromeMenu {
                    title: title.to_owned(),
                    rows: flat,
                });
            });
        });
        Self::native_menu_actions(picked, shown, &target, editor, actions);
        // A command or row that ran changes what the window shows over the
        // frames after it (Find mode's editors, a layout's records), as a
        // click in its own menus would, but comes with no input that makes
        // egui draw them: the window asks.
        if actions.len() > ran {
            ctx.request_repaint();
        }
        chrome.menus = recorded;
        chrome.shown_target = Some(target);
        if let Some(kept) = self.mobile.as_deref_mut() {
            *kept = chrome;
        }
    }
}

/// The recorded rows in order, each row's id prefixed with its window's
/// ([`OpenMakerApp::choose_menu_row`]).
fn flatten(rows: &[Row], depth: u32, window: u32, out: &mut Vec<MenuRow>) {
    for row in rows {
        match row {
            Row::Item {
                id,
                label,
                checked,
                enabled,
                ..
            } => out.push(MenuRow {
                id: format!("{window}:{id}"),
                depth,
                kind: MenuRowKind::Command,
                label: label.clone(),
                enabled: *enabled,
                checked: *checked,
            }),
            Row::Separator => out.push(MenuRow {
                id: String::new(),
                depth,
                kind: MenuRowKind::Separator,
                label: String::new(),
                enabled: false,
                checked: false,
            }),
            Row::Submenu {
                id,
                label,
                enabled,
                rows,
            } => {
                out.push(MenuRow {
                    id: format!("{window}:{id}"),
                    depth,
                    kind: MenuRowKind::Submenu,
                    label: label.clone(),
                    enabled: *enabled,
                    checked: false,
                });
                flatten(rows, depth + 1, window, out);
            }
        }
    }
}
