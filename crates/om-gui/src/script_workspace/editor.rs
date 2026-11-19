//! The step editor (captures 35, 35b): one open script's steps as numbered
//! lines of Script Workspace text, with selection, editing, undo and the
//! options of each step. Every change goes through `om-scripts`' edit API:
//! an edit that would unbalance If, Loop or Open Transaction blocks is
//! refused and leaves the script as it was.

use std::collections::{BTreeMap, BTreeSet};

use egui::{
    pos2, text::LayoutJob, vec2, Align2, Color32, FontId, Key, Rect, Role, Sense, Stroke,
    TextFormat, Ui, WidgetInfo,
};
use om_data::scripts::display::{script_lines, LineKind, StepLine, StepNames, INDENT};
use om_data::scripts::{
    self, step_compat, Client, ParamKind, ParamValue, Script, ScriptError, ScriptStep,
};

use super::{glyphs, parameter_calculation, CalculationTarget};
use crate::theme;

/// Height of one step line, in points (captures 35, 35b).
pub(super) const LINE_HEIGHT: f32 = 21.0;
/// Width of the line number gutter.
pub(super) const GUTTER: f32 = 36.0;
/// The step text's left inset inside the editor.
const TEXT_INSET: f32 = 15.0;
const MONOSPACE: f32 = 12.5;
/// The gutter's gray (35: the band left of the step text).
const GUTTER_BG: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF2);
/// A selected line while the editor has the focus, and otherwise (35b).
const SELECTED_FOCUSED: Color32 = Color32::from_rgb(0xD6, 0xE4, 0xFB);
const SELECTED: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xE6);
/// A pasted line that could not be read, and its gutter mark.
const MARKED: Color32 = Color32::from_rgb(0xFD, 0xEC, 0xEA);
const MARK: Color32 = Color32::from_rgb(0xD9, 0x3B, 0x30);
/// The gear's blue (35b).
const GEAR: Color32 = Color32::from_rgb(0x2F, 0x7C, 0xF6);

/// Steps that open a block and the step FileMaker adds to close it.
const BLOCKS: [(u16, u16); 3] = [
    (scripts_flow::IF, scripts_flow::END_IF),
    (scripts_flow::LOOP, scripts_flow::END_LOOP),
    (
        scripts_flow::OPEN_TRANSACTION,
        scripts_flow::COMMIT_TRANSACTION,
    ),
];

/// Block step ids (om-scripts' flow constants).
pub(super) mod scripts_flow {
    pub const IF: u16 = 68;
    pub const ELSE: u16 = 69;
    pub const END_IF: u16 = 70;
    pub const LOOP: u16 = 71;
    pub const END_LOOP: u16 = 73;
    pub const ELSE_IF: u16 = 125;
    pub const OPEN_TRANSACTION: u16 = 205;
    pub const COMMIT_TRANSACTION: u16 = 206;
    pub const COMMENT: u16 = 89;
}

/// The steps, selection and unread pasted lines before an edit.
#[derive(Clone)]
struct Snapshot {
    steps: Vec<ScriptStep>,
    selection: BTreeSet<usize>,
    marked: BTreeMap<[u8; 6], String>,
}

/// A step being typed on a new line, with the matching step names.
pub(super) struct Typing {
    pub text: String,
    /// Insert before this step index.
    pub at: usize,
    pub highlighted: usize,
    focus: bool,
}

/// An option edited in place: a comment's text or a calculation's source.
pub(super) enum Inline {
    Comment {
        index: usize,
        text: String,
    },
    Calculation {
        index: usize,
        target: CalculationTarget,
        text: String,
        original: String,
    },
}

/// What the editor asks of the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EditorRequest {
    /// Open the step's options dialog.
    Options(usize),
    /// Specify the calculation of a step's parameter.
    Calculation {
        index: usize,
        target: CalculationTarget,
    },
    /// Compile `text` for a step's calculation parameter (inline edit).
    CompileInline {
        index: usize,
        target: CalculationTarget,
        text: String,
        original: String,
    },
    /// The editor took the keyboard focus.
    Focus,
}

/// One open script.
pub(super) struct Editor {
    /// The draft being edited.
    pub script: Script,
    /// The script as last loaded or saved.
    pub saved: Script,
    pub selection: BTreeSet<usize>,
    anchor: Option<usize>,
    /// A preview tab (single click in the scripts pane); the first edit
    /// keeps it open.
    pub preview: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Why the last edit was refused.
    pub refusal: Option<String>,
    /// What the last paste could not read, shown under the steps.
    pub notice: Option<String>,
    /// Pasted lines that could not be read, kept as disabled comments: their
    /// steps' data keys and why.
    pub marked: BTreeMap<[u8; 6], String>,
    pub typing: Option<Typing>,
    pub inline: Option<Inline>,
    /// Lines as shown, recomputed when the steps or names change.
    lines: Vec<StepLine>,
    lines_for: Option<(Vec<ScriptStep>, usize)>,
    /// A drag of the selection: the line it would be dropped before.
    drop_at: Option<usize>,
    scroll_to: Option<usize>,
}

/// The steps an edit of `selection` takes along: the other steps of every
/// block step's block (If with its Else Ifs, Else and End If; Loop and End
/// Loop; Open and Commit Transaction).
pub(super) fn with_partners(steps: &[ScriptStep], selection: &BTreeSet<usize>) -> BTreeSet<usize> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        if !step.is_enabled() {
            continue;
        }
        match step.id() {
            scripts_flow::IF | scripts_flow::LOOP | scripts_flow::OPEN_TRANSACTION => {
                open.push(groups.len());
                groups.push(vec![i]);
            }
            scripts_flow::ELSE | scripts_flow::ELSE_IF => {
                if let Some(g) = open.last() {
                    groups[*g].push(i);
                }
            }
            scripts_flow::END_IF | scripts_flow::END_LOOP | scripts_flow::COMMIT_TRANSACTION => {
                if let Some(g) = open.pop() {
                    groups[g].push(i);
                }
            }
            _ => {}
        }
    }
    let mut out = selection.clone();
    for group in groups {
        if group.iter().any(|i| selection.contains(i)) {
            out.extend(group);
        }
    }
    out
}

/// The steps added for step `id`: a block step brings the step that closes
/// its block, as FileMaker inserts If with End If.
pub(super) fn new_steps(id: u16) -> Vec<ScriptStep> {
    let mut steps = vec![new_step(id)];
    if let Some((_, end)) = BLOCKS.iter().find(|(open, _)| *open == id) {
        steps.push(new_step(*end));
    }
    steps
}

/// A new step `id` with its defaults: Go to Related Record shows the
/// related records on `<Current Layout>` (step value 1; FileMaker's stored
/// steps hold 1 or 5, script-steps.md 11.8), not a layout it has none of.
pub(super) fn new_step(id: u16) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    if id == 74 {
        // The schema stores this step value with its Options.
        let _ = step.set_parameter("Options", ParamValue::Choice(1));
    }
    step
}

/// A fresh random UUID (version 4) for a copied step's `KUniqueID`.
pub(super) fn fresh_uuid() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 16];
    for half in out.chunks_mut(8) {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        half.copy_from_slice(&hasher.finish().to_be_bytes());
    }
    out[6] = (out[6] & 0x0f) | 0x40;
    out[8] = (out[8] & 0x3f) | 0x80;
    out
}

/// A copy of `step` to insert elsewhere: a fresh `KUniqueID`, as FileMaker
/// gives a duplicated step.
pub(super) fn copied(step: &ScriptStep) -> ScriptStep {
    match step.unique_id() {
        Some(_) => with_new_uuid(step.clone()),
        None => step.clone(),
    }
}

/// `step` with a fresh `KUniqueID` of its own, as FileMaker gives a pasted
/// step: whether it brought none (text and XML bring none) or another's.
pub(super) fn with_new_uuid(mut step: ScriptStep) -> ScriptStep {
    step.set_data_leaf(
        scripts::StepData::UNIQUE_ID,
        Some(om_data::runtime::OwnedNode::leaf([0xd8], fresh_uuid())),
    );
    step
}

/// Whether the step has options to show (a schema with parameters).
pub(super) fn has_options(step: &ScriptStep) -> bool {
    step.schema().is_some_and(|s| {
        s.params
            .iter()
            .any(|p| p.name != "Collapsed" && p.kind != ParamKind::Index)
    })
}

/// The step ids the steps pane and the new-step list offer: named, not
/// obsolete, and not among the 26.1.1 steps OpenMaker cannot edit or run
/// yet.
pub(super) fn offered_steps() -> impl Iterator<Item = u16> {
    scripts::step_ids().filter(|id| {
        scripts::step_name(*id).is_some()
            && !scripts::OBSOLETE_STEP_IDS.contains(id)
            && !scripts::PENDING_STEP_IDS.contains(id)
    })
}

/// Step ids whose names match `text`: names that start with it first,
/// then names that contain it, each alphabetically.
pub(super) fn matching_steps(text: &str) -> Vec<u16> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return Vec::new();
    }
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for id in offered_steps() {
        let name = scripts::step_name(id).unwrap_or_default().to_lowercase();
        if name.starts_with(&text) {
            starts.push((name, id));
        } else if name.contains(&text) {
            contains.push((name, id));
        }
    }
    starts.sort();
    contains.sort();
    starts
        .into_iter()
        .chain(contains)
        .map(|(_, id)| id)
        .collect()
}

impl Editor {
    /// An open script with nothing selected, as FileMaker opens one (35).
    pub fn new(script: Script, preview: bool) -> Self {
        Self {
            saved: script.clone(),
            script,
            anchor: None,
            selection: BTreeSet::new(),
            preview,
            undo: Vec::new(),
            redo: Vec::new(),
            refusal: None,
            notice: None,
            marked: BTreeMap::new(),
            typing: None,
            inline: None,
            lines: Vec::new(),
            lines_for: None,
            drop_at: None,
            scroll_to: None,
        }
    }

    pub fn id(&self) -> u64 {
        self.script.id
    }

    /// Unsaved changes to the steps.
    pub fn dirty(&self) -> bool {
        self.script.steps != self.saved.steps
    }

    /// The script was saved as `script`.
    pub fn saved(&mut self, script: Script) {
        self.script.steps = script.steps.clone();
        self.script.metadata = script.metadata.clone();
        self.script.unreferenced_data = script.unreferenced_data.clone();
        self.saved = script;
        let len = self.script.steps.len();
        self.selection.retain(|i| *i < len);
        self.forget_marks();
    }

    /// Discard the draft.
    pub fn revert(&mut self) {
        let saved = self.saved.clone();
        self.saved(saved);
        self.undo.clear();
        self.redo.clear();
        self.notice = None;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            steps: self.script.steps.clone(),
            selection: self.selection.clone(),
            marked: self.marked.clone(),
        }
    }

    /// Drop the marks of steps no longer in the script: a later step may
    /// get the same data key.
    fn forget_marks(&mut self) {
        let keys: BTreeSet<[u8; 6]> = self.script.steps.iter().map(|s| s.data_key.0).collect();
        self.marked.retain(|key, _| keys.contains(key));
    }

    /// Apply `f` to a copy of the script; keep the result, and make the
    /// change undoable, only when it nests. `f` returns the new selection.
    pub fn edit(
        &mut self,
        f: impl FnOnce(&mut Script) -> Result<BTreeSet<usize>, ScriptError>,
    ) -> bool {
        let mut draft = self.script.clone();
        match f(&mut draft).and_then(|selection| {
            draft.update_partner_indexes()?;
            Ok(selection)
        }) {
            Ok(selection) => {
                self.undo.push(self.snapshot());
                self.redo.clear();
                self.script = draft;
                self.anchor = selection.first().copied();
                self.scroll_to = selection.last().copied();
                self.selection = selection;
                self.preview = false;
                self.refusal = None;
                self.notice = None;
                self.forget_marks();
                true
            }
            Err(error) => {
                self.refusal = Some(error.0);
                false
            }
        }
    }

    /// Steps in a new order (indexes into the current steps): moves and
    /// deletions keep every step's data key.
    fn reorder(&mut self, order: Vec<usize>, selection: BTreeSet<usize>) -> bool {
        self.edit(|script| {
            let steps: Vec<ScriptStep> = order.iter().map(|i| script.steps[*i].clone()).collect();
            script.steps = steps;
            Ok(selection)
        })
    }

    /// Where new steps go: after the last selected step, else at the end.
    fn insertion_point(&self) -> usize {
        self.selection
            .last()
            .map_or(self.script.steps.len(), |i| i + 1)
    }

    /// Insert steps after the selection (FileMaker's double-click or Return
    /// in the steps pane) and select the first.
    pub fn insert(&mut self, steps: Vec<ScriptStep>) -> bool {
        let at = self.insertion_point();
        self.insert_at(at, steps)
    }

    pub fn insert_at(&mut self, at: usize, steps: Vec<ScriptStep>) -> bool {
        self.edit(|script| {
            script.insert_steps(at, steps)?;
            Ok(BTreeSet::from([at]))
        })
    }

    /// A new step of `id` after the selection, closing its block.
    pub fn insert_new(&mut self, id: u16) -> bool {
        self.insert(new_steps(id))
    }

    /// Delete the selection and the other steps of its blocks.
    pub fn delete(&mut self) -> bool {
        if self.selection.is_empty() {
            return false;
        }
        let gone = with_partners(&self.script.steps, &self.selection);
        let first = *gone.first().expect("selection");
        let order: Vec<usize> = (0..self.script.steps.len())
            .filter(|i| !gone.contains(i))
            .collect();
        let next = first.min(order.len().saturating_sub(1));
        let selection = if order.is_empty() {
            BTreeSet::new()
        } else {
            BTreeSet::from([next])
        };
        self.reorder(order, selection)
    }

    /// Copies of the selection after its last step.
    pub fn duplicate(&mut self) -> bool {
        let steps: Vec<ScriptStep> = self
            .selection
            .iter()
            .map(|i| copied(&self.script.steps[*i]))
            .collect();
        if steps.is_empty() {
            return false;
        }
        let at = self.insertion_point();
        let count = steps.len();
        self.edit(|script| {
            script.insert_steps(at, steps)?;
            Ok((at..at + count).collect())
        })
    }

    /// Insert copies of `steps` after the selection, each with its own
    /// UUID.
    pub fn paste(&mut self, steps: &[ScriptStep]) -> bool {
        if steps.is_empty() {
            return false;
        }
        let at = self.insertion_point();
        let count = steps.len();
        let copies: Vec<ScriptStep> = steps.iter().cloned().map(with_new_uuid).collect();
        self.edit(|script| {
            script.insert_steps(at, copies)?;
            Ok((at..at + count).collect())
        })
    }

    /// Insert pasted text's steps after the selection as one edit, each a
    /// new step with its own UUID. Lines that could not be read go in as
    /// disabled comments holding the line, marked in the editor and counted
    /// in the notice.
    pub fn paste_text(&mut self, pasted: Vec<scripts::display::Pasted>) {
        use scripts::display::{failed_comment, Pasted};
        let at = self.insertion_point();
        let mut failed = Vec::new();
        let steps: Vec<ScriptStep> = pasted
            .into_iter()
            .enumerate()
            .map(|(i, pasted)| match pasted {
                Pasted::Step(step) => with_new_uuid(step),
                Pasted::Failed { text, reason } => {
                    failed.push((i, reason));
                    with_new_uuid(failed_comment(&text))
                }
            })
            .collect();
        let count = steps.len();
        if count == 0
            || !self.edit(|script| {
                script.insert_steps(at, steps)?;
                Ok((at..at + count).collect())
            })
        {
            return;
        }
        for (i, reason) in &failed {
            self.marked
                .insert(self.script.steps[at + i].data_key.0, reason.clone());
        }
        self.notice = match failed.len() {
            0 => None,
            1 => {
                Some("1 pasted line could not be read and was added as a disabled comment.".into())
            }
            n => Some(format!(
                "{n} pasted lines could not be read and were added as disabled comments."
            )),
        };
    }

    /// Disable the selection (with its blocks' other steps), or enable it
    /// when every selected step is disabled.
    pub fn toggle_enabled(&mut self) -> bool {
        if self.selection.is_empty() {
            return false;
        }
        let enable = self
            .selection
            .iter()
            .all(|i| !self.script.steps[*i].is_enabled());
        let affected = if enable {
            self.selection.clone()
        } else {
            with_partners(&self.script.steps, &self.selection)
        };
        let selection = self.selection.clone();
        self.edit(|script| {
            for i in &affected {
                script.steps[*i].set_enabled(enable);
            }
            // `edit` recomputes the cached indexes; a block left half
            // disabled is refused there.
            Ok(selection)
        })
    }

    /// Move the selection one line up (`-1`) or down (`1`).
    pub fn move_by(&mut self, delta: isize) -> bool {
        let n = self.script.steps.len();
        if self.selection.is_empty() {
            return false;
        }
        let mut order: Vec<usize> = (0..n).collect();
        let selected: Vec<usize> = self.selection.iter().copied().collect();
        if delta < 0 {
            if selected[0] == 0 {
                return false;
            }
            for &i in &selected {
                order.swap(i - 1, i);
            }
        } else {
            if *selected.last().expect("selection") + 1 >= n {
                return false;
            }
            for &i in selected.iter().rev() {
                order.swap(i, i + 1);
            }
        }
        let selection = selected
            .iter()
            .map(|i| i.saturating_add_signed(delta))
            .collect();
        self.reorder(order, selection)
    }

    /// Move the selection so that it lands before step `before` (a drag).
    pub fn move_to(&mut self, before: usize) -> bool {
        let n = self.script.steps.len();
        let selected: Vec<usize> = self.selection.iter().copied().collect();
        if selected.is_empty() || before > n {
            return false;
        }
        let rest: Vec<usize> = (0..n).filter(|i| !self.selection.contains(i)).collect();
        let at = rest.iter().take_while(|i| **i < before).count();
        let mut order = rest[..at].to_vec();
        order.extend(&selected);
        order.extend(&rest[at..]);
        if order.iter().copied().eq(0..n) {
            return false;
        }
        let selection = (at..at + selected.len()).collect();
        self.reorder(order, selection)
    }

    /// Replace step `index` (an options dialog's OK).
    pub fn set_step(&mut self, index: usize, step: ScriptStep) -> bool {
        self.edit(|script| {
            script.edit_step(index, |s| {
                *s = step;
                Ok(())
            })?;
            Ok(BTreeSet::from([index]))
        })
    }

    /// Insert an empty comment after the selection and edit its text.
    pub fn add_comment(&mut self) -> bool {
        let at = self.insertion_point();
        if self.insert_at(at, vec![ScriptStep::new(scripts_flow::COMMENT)]) {
            self.inline = Some(Inline::Comment {
                index: at,
                text: String::new(),
            });
            true
        } else {
            false
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.script.steps = snapshot.steps;
        self.selection = snapshot.selection;
        self.marked = snapshot.marked;
        self.refusal = None;
        self.notice = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.script.steps = snapshot.steps;
        self.selection = snapshot.selection;
        self.marked = snapshot.marked;
        self.refusal = None;
        self.notice = None;
        true
    }

    /// Select step `index`: alone, extending from the anchor (shift), or
    /// toggled (⌘).
    pub fn select(&mut self, index: usize, extend: bool, toggle: bool) {
        if extend {
            let anchor = self.anchor.unwrap_or(index);
            self.selection = (anchor.min(index)..=anchor.max(index)).collect();
        } else if toggle {
            if !self.selection.remove(&index) {
                self.selection.insert(index);
            }
            self.anchor = Some(index);
        } else {
            self.selection = BTreeSet::from([index]);
            self.anchor = Some(index);
        }
    }

    /// Move the selection to the previous or next line.
    fn step_selection(&mut self, delta: isize, extend: bool) {
        let n = self.script.steps.len();
        if n == 0 {
            return;
        }
        let from = if delta < 0 {
            self.selection.first().copied()
        } else {
            self.selection.last().copied()
        };
        let next = from.map_or(0, |i| i.saturating_add_signed(delta).min(n - 1));
        if extend {
            self.select(next, true, false);
        } else {
            self.select(next, false, false);
        }
        self.scroll_to = Some(next);
    }

    /// The lines as shown, for `names`.
    pub fn lines(&mut self, names: &dyn StepNames, names_revision: usize) -> &[StepLine] {
        let stale = self
            .lines_for
            .as_ref()
            .is_none_or(|(steps, rev)| *rev != names_revision || *steps != self.script.steps);
        if stale {
            self.lines = script_lines(&self.script.steps, names);
            self.lines_for = Some((self.script.steps.clone(), names_revision));
        }
        &self.lines
    }

    /// The editor's keys while it has the focus and no text field does.
    pub fn keys(&mut self, ui: &Ui) -> Vec<EditorRequest> {
        let mut out = Vec::new();
        let (command, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let pressed = |key: Key| ui.input(|i| i.key_pressed(key));
        if command && pressed(Key::ArrowUp) {
            self.move_by(-1);
        } else if command && pressed(Key::ArrowDown) {
            self.move_by(1);
        } else if pressed(Key::ArrowUp) {
            self.step_selection(-1, shift);
        } else if pressed(Key::ArrowDown) {
            self.step_selection(1, shift);
        } else if pressed(Key::Delete) || pressed(Key::Backspace) {
            self.delete();
        } else if command && pressed(Key::D) {
            self.duplicate();
        } else if command && pressed(Key::Slash) {
            self.toggle_enabled();
        } else if command && shift && pressed(Key::Z) {
            self.redo();
        } else if command && pressed(Key::Z) {
            self.undo();
        } else if pressed(Key::Plus) && self.selection.len() == 1 {
            // FileMaker's "+" shows the selected step's options.
            out.push(EditorRequest::Options(
                *self.selection.first().expect("one"),
            ));
        } else if pressed(Key::Enter) {
            let at = self.insertion_point();
            self.typing = Some(Typing {
                text: String::new(),
                at,
                highlighted: 0,
                focus: true,
            });
        } else if !command {
            // Typing a step name starts a new line with it.
            let typed: String = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) => Some(t.clone()),
                        _ => None,
                    })
                    .collect()
            });
            if !typed.is_empty() && typed != "+" {
                let at = self.insertion_point();
                self.typing = Some(Typing {
                    text: typed,
                    at,
                    highlighted: 0,
                    focus: true,
                });
            }
        }
        out
    }

    /// Draw the steps; returns what the workspace should open.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        names: &dyn StepNames,
        names_revision: usize,
        focused: bool,
        compatibility: Option<Client>,
    ) -> Vec<EditorRequest> {
        let mut requests = Vec::new();
        let font = FontId::monospace(MONOSPACE);
        let lines = self.lines(names, names_revision).to_vec();
        let char_width = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
        let full = ui.available_rect_before_wrap();
        // The gutter's band runs the editor's full height.
        ui.painter().rect_filled(
            Rect::from_min_size(full.min, vec2(GUTTER, full.height())),
            0.0,
            GUTTER_BG,
        );
        let typing_at = self.typing.as_ref().map(|t| t.at);
        let rows = lines.len() + usize::from(typing_at.is_some());
        let content_width = lines
            .iter()
            .map(|l| (l.depth * INDENT.len() + l.text.chars().count()) as f32 * char_width)
            .fold(0.0, f32::max)
            + GUTTER
            + TEXT_INSET
            + 40.0;
        let scroll_to = self.scroll_to.take();
        egui::ScrollArea::both()
            .id_salt("script_steps")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (area, _) = ui.allocate_exact_size(
                    vec2(
                        content_width.max(ui.available_width()),
                        (rows as f32 * LINE_HEIGHT).max(ui.available_height()),
                    ),
                    Sense::hover(),
                );
                let painter = ui.painter_at(area);
                painter.rect_filled(
                    Rect::from_min_size(area.min, vec2(GUTTER, area.height())),
                    0.0,
                    GUTTER_BG,
                );
                // A click below the last line clears the selection.
                let below = Rect::from_min_max(
                    pos2(area.left(), area.top() + rows as f32 * LINE_HEIGHT),
                    area.max,
                );
                if ui
                    .interact(below, ui.scope_id().with("below"), Sense::click())
                    .clicked()
                {
                    self.selection.clear();
                    requests.push(EditorRequest::Focus);
                }
                let mut row = 0usize;
                for (index, line) in lines.iter().enumerate() {
                    if typing_at == Some(index) {
                        let rect = row_rect(area, row);
                        self.typing_row(ui, rect, char_width, &mut requests);
                        row += 1;
                    }
                    let rect = row_rect(area, row);
                    if scroll_to == Some(index) {
                        ui.scroll_to_rect(rect, None);
                    }
                    self.step_row(
                        ui,
                        rect,
                        index,
                        line,
                        names,
                        &font,
                        char_width,
                        focused,
                        compatibility,
                        &mut requests,
                    );
                    row += 1;
                }
                if typing_at == Some(lines.len()) {
                    let rect = row_rect(area, row);
                    self.typing_row(ui, rect, char_width, &mut requests);
                }
                if let Some(before) = self.drop_at {
                    let y = area.top() + before as f32 * LINE_HEIGHT;
                    painter.hline(
                        area.left() + GUTTER..=area.right(),
                        y,
                        Stroke::new(2.0, theme::HIGHLIGHT),
                    );
                }
            });
        requests
    }

    #[allow(clippy::too_many_arguments)]
    fn step_row(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        index: usize,
        line: &StepLine,
        names: &dyn StepNames,
        font: &FontId,
        char_width: f32,
        focused: bool,
        compatibility: Option<Client>,
        requests: &mut Vec<EditorRequest>,
    ) {
        let selected = self.selection.contains(&index);
        let id = ui.scope_id().with(("step", index));
        let response = ui.interact(rect, id, Sense::click_and_drag());
        let painter = ui.painter().clone();
        // A pasted line that could not be read: tinted, with a mark in the
        // gutter and the reason on hover.
        let marked = self
            .marked
            .get(&self.script.steps[index].data_key.0)
            .cloned();
        if let Some(reason) = &marked {
            painter.rect_filled(
                Rect::from_min_max(pos2(rect.left() + GUTTER, rect.top()), rect.max),
                0.0,
                MARKED,
            );
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(rect.left() + GUTTER - 4.0, rect.top()),
                    vec2(3.0, rect.height()),
                ),
                0.0,
                MARK,
            );
            response.clone().on_hover_text(reason);
        }
        if selected {
            painter.rect_filled(
                Rect::from_min_max(pos2(rect.left() + GUTTER, rect.top()), rect.max),
                0.0,
                if focused { SELECTED_FOCUSED } else { SELECTED },
            );
        }
        painter.text(
            pos2(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            (index + 1).to_string(),
            FontId::proportional(theme::BODY - 1.0),
            theme::TEXT,
        );
        let text_x = rect.left() + GUTTER + TEXT_INSET + line.depth as f32 * 4.0 * char_width;
        let step = self.script.steps[index].clone();
        let step = &step;
        let supported = compatibility
            .is_none_or(|client| step_compat(step.id()).is_none_or(|c| c.supports(client)));
        let color = if !line.enabled || !supported {
            theme::WEAK_TEXT
        } else {
            theme::TEXT
        };
        let inline_here = match &self.inline {
            Some(Inline::Comment { index: i, .. }) | Some(Inline::Calculation { index: i, .. }) => {
                *i == index
            }
            None => false,
        };
        let mut job = LayoutJob::default();
        job.append(&line.text, 0.0, TextFormat::simple(font.clone(), color));
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        let text_pos = pos2(text_x, rect.center().y - galley.size().y / 2.0);
        if !inline_here {
            painter.galley(text_pos, galley.clone(), color);
            if line.kind == LineKind::Comment && !line.text.is_empty() {
                // FileMaker shows comments in bold; the monospace face has
                // no bold weight, so the text is drawn twice, offset.
                painter.galley(text_pos + vec2(0.6, 0.0), galley.clone(), color);
            }
        }
        // The option under the pointer, as FileMaker highlights it.
        let span_rect = |range: &std::ops::Range<usize>| {
            let start = line.text[..range.start].chars().count() as f32;
            let len = line.text[range.clone()].chars().count() as f32;
            Rect::from_min_size(
                pos2(text_pos.x + start * char_width - 2.0, rect.top() + 2.0),
                vec2(len * char_width + 4.0, rect.height() - 4.0),
            )
        };
        let hovered_span = response.hover_pos().and_then(|pos| {
            line.options
                .iter()
                .find(|o| span_rect(&o.range).contains(pos))
                .cloned()
        });
        if let (Some(span), false) = (&hovered_span, inline_here) {
            painter.rect_stroke(
                span_rect(&span.range),
                3.0,
                Stroke::new(1.0, theme::HAIRLINE),
                egui::StrokeKind::Inside,
            );
        }
        // The gear opens the options of a single selected step (35b).
        let mut gear_clicked = false;
        if selected && self.selection.len() == 1 && has_options(step) && !inline_here {
            let center = pos2(text_pos.x + galley.size().x + 13.0, rect.center().y);
            let gear_rect = Rect::from_center_size(center, vec2(18.0, 18.0));
            glyphs::gear(&painter, center, 8.0, GEAR);
            let gear = ui.interact(gear_rect, id.with("gear"), Sense::click());
            gear.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Step Options"));
            if gear.clicked() {
                gear_clicked = true;
                requests.push(EditorRequest::Options(index));
            }
        }
        if inline_here {
            self.inline_row(ui, rect, text_x, index, requests);
        }
        // "Line 2: Select All": the line number keeps a step apart from
        // the steps pane's entry of the same name.
        let label = match marked {
            Some(_) => format!("Line {}: {} (could not be read)", index + 1, line.text),
            None => format!("Line {}: {}", index + 1, line.text),
        };
        response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, &label));
        // Selection, the options of a clicked option, the context menu.
        if response.clicked() && !gear_clicked {
            let (shift, command) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
            // A click on a calculation between the selected step's
            // brackets edits it in place (Claris: "Only some calculations
            // are editable inline": one calculation option).
            let only = selected && self.selection.len() == 1 && !shift && !command;
            let calc = hovered_span
                .as_ref()
                .filter(|_| only)
                .and_then(|span| CalculationTarget::for_span(step, span));
            match calc {
                Some(target) => {
                    let text = parameter_calculation(step, target)
                        .map(|calc| om_data::scripts::display::calculation_text(&calc, names))
                        .unwrap_or_default();
                    self.inline = Some(Inline::Calculation {
                        index,
                        target,
                        original: text.clone(),
                        text,
                    });
                }
                None => self.select(index, shift, command),
            }
            requests.push(EditorRequest::Focus);
        }
        if response.double_clicked() {
            match (&hovered_span, line.kind) {
                (_, LineKind::Comment) => {
                    self.inline = Some(Inline::Comment {
                        index,
                        text: step.text(0).unwrap_or_default(),
                    });
                }
                (Some(span), _) => {
                    self.inline = None;
                    let calc = CalculationTarget::for_span(step, span);
                    match calc {
                        Some(target) => requests.push(EditorRequest::Calculation { index, target }),
                        None => requests.push(EditorRequest::Options(index)),
                    }
                }
                (None, _) if has_options(step) => requests.push(EditorRequest::Options(index)),
                (None, _) => {}
            }
        }
        // Dragging the selection to another place.
        if response.drag_started() && !selected {
            self.select(index, false, false);
        }
        if response.dragged() {
            if let Some(pos) = ui.ctx().pointer_interact_pos() {
                let top = rect.top() - index as f32 * LINE_HEIGHT;
                let before = ((pos.y - top) / LINE_HEIGHT).round().max(0.0) as usize;
                self.drop_at = Some(before.min(self.script.steps.len()));
            }
        }
        if response.drag_stopped() {
            if let Some(before) = self.drop_at.take() {
                self.move_to(before);
            }
        }
        response.context_menu(|ui| {
            if !self.selection.contains(&index) {
                self.select(index, false, false);
            }
            self.context_menu(ui, requests);
        });
    }

    /// The step context menu: clipboard, duplicate, delete, disable,
    /// comment, options.
    fn context_menu(&mut self, ui: &mut Ui, requests: &mut Vec<EditorRequest>) {
        ui.set_min_width(180.0);
        let one = self.selection.len() == 1;
        if ui.button("Cut").clicked() {
            super::clipboard::put(ui.ctx(), self, true);
            self.delete();
            ui.close();
        }
        if ui.button("Copy as Text").clicked() {
            super::clipboard::put(ui.ctx(), self, false);
            ui.close();
        }
        if ui.button("Paste").clicked() {
            // Fetch the current system clipboard through the same Paste
            // event as the keyboard; do not reuse a stale internal copy.
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            self.duplicate();
            ui.close();
        }
        if ui.button("Delete").clicked() {
            self.delete();
            ui.close();
        }
        ui.separator();
        let all_disabled = self
            .selection
            .iter()
            .all(|i| !self.script.steps[*i].is_enabled());
        if ui
            .button(if all_disabled { "Enable" } else { "Disable" })
            .clicked()
        {
            self.toggle_enabled();
            ui.close();
        }
        if ui.button("Add Comment").clicked() {
            self.add_comment();
            ui.close();
        }
        let options = one
            && self
                .selection
                .first()
                .is_some_and(|i| has_options(&self.script.steps[*i]));
        if ui
            .add_enabled(options, egui::Button::new("Step Options..."))
            .clicked()
        {
            if let Some(i) = self.selection.first() {
                requests.push(EditorRequest::Options(*i));
            }
            ui.close();
        }
    }

    /// The new step line: a text field and the step names it matches.
    fn typing_row(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        _char_width: f32,
        requests: &mut Vec<EditorRequest>,
    ) {
        let Some(typing) = &mut self.typing else {
            return;
        };
        let field_rect = Rect::from_min_max(
            pos2(rect.left() + GUTTER + TEXT_INSET - 3.0, rect.top() + 1.0),
            pos2(rect.right() - 8.0, rect.bottom() - 1.0),
        );
        let matches = matching_steps(&typing.text);
        let (up, down, escape) = ui.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::Escape),
            )
        });
        if down && typing.highlighted + 1 < matches.len() {
            typing.highlighted += 1;
        }
        if up && typing.highlighted > 0 {
            typing.highlighted -= 1;
        }
        // The list shows only while the field has the focus, and egui takes
        // the focus from a field on a click anywhere else before the list
        // is drawn, so a click on a step name would close the list without
        // choosing it. Keep the focus while the pointer is on the list, as
        // egui's own completion popup does.
        let list_id = ui.scope_id().with("step_matches");
        let ctx = ui.ctx().clone();
        let on_list = ctx
            .pointer_interact_pos()
            .and_then(|pos| ctx.layer_id_at(pos))
            == Some(egui::LayerId::new(egui::Order::Foreground, list_id));
        let surrender = ctx.options(|o| o.input_options.surrender_focus_on);
        if on_list {
            ctx.options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        let edit = ui.put(
            field_rect,
            egui::TextEdit::singleline(&mut typing.text)
                .font(FontId::monospace(MONOSPACE))
                .hint_text("Type a script step")
                .frame(egui::Frame::new())
                .desired_width(field_rect.width()),
        );
        ctx.options_mut(|o| o.input_options.surrender_focus_on = surrender);
        crate::widgets::accessible_label(ui, edit.id, "New Step");
        if typing.focus {
            edit.request_focus();
            typing.focus = false;
        }
        typing.highlighted = typing.highlighted.min(matches.len().saturating_sub(1));
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let text = typing.text.clone();
        let at = typing.at;
        let highlighted = typing.highlighted;
        // The matching steps, below the line.
        if !matches.is_empty() && edit.has_focus() {
            egui::Area::new(list_id)
                .order(egui::Order::Foreground)
                .fixed_pos(pos2(field_rect.left(), field_rect.bottom() + 2.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(260.0);
                        for (n, id) in matches.iter().take(12).enumerate() {
                            let name = scripts::step_name(*id).unwrap_or_default();
                            let chosen = ui.selectable_label(n == highlighted, name);
                            if chosen.clicked() {
                                self.typing = None;
                                self.insert_at(at, new_steps(*id));
                                requests.push(EditorRequest::Focus);
                                return;
                            }
                        }
                    });
                });
        }
        if escape {
            self.typing = None;
            requests.push(EditorRequest::Focus);
        } else if entered {
            self.typing = None;
            if let Some(comment) = text.trim_start().strip_prefix('#') {
                let mut step = ScriptStep::new(scripts_flow::COMMENT);
                step.set_text(0, Some(comment.trim_start()));
                self.insert_at(at, vec![step]);
            } else if let Some(id) = matches.get(highlighted) {
                self.insert_at(at, new_steps(*id));
            }
            requests.push(EditorRequest::Focus);
        }
    }

    /// A comment's text or a calculation edited in place.
    fn inline_row(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        text_x: f32,
        index: usize,
        requests: &mut Vec<EditorRequest>,
    ) {
        let field_rect = Rect::from_min_max(
            pos2(text_x - 3.0, rect.top() + 1.0),
            pos2(rect.right() - 8.0, rect.bottom() - 1.0),
        );
        let Some(inline) = &mut self.inline else {
            return;
        };
        let text = match inline {
            Inline::Comment { text, .. } | Inline::Calculation { text, .. } => text,
        };
        let edit = ui.put(
            field_rect,
            egui::TextEdit::singleline(text)
                .font(FontId::monospace(MONOSPACE))
                .desired_width(field_rect.width()),
        );
        crate::widgets::accessible_label(ui, edit.id, "Edit Option");
        if !edit.has_focus() && !edit.lost_focus() {
            edit.request_focus();
        }
        let escape = ui.input(|i| i.key_pressed(Key::Escape));
        if escape {
            self.inline = None;
            requests.push(EditorRequest::Focus);
            return;
        }
        if edit.lost_focus() {
            // This Return submits the inline edit, not an alert created by
            // its compilation later in this frame.
            ui.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, Key::Enter);
            });
            match self.inline.take() {
                Some(Inline::Comment { text, .. }) => {
                    self.edit(|script| {
                        script.edit_step(index, |s| {
                            s.set_text(0, Some(&text));
                            Ok(())
                        })?;
                        Ok(BTreeSet::from([index]))
                    });
                }
                Some(Inline::Calculation {
                    target,
                    text,
                    original,
                    ..
                }) if text != original => requests.push(EditorRequest::CompileInline {
                    index,
                    target,
                    text,
                    original,
                }),
                Some(Inline::Calculation { .. }) => {}
                None => {}
            }
            requests.push(EditorRequest::Focus);
        }
    }
}

fn row_rect(area: Rect, row: usize) -> Rect {
    Rect::from_min_size(
        pos2(area.left(), area.top() + row as f32 * LINE_HEIGHT),
        vec2(area.width(), LINE_HEIGHT),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::scripts::display::{step_line, NoStepNames};
    use scripts_flow::*;

    fn editor(ids: &[u16]) -> Editor {
        let mut script = Script {
            id: 1,
            name: "Test".into(),
            ..Script::default()
        };
        script
            .insert_steps(0, ids.iter().map(|id| ScriptStep::new(*id)))
            .unwrap();
        Editor::new(script, false)
    }

    fn ids(e: &Editor) -> Vec<i16> {
        e.script.steps.iter().map(|s| s.opcode).collect()
    }

    #[test]
    fn new_blocks_bring_their_closing_step() {
        let mut e = editor(&[7]);
        assert!(e.insert_new(IF));
        assert_eq!(ids(&e), [7, 68, 70]);
        assert_eq!(e.selection, BTreeSet::from([1]));
        assert!(e.insert_new(LOOP));
        assert_eq!(ids(&e), [7, 68, 71, 73, 70]);
        assert!(!e.insert_new(ELSE_IF));
        // An Else If inside a Loop is refused; nothing changes.
        assert_eq!(ids(&e), [7, 68, 71, 73, 70]);
        assert!(e.refusal.as_deref().unwrap().contains("Loop"));
        assert!(e.dirty());
    }

    #[test]
    fn deleting_a_block_step_takes_its_partners_and_keeps_the_body() {
        let mut e = editor(&[IF, 7, ELSE, 8, END_IF, 9]);
        e.select(2, false, false);
        assert!(e.delete());
        assert_eq!(ids(&e), [7, 8, 9]);
        assert!(e.undo());
        assert_eq!(ids(&e), [68, 7, 69, 8, 70, 9]);
        assert!(e.redo());
        assert_eq!(ids(&e), [7, 8, 9]);
    }

    #[test]
    fn moves_keep_blocks_balanced() {
        let mut e = editor(&[7, IF, 8, END_IF]);
        e.select(0, false, false);
        assert!(e.move_by(1));
        assert_eq!(ids(&e), [68, 7, 8, 70]);
        assert_eq!(e.selection, BTreeSet::from([1]));
        // Moving End If above its If is refused.
        e.select(3, false, false);
        assert!(e.move_to(0) || e.refusal.is_some());
        assert_eq!(ids(&e), [68, 7, 8, 70]);
        // Drag two steps to the end.
        e.select(1, false, false);
        e.select(2, true, false);
        assert!(e.move_to(0));
        assert_eq!(ids(&e), [7, 8, 68, 70]);
        assert_eq!(e.selection, BTreeSet::from([0, 1]));
        assert_eq!(e.script.steps[2].value, 3);
    }

    #[test]
    fn disabling_takes_the_block_along_and_duplicates_get_fresh_keys() {
        let mut e = editor(&[LOOP, 7, END_LOOP]);
        e.select(0, false, false);
        assert!(e.toggle_enabled());
        assert!(e.script.steps.iter().filter(|s| !s.is_enabled()).count() == 2);
        assert!(e.toggle_enabled() || e.refusal.is_some());
        let mut e = editor(&[7, 8]);
        e.select(0, false, false);
        e.select(1, true, false);
        assert!(e.duplicate());
        assert_eq!(ids(&e), [7, 8, 7, 8]);
        let keys: BTreeSet<_> = e.script.steps.iter().map(|s| s.data_key.0).collect();
        assert_eq!(keys.len(), 4);
        assert_eq!(e.selection, BTreeSet::from([2, 3]));
    }

    #[test]
    fn a_click_on_a_typed_steps_match_inserts_it() {
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{cell::RefCell, rc::Rc};

        let editor = Rc::new(RefCell::new(editor(&[7])));
        editor.borrow_mut().typing = Some(Typing {
            text: "Show Cu".into(),
            at: 1,
            highlighted: 0,
            focus: true,
        });
        let shown = Rc::clone(&editor);
        let mut app = Harness::builder()
            .with_size(vec2(800.0, 400.0))
            .build_ui(move |ui| {
                shown.borrow_mut().ui(ui, &NoStepNames, 0, true, None);
            });
        app.run_steps(2);
        // The pointer comes to the list first, then presses and releases.
        app.get_by_label("Show Custom Dialog").hover();
        app.run_steps(2);
        app.get_by_label("Show Custom Dialog").click();
        app.run_steps(2);
        let editor = editor.borrow();
        assert!(editor.typing.is_none());
        assert_eq!(ids(&editor), [7, 87]);
    }

    #[test]
    fn typed_names_match_by_prefix_then_substring() {
        let found = matching_steps("set f");
        assert_eq!(scripts::step_name(found[0]), Some("Set Field"));
        assert!(found.contains(&147));
        assert!(matching_steps("loop").contains(&LOOP));
        assert!(matching_steps("").is_empty());
        assert_eq!(
            step_line(&ScriptStep::new(50), &NoStepNames).text,
            "Select All"
        );
    }
}
