//! Undo and redo of the window's staged layout changes. A checkpoint holds
//! the draft together so deleting an object can also undo the removal of its
//! staged frame, control and words.

use std::collections::VecDeque;
use std::mem::size_of;

use super::{
    CanvasState, ControlChanges, DeletedObjects, MovedObjects, NewButton, NewField, NewPortal,
    NewText, NewWidth, WordChanges,
};
use crate::session::{Geometry, Session};

const MAX_STEPS: usize = 32;
// Bound the amount of draft data copied for each checkpoint. Include room
// for map/set entries, which carry keys as well as their inline values.
const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;
const MAP_ENTRY_OVERHEAD: usize = 32;

/// Edit menu commands for the unsaved layout. Each operates on the window's
/// current draft and returns whether a checkpoint was applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftEdit {
    Undo,
    Redo,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DraftGesture {
    Pointer(egui::Id),
    Text(egui::Id),
}

#[derive(Default)]
pub(super) struct History {
    undo: VecDeque<Draft>,
    redo: Vec<Draft>,
    gesture: Option<(DraftGesture, Draft)>,
}

/// Persistent draft data only. Geometry, input state, selection, and dialogs
/// are deliberately outside a checkpoint. An unfinished dialog owns its
/// placement until OK, so Cancel does not create an undo step.
pub(crate) struct Draft {
    fields: Vec<NewField>,
    button: Option<NewButton>,
    text: Option<NewText>,
    portal: Option<NewPortal>,
    moved: MovedObjects,
    deleted: DeletedObjects,
    controls: ControlChanges,
    words: WordChanges,
    width: Option<NewWidth>,
}

impl CanvasState {
    pub(crate) fn clear_history(&mut self) {
        self.history = History::default();
    }

    /// The state before a canvas gesture or a discrete draft edit.
    pub(crate) fn checkpoint(&self) -> Option<Draft> {
        self.snapshot_fits()?;
        Some(Draft {
            fields: self.new_fields.clone(),
            button: self
                .new_button
                .as_ref()
                .filter(|b| b.step.is_some())
                .cloned(),
            text: self.new_text.clone().map(|mut text| {
                text.focus = false;
                text
            }),
            portal: self
                .new_portal
                .as_ref()
                .filter(|p| p.setup.is_some())
                .cloned(),
            moved: self.moved.clone(),
            deleted: self.deleted.clone(),
            controls: self.controls.clone(),
            words: self.words.clone(),
            width: self.width,
        })
    }

    fn snapshot_fits(&self) -> Option<()> {
        let button = self.new_button.as_ref().filter(|b| b.step.is_some());
        let portal = self.new_portal.as_ref().filter(|p| p.setup.is_some());
        let mut remaining = MAX_SNAPSHOT_BYTES - size_of::<Draft>();
        if let Some(data) = button.and_then(|b| b.step.as_ref()?.data.as_ref()) {
            remaining -= data.memory_within(remaining)?;
        }
        let mut reserve = |bytes: usize| {
            remaining = remaining.checked_sub(bytes)?;
            Some(())
        };
        for (count, size) in [
            (self.new_fields.len(), size_of::<NewField>()),
            (
                self.moved.frames.len(),
                size_of::<(u64, om_data::GeomRect)>() + MAP_ENTRY_OVERHEAD,
            ),
            (
                self.deleted.keys.len(),
                size_of::<u64>() + MAP_ENTRY_OVERHEAD,
            ),
            (
                self.controls.objects.len(),
                size_of::<(u64, (u32, Option<u64>))>() + MAP_ENTRY_OVERHEAD,
            ),
            (
                self.words.objects.len(),
                size_of::<(u64, String)>() + MAP_ENTRY_OVERHEAD,
            ),
        ] {
            reserve(count.saturating_mul(size))?;
        }
        for field in self.new_fields.iter().filter_map(|f| f.field.as_ref()) {
            reserve(field.name.len())?;
        }
        for text in self.words.objects.values() {
            reserve(text.len())?;
        }
        if let Some(text) = &self.new_text {
            reserve(text.text.len())?;
        }
        if let Some(button) = button {
            reserve(button.label.len())?;
            reserve(button.action.len())?;
        }
        if let Some(choice) = portal.and_then(|p| p.setup.as_ref()) {
            reserve(choice.name.len())?;
            reserve(
                choice
                    .fields
                    .len()
                    .saturating_mul(size_of::<(u64, String)>()),
            )?;
            for (_, name) in &choice.fields {
                reserve(name.len())?;
            }
        }
        Some(())
    }

    pub(crate) fn record_frame(&mut self, before: Option<Draft>, ctx: &egui::Context) {
        let editor = ctx
            .memory(|memory| memory.focused())
            .filter(|&id| egui::TextEdit::load_state(ctx, id).is_some());
        let outside_editor = |id: &egui::Id| Some(*id) != editor;
        // An outside drag can leave an Inspector editor focused. Give the
        // drag and its release their own step, and keep selection of text
        // within the focused editor in that editor's typing session.
        if let Some(id) = ctx.dragged_id().filter(outside_editor) {
            self.record_draft(before, Some(DraftGesture::Pointer(id)), true);
        } else if let Some(id) = ctx.drag_stopped_id().filter(outside_editor) {
            self.record_draft(before, Some(DraftGesture::Pointer(id)), false);
        } else {
            self.record_draft(before, editor.map(DraftGesture::Text), editor.is_some());
        }
    }

    /// Keep one checkpoint for a completed pointer gesture or typing
    /// session. A no-op leaves the redo branch intact.
    fn record_draft(
        &mut self,
        before: Option<Draft>,
        gesture: Option<DraftGesture>,
        ongoing: bool,
    ) {
        let before = match self.history.gesture.take() {
            Some((previous, checkpoint)) if gesture.is_some_and(|next| next != previous) => {
                // The live draft at the start of this frame still includes
                // the previous gesture. Keep that step before recording the
                // first mutations of the new pointer gesture or text editor.
                self.push_checkpoint(checkpoint);
                before
            }
            Some((_, checkpoint)) => Some(checkpoint),
            None => before,
        };
        let Some(before) = before else {
            self.clear_history();
            return;
        };
        if before.matches(self) {
            return;
        }
        if self.snapshot_fits().is_none() {
            self.clear_history();
            return;
        }
        if let Some(gesture) = gesture.filter(|_| ongoing) {
            self.history.gesture = Some((gesture, before));
        } else {
            self.push_checkpoint(before);
        }
    }

    /// A discrete command gets its own step even if a text editor kept
    /// focus while the user opened a menu.
    pub(crate) fn record_command(&mut self, before: Option<Draft>) {
        if let Some((_, checkpoint)) = self.history.gesture.take() {
            self.push_checkpoint(checkpoint);
        }
        self.record_draft(before, None, false);
    }

    fn push_checkpoint(&mut self, before: Draft) {
        self.history.redo.clear();
        if self.history.undo.len() == MAX_STEPS {
            self.history.undo.pop_front();
        }
        self.history.undo.push_back(before);
    }

    pub(crate) fn draft_edits(&self, session: &Session) -> (bool, bool) {
        let Geometry::Loaded(geometry) = &session.geometry else {
            return (false, false);
        };
        if !self.matches(session)
            || self.geometry.as_ref() != Some(geometry)
            || self.history.gesture.is_some()
            || self.retyping.is_some()
            || self.part_drag.is_some()
            || self.part_resize.is_some()
        {
            return (false, false);
        }
        (!self.history.undo.is_empty(), !self.history.redo.is_empty())
    }

    pub(crate) fn edit_draft(
        &mut self,
        ctx: &egui::Context,
        session: &Session,
        edit: DraftEdit,
    ) -> bool {
        let (undo, redo) = self.draft_edits(session);
        let available = match edit {
            DraftEdit::Undo => undo,
            DraftEdit::Redo => redo,
        };
        if !available {
            return false;
        }
        let Some(current) = self.checkpoint() else {
            self.clear_history();
            return false;
        };
        let draft = match edit {
            DraftEdit::Undo => self.history.undo.pop_back().inspect(|_| {
                self.history.redo.push(current);
            }),
            DraftEdit::Redo => self.history.redo.pop().inspect(|_| {
                self.history.undo.push_back(current);
            }),
        };
        let Some(draft) = draft else {
            return false;
        };
        self.drop_new_object();
        self.clear_selection();
        self.scroll_to_selected = false;
        if let Some(editor) = self.new_text_editor.take() {
            egui::TextEdit::store_state(ctx, editor, egui::text_edit::TextEditState::default());
        }
        self.new_fields = draft.fields;
        self.new_button = draft.button;
        self.new_text = draft.text;
        self.new_portal = draft.portal;
        self.moved = draft.moved;
        self.deleted = draft.deleted;
        self.controls = draft.controls;
        self.words = draft.words;
        self.width = draft.width;
        true
    }
}

impl Draft {
    fn matches(&self, canvas: &CanvasState) -> bool {
        self.fields == canvas.new_fields
            && self.button.as_ref() == canvas.new_button.as_ref().filter(|b| b.step.is_some())
            && self.portal.as_ref() == canvas.new_portal.as_ref().filter(|p| p.setup.is_some())
            && match (&self.text, &canvas.new_text) {
                (None, None) => true,
                (Some(before), Some(now)) => {
                    (before.left, before.top, before.width, before.height)
                        == (now.left, now.top, now.width, now.height)
                        && before.text == now.text
                        && before.revision == now.revision
                }
                _ => false,
            }
            && self.moved == canvas.moved
            && self.deleted == canvas.deleted
            && self.controls == canvas.controls
            && self.words == canvas.words
            && self.width == canvas.width
    }
}
