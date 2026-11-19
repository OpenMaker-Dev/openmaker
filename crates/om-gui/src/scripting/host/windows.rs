//! The file's windows on the worker (script-runtime.md "Implementation
//! notes (desktop client)", Windows; FileMaker Server's answers in 13.25,
//! battery s35-windows): each window has its own layout, found set (the
//! provider's, `DataProvider::use_window`), mode, view and current record.
//! The script works on the current window ([`super::super::session::Shared::window`]);
//! the others wait, the most recently current last.
//!
//! - New Window opens a window showing what the current one shows (its
//!   layout, found set, record, mode and view), named as given, else the
//!   current window's name and " - 2" (" - 3", ... when taken), and makes
//!   it current; with a layout it goes there (OnLayoutEnter runs).
//! - Select Window makes the named window current; names are matched
//!   front to back and may repeat. No such window is 112.
//! - Close Window commits the window's record and closes it; the window
//!   that was current before it becomes current. The file's last window
//!   closes the file.
//!
//! Window styles (Claris, "About window styles"; script-steps.md 11.8):
//! - A Card belongs to the window current when it opened (its parent),
//!   which can have only one. Closing a card makes its parent current;
//!   closing a parent closes its card first, and a card that cannot close
//!   (its record refuses to commit) fails the step with both open.
//! - A Dialog returns to the window it was opened from. While one is open
//!   New Window opens only dialogs and cards, and Select Window selects
//!   only the current window.
//! - Floating Document and unmapped style words are not supported: the
//!   script stops before the window opens.

use om_data::runtime::{
    Bounds, ErrorCode, HostError, HostResult, WindowOptions, WindowStyle as Requested, WindowTarget,
};

use super::super::window::Window;
use super::super::{Card, ScriptEvent, WindowStyle};
use super::WorkerHost;

/// What the window steps find a window by when they take one by name
/// (Claris, `select-window.html`: "first searches for a window with a
/// matching name. If none are found, it then searches for a window with a
/// matching UUID"; script-runtime.md 9.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowKey {
    Name,
    /// `Get ( WindowUUID )`.
    Uuid,
}

/// Whether a window's name or UUID is `wanted`: "Window name or UUID
/// selection is not case sensitive" (Claris). Case is folded as Unicode
/// lowercase; how FileMaker folds letters beyond ASCII is unmeasured.
pub(super) fn same_window_key(key: &str, wanted: &str) -> bool {
    key == wanted || key.to_lowercase() == wanted.to_lowercase()
}

impl WorkerHost<'_> {
    /// The provider uses the current window's found sets.
    pub(crate) fn use_current_window(&mut self) {
        let id = self.s.file.window.id();
        self.p().use_window(id);
    }

    pub(crate) fn reveal_window(&mut self) {
        if self.s.file.window.state.hidden {
            self.s.file.window.state.hidden = false;
            self.s.file.window.touch();
        }
    }

    /// The file's name, which names its first window.
    fn file_title(&self) -> String {
        let file = &self.s.file.context.file;
        om_data::file_refs::file_stem(file).to_owned()
    }

    fn name_of(&self, window: &Window) -> String {
        window
            .state
            .title
            .clone()
            .unwrap_or_else(|| self.file_title())
    }

    /// The windows' names, front to back (`WindowNames`).
    pub(super) fn window_names(&self) -> Vec<String> {
        std::iter::once(&self.s.file.window)
            .chain(self.s.file.others.iter().rev())
            .map(|w| self.name_of(w))
            .collect()
    }

    /// The window `name` names, front to back: `None` the current one,
    /// else its place among the others; 112 when none has the name. By
    /// its name, else by its `Get ( WindowUUID )` ([`WindowKey`]).
    pub(super) fn named(&self, name: &str) -> Result<Option<usize>, HostError> {
        [WindowKey::Name, WindowKey::Uuid]
            .into_iter()
            .find_map(|key| self.window_by(key, name))
            .ok_or(HostError::Error(ErrorCode::WINDOW_MISSING))
    }

    /// The window of this file whose `key` is `name`, front to back, as
    /// [`Self::named`] answers.
    pub(super) fn window_by(&self, key: WindowKey, name: &str) -> Option<Option<usize>> {
        let matches = |window: &Window| match key {
            WindowKey::Name => same_window_key(&self.name_of(window), name),
            WindowKey::Uuid => same_window_key(&window.state.uuid, name),
        };
        if matches(&self.s.file.window) {
            return Some(None);
        }
        (0..self.s.file.others.len())
            .rev()
            .find(|&i| matches(&self.s.file.others[i]))
            .map(Some)
    }

    fn target(&self, target: &WindowTarget) -> Result<Option<usize>, HostError> {
        match target {
            WindowTarget::Current => Ok(None),
            WindowTarget::Named(name) => self.named(name),
        }
    }

    /// Tell the UI what changed in the current window before another one
    /// becomes current (or a window [`Self::on_window`] works on goes back
    /// to waiting): it stays where it is in the UI.
    pub(super) fn flush_background(&mut self) {
        if let Some(mut update) = self.s.file.window.take_update(false) {
            update.background = true;
            update.file = self.s.file.id;
            if update.data_changed {
                self.invalidate_peer_calculations();
            }
            self.send(ScriptEvent::Window(Box::new(update)));
        }
    }

    /// Make the waiting window `index` current; the current one waits,
    /// most recently current.
    pub(super) fn make_current(&mut self, index: usize) {
        self.flush_background();
        let mut window = self.s.file.others.remove(index);
        window.reenter();
        let previous = std::mem::replace(&mut self.s.file.window, window);
        self.s.file.others.push(previous);
        self.use_current_window();
        *self.s.file.cache.borrow_mut() = Default::default();
        self.sync_layout(true);
    }

    /// The file's windows, the current one first.
    fn all_windows(&self) -> impl Iterator<Item = &Window> {
        std::iter::once(&self.s.file.window).chain(&self.s.file.others)
    }

    fn window_by_id(&self, id: u32) -> Option<&Window> {
        self.all_windows().find(|w| w.id() == id)
    }

    /// The card open on window `id`.
    fn card_of(&self, id: u32) -> Option<u32> {
        self.all_windows()
            .find(|w| w.state.style.card().is_some_and(|c| c.parent == id))
            .map(Window::id)
    }

    pub(super) fn dialog_open(&self) -> bool {
        self.all_windows().any(|w| w.state.style.is_dialog())
            || self.files.peers.values().any(|file| {
                std::iter::once(&file.state.window)
                    .chain(&file.state.others)
                    .any(|w| w.state.style.is_dialog())
            })
    }

    /// The style a new window opens with, checked before anything
    /// happens: the current window becomes a card's parent or a dialog's
    /// opener.
    pub(super) fn new_window_style(
        &self,
        options: WindowOptions,
        bounds: Bounds,
    ) -> HostResult<WindowStyle> {
        let current = self.s.file.window.id();
        match options.style {
            Requested::Unknown(_) => Err(HostError::Unsupported),
            // Claris: "Any operations that attempt to open a document or
            // floating document window will fail when a dialog window is
            // open" (OpenMaker answers 3).
            Requested::Document if self.dialog_open() => Err(ErrorCode::UNAVAILABLE.into()),
            Requested::Document => Ok(WindowStyle::Document),
            Requested::Dialog => Ok(WindowStyle::Dialog { opener: current }),
            // "You cannot open more than one card from the same parent
            // window" (OpenMaker answers 3).
            Requested::Card if self.card_of(current).is_some() => {
                Err(ErrorCode::UNAVAILABLE.into())
            }
            Requested::Card => Ok(WindowStyle::Card(Card {
                parent: current,
                dim: options.dim_parent,
                close: options.close,
                left: bounds.left,
                top: bounds.top,
                width: bounds.width,
                height: bounds.height,
            })),
        }
    }

    /// Where card `card` shows, from its parent's layout area as the UI
    /// last drew it.
    fn card_placement(&self, card: &Card) -> crate::desktop::Placement {
        self.window_by_id(card.parent)
            .and_then(|w| w.state.placement.clone())
            .unwrap_or_default()
            .card(card)
    }

    /// Move/Resize Window of a card: its bounds on its parent change (no
    /// window of its own moves). `false` for other windows.
    pub(super) fn move_resize_card(&mut self, bounds: Bounds) -> bool {
        let WindowStyle::Card(card) = &mut self.s.file.window.state.style else {
            return false;
        };
        let set = |slot: &mut Option<f64>, value: Option<f64>| {
            if value.is_some() {
                *slot = value;
            }
        };
        set(&mut card.left, bounds.left);
        set(&mut card.top, bounds.top);
        set(&mut card.width, bounds.width);
        set(&mut card.height, bounds.height);
        let card = *card;
        self.s.file.window.state.placement = Some(self.card_placement(&card));
        self.s.file.window.touch();
        true
    }

    /// While a dialog is open, Select Window selects only the current
    /// window (Claris: "Can select only the current dialog window").
    pub(super) fn check_dialog_selection(&self, target: &WindowTarget) -> HostResult {
        if !self.dialog_open() {
            return Ok(());
        }
        match target {
            WindowTarget::Current => Ok(()),
            WindowTarget::Named(name) if matches!(self.named(name), Ok(None)) => Ok(()),
            WindowTarget::Named(_) => Err(ErrorCode::UNAVAILABLE.into()),
        }
    }

    /// New Window: a window named `name` (else after the current one)
    /// showing what the current one shows becomes current, in `style`.
    pub(super) fn open_window(&mut self, name: Option<String>, style: WindowStyle) -> HostResult {
        self.commit_window()?;
        let names = self.window_names();
        let name = name.filter(|n| !n.is_empty()).unwrap_or_else(|| {
            let base = self.name_of(&self.s.file.window);
            (2..)
                .map(|n| format!("{base} - {n}"))
                .find(|n| !names.contains(n))
                .expect("a free name")
        });
        let id = self.files.allocate_window();
        let from = self.s.file.window.id();
        let mut window = self.s.file.window.opened_from(id, name);
        window.state.hidden = false;
        window.state.style = style;
        if let WindowStyle::Card(card) = &style {
            // Cards have no status toolbar (Claris).
            window.state.toolbar = false;
            window.state.placement = Some(self.card_placement(card));
        }
        self.p().copy_window(from, id);
        self.flush_background();
        let previous = std::mem::replace(&mut self.s.file.window, window);
        self.s.file.others.push(previous);
        self.use_current_window();
        self.sync_layout(true);
        self.send(ScriptEvent::Viewport(egui::ViewportCommand::Focus));
        Ok(())
    }

    /// Select Window.
    pub(super) fn select(&mut self, target: &WindowTarget) -> HostResult {
        if let Some(index) = self.target(target)? {
            self.make_current(index);
        }
        self.send(ScriptEvent::Viewport(egui::ViewportCommand::Focus));
        Ok(())
    }

    /// Close Window: the target is committed and closed; the window
    /// current before it is current again (or stays current when another
    /// window closed), and a closed card's parent or dialog's opener
    /// before any other. A window's card closes first (Claris: "If a
    /// script attempts to close a card's parent window, the script first
    /// attempts to close the card"); the step fails if it cannot. The
    /// last window closes the file.
    pub(super) fn close(&mut self, target: &WindowTarget) -> HostResult {
        let closing = match self.target(target)? {
            None => self.s.file.window.id(),
            Some(index) => self.s.file.others[index].id(),
        };
        let current = self.s.file.window.id();
        // The innermost card first (a card may have a card of its own).
        let mut cards = Vec::new();
        let mut parent = closing;
        while let Some(card) = self.card_of(parent) {
            cards.push(card);
            parent = card;
        }
        for card in cards.into_iter().rev().chain([closing]) {
            self.close_one(card, current)?;
        }
        Ok(())
    }

    /// Close window `id`: it becomes current and commits. A Card returns
    /// to its surviving parent first; other windows preserve `keep` (the
    /// window current when the step started), then try their opener and
    /// the most recently current window.
    fn close_one(&mut self, id: u32, keep: u32) -> HostResult {
        if self.s.file.window.id() != id {
            let Some(index) = self.s.file.others.iter().position(|w| w.id() == id) else {
                return Err(ErrorCode::WINDOW_MISSING.into());
            };
            self.make_current(index);
        }
        self.commit_window()?;
        if self.s.file.others.is_empty() {
            // The file's last window closes the file.
            self.s.halt = true;
            self.send(ScriptEvent::CloseFile);
            return Ok(());
        }
        let parent = self.s.file.window.state.style.parent();
        let keep = Some(keep).filter(|k| *k != id);
        let preferred = if self.s.file.window.state.style.card().is_some() {
            [parent, keep]
        } else {
            [keep, parent]
        };
        let others = &self.s.file.others;
        let index = preferred
            .into_iter()
            .flatten()
            .find_map(|want| others.iter().position(|w| w.id() == want))
            .unwrap_or(others.len() - 1);
        let mut next = self.s.file.others.remove(index);
        self.send(ScriptEvent::CloseWindow(id));
        self.p().drop_window(id);
        next.reenter();
        self.s.file.window = next;
        self.use_current_window();
        *self.s.file.cache.borrow_mut() = Default::default();
        self.sync_layout(true);
        Ok(())
    }

    /// Run `f` with the target window current, then make the current one
    /// current again (Set Window Title, Move/Resize Window of a named
    /// window do not bring it to the front).
    pub(super) fn on_window(
        &mut self,
        target: &WindowTarget,
        f: impl FnOnce(&mut Self) -> HostResult,
    ) -> HostResult {
        let Some(index) = self.target(target)? else {
            return f(self);
        };
        std::mem::swap(&mut self.s.file.window, &mut self.s.file.others[index]);
        let result = f(self);
        self.flush_background();
        std::mem::swap(&mut self.s.file.window, &mut self.s.file.others[index]);
        result
    }
}
