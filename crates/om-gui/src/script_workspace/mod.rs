//! The Script Workspace (captures 35, 35b): a window of its own titled
//! `Script Workspace (<file> (<host>))`, with a toolbar, the scripts pane,
//! the step editor with a tab per open script, and the steps pane.
//!
//! Steps are edited through `om-scripts`' edit API and saved with the
//! provider's `save_script`; the scripts pane's changes (new, renamed,
//! moved, duplicated and deleted members, script options) are stored at
//! once with `save_script_catalog`, as FileMaker commits them. A provider
//! that cannot save answers with an error, which the workspace shows as
//! FileMaker shows a failed save: an alert, the script left unsaved.
//!
//! Where the backend opens real windows the workspace is one; otherwise
//! (embedded viewports, the headless tests) it is drawn as a window inside
//! the document window.

pub(crate) mod button_setup;
pub(crate) mod calculation;
mod catalog_pane;
mod editor;
mod export_dialog;
mod file_dialog;
mod find_dialog;
mod glyphs;
mod import_dialog;
mod options;
mod pickers;
pub(crate) mod sort_dialog;
pub(crate) use pickers::Picked;
pub(crate) use sort_dialog::{BrowseSort, SortOrder};
mod steps_pane;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use egui::{
    pos2, vec2, Color32, CornerRadius, FontId, Id, Key, KeyboardShortcut, Modifiers, Rect, Role,
    Sense, Stroke, Ui, WidgetInfo,
};
use om_data::host::CalcSchema;
use om_data::scripts::display::{parse_steps, paste_limits, NoStepNames, Origin, StepNames};
use om_data::scripts::{Client, Location, ParamKind, SlotKind};
use om_data::{DataError, Script, ScriptCatalog, ScriptKind, ScriptStep};

use catalog_pane::{PaneRequest, ScriptsPane};
use editor::{Editor, EditorRequest};
use glyphs::Glyph;
use steps_pane::StepsPane;

use crate::{theme, widgets};

/// The panes' widths in capture 35.
const SCRIPTS_PANE: f32 = 224.0;
const STEPS_PANE: f32 = 288.0;
const TITLE_BAR: f32 = 28.0;
const TOOLBAR: f32 = 30.0;
/// The editor's tab bar.
const TAB_BAR: f32 = 26.0;
const PANE_BG: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF0);
const BAR_BG: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xEC);
const RULE: Color32 = Color32::from_rgb(0xD3, 0xD3, 0xD3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScriptWorkspaceAction {
    Run(u64),
    /// Save one script's steps.
    Save(Script),
    /// Store the catalog's members (the scripts pane's changes).
    SaveCatalog(ScriptCatalog),
    Close,
}

/// Which pane has the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Focus {
    #[default]
    Scripts,
    Editor,
    Steps,
}

/// What follows a Save / Don't Save answer or a finished save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Then {
    /// Close the tab of this script.
    CloseTab(u64),
    /// Close the workspace (after asking for every unsaved script).
    CloseWorkspace,
    /// Run the script.
    Run(u64),
    /// Nothing more.
    Stay,
}

enum Dialog {
    /// "Save changes to script “X”?": Don't Save, Cancel, Save.
    SaveChanges { id: u64, then: Then },
    /// "Permanently delete …?"
    ConfirmDelete(Vec<u64>),
    /// An alert with OK.
    Alert(String),
    Options {
        id: u64,
        index: usize,
        dialog: Box<options::StepOptions>,
    },
    Calculation {
        id: u64,
        index: usize,
        target: CalculationTarget,
        dialog: Box<calculation::SpecifyCalculation>,
    },
}

pub(crate) struct ScriptWorkspace {
    /// The document window's title, `test project (fmhost)`.
    document: String,
    clipboard_scope: u64,
    catalog: Option<ScriptCatalog>,
    names: Option<Arc<CalcSchema>>,
    /// The document's unmodified schema; `names` overlays the current catalog.
    names_source: Option<Arc<CalcSchema>>,
    /// Bumped when `names` changes, so editors re-render their lines.
    names_revision: usize,
    pane: ScriptsPane,
    steps: StepsPane,
    tabs: Vec<Editor>,
    active: Option<usize>,
    show_scripts: bool,
    show_steps: bool,
    focus: Focus,
    compatibility: Option<Client>,
    busy: bool,
    dialog: Option<Dialog>,
    /// What a save in flight continues with.
    after_save: Option<Then>,
    /// The catalog a catalog save in flight sent, and a new script of it
    /// to open once it is stored: the host may store a new member under
    /// another id (the next its index issues).
    sent_catalog: Option<ScriptCatalog>,
    open_when_saved: Option<u64>,
    /// The file whose windows show this workspace (the app's file id).
    owner: u64,
    /// The app's requests for this workspace whose replies are still to
    /// come: a reply to any other request is not this workspace's.
    awaiting: BTreeSet<u64>,
}

/// The search field of a pane (35: rounded, magnifier, "Search").
/// The scripts pane's field is gray, the steps pane's white with a border
/// (35, 35b).
pub(super) fn search_field(ui: &mut Ui, text: &mut String, label: &str, gray: bool) {
    let width = ui.available_width() - 12.0;
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let (fill, stroke) = if gray {
            (Color32::from_gray(0xE4), Stroke::NONE)
        } else {
            (Color32::WHITE, Stroke::new(1.0, theme::SEARCH_BORDER))
        };
        let frame = egui::Frame::new()
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(5))
            .inner_margin(egui::Margin::symmetric(6, 2));
        frame.show(ui, |ui| {
            ui.set_width(width - 12.0);
            ui.horizontal(|ui| {
                let (glyph, _) = ui.allocate_exact_size(vec2(14.0, 16.0), Sense::hover());
                crate::icons::paint(
                    ui.painter(),
                    glyph,
                    crate::icons::Icon::Search,
                    theme::WEAK_TEXT,
                    Color32::WHITE,
                );
                let edit = ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(egui::RichText::new("Search").size(theme::BODY - 1.0))
                        .font(FontId::proportional(theme::BODY - 1.0))
                        .frame(egui::Frame::new())
                        .desired_width(ui.available_width()),
                );
                widgets::accessible_label(ui, edit.id, label);
            });
        });
    });
}

/// The workspace's step clipboard. The system clipboard gets the steps as
/// text (script-steps.md, "Text form of steps"), read back against the
/// pasting file's names. The steps themselves stay in the process, but
/// their ids are file-local: they paste whole only within the workspace
/// that copied them, and only while the system clipboard still holds their
/// text.
mod clipboard {
    use super::editor::{with_partners, Editor};
    use egui::{Context, Id};
    use om_data::host::CalcSchema;
    use om_data::scripts::display::{steps_text, NoStepNames, StepNames};
    use om_data::ScriptStep;
    use std::sync::Arc;

    #[derive(Clone)]
    struct Scope {
        id: u64,
        names: Option<Arc<CalcSchema>>,
    }
    #[derive(Clone)]
    struct Copy {
        scope: u64,
        text: String,
        steps: Vec<ScriptStep>,
    }

    fn id() -> Id {
        Id::unique("om_script_workspace_clipboard")
    }

    pub fn activate(ctx: &Context, scope: u64, names: Option<Arc<CalcSchema>>) {
        ctx.data_mut(|d| d.insert_temp(id().with("scope"), Scope { id: scope, names }));
    }

    /// Copy the editor's selection; a cut takes the steps `delete` removes
    /// with it.
    pub fn put(ctx: &Context, editor: &Editor, cut: bool) {
        let selection = if cut {
            with_partners(&editor.script.steps, &editor.selection)
        } else {
            editor.selection.clone()
        };
        let steps = selection
            .iter()
            .map(|i| editor.script.steps[*i].clone())
            .collect();
        put_scripts(ctx, vec![steps]);
    }

    /// Copy the steps of whole scripts, one after another.
    pub fn put_scripts(ctx: &Context, scripts: Vec<Vec<ScriptStep>>) {
        let Some(scope) = ctx.data(|d| d.get_temp::<Scope>(id().with("scope"))) else {
            return;
        };
        let names: &dyn StepNames = scope
            .names
            .as_deref()
            .map(|n| n as &dyn StepNames)
            .unwrap_or(&NoStepNames);
        let text: String = scripts
            .iter()
            .map(|steps| steps_text(steps, names))
            .collect();
        let steps = scripts.concat();
        if steps.is_empty() {
            return;
        }
        ctx.copy_text(text.clone());
        ctx.data_mut(|d| {
            d.insert_temp(
                id(),
                Copy {
                    scope: scope.id,
                    text,
                    steps,
                },
            )
        });
    }

    /// The copied steps, when this workspace copied them and the system
    /// clipboard's `text` is still theirs.
    pub fn get(ctx: &Context, text: &str) -> Vec<ScriptStep> {
        ctx.data(|d| {
            d.get_temp::<Scope>(id().with("scope"))
                .zip(d.get_temp::<Copy>(id()))
                .filter(|(scope, copy)| scope.id == copy.scope && copy.text == text)
                .map(|(_, copy)| copy.steps)
                .unwrap_or_default()
        })
    }
}

/// A displayed calculation's schema parameter and its actual stored slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CalculationTarget {
    parameter: usize,
    slot: u16,
}

impl CalculationTarget {
    /// Only spans that actually display a calculation can edit one.
    /// Selector labels may share a parameter with its calculation.
    fn for_span(step: &ScriptStep, span: &om_data::scripts::display::OptionSpan) -> Option<Self> {
        Self::for_option(step, span.parameter?, Some(span.calculation_slot?))
    }

    fn for_option(step: &ScriptStep, parameter: usize, slot: Option<u16>) -> Option<Self> {
        let p = step.schema()?.params.get(parameter)?;
        (p.kind == ParamKind::Calculation).then_some(())?;
        let slots: Vec<u16> = p
            .storage
            .iter()
            .filter_map(|location| match location {
                Location::Slot {
                    slot,
                    kind: SlotKind::Calculation,
                } => Some(*slot),
                _ => None,
            })
            .collect();
        let slot = match slot {
            Some(slot) if slots.contains(&slot) => slot,
            None if slots.len() == 1 => slots[0],
            _ => return None,
        };
        Some(Self { parameter, slot })
    }
}

/// Set the displayed calculation, preserving sibling slots and updating
/// the same companion flags as the step options dialog.
fn set_parameter_calculation(
    step: &mut ScriptStep,
    target: CalculationTarget,
    calculation: Option<&om_data::Calculation>,
) -> Result<(), String> {
    CalculationTarget::for_option(step, target.parameter, Some(target.slot))
        .ok_or_else(|| "This option is not a calculation.".to_owned())?;
    let p = &step.schema().expect("validated schema").params[target.parameter];
    let flags = options::companion_flags(p);
    options::set_calculation(step, target.slot, flags, calculation);
    Ok(())
}

/// The calculation stored for this exact displayed option.
fn parameter_calculation(
    step: &ScriptStep,
    target: CalculationTarget,
) -> Option<om_data::Calculation> {
    CalculationTarget::for_option(step, target.parameter, Some(target.slot))?;
    step.calculation(target.slot)
}

impl ScriptWorkspace {
    /// A workspace for the document window titled `document`.
    pub(crate) fn new(document: String) -> Self {
        static NEXT_CLIPBOARD_SCOPE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        Self {
            document,
            clipboard_scope: NEXT_CLIPBOARD_SCOPE
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            catalog: None,
            names: None,
            names_source: None,
            names_revision: 0,
            pane: ScriptsPane::default(),
            steps: StepsPane::default(),
            tabs: Vec::new(),
            active: None,
            show_scripts: true,
            show_steps: true,
            focus: Focus::Scripts,
            compatibility: None,
            busy: true,
            dialog: None,
            after_save: None,
            sent_catalog: None,
            open_when_saved: None,
            owner: 0,
            awaiting: BTreeSet::new(),
        }
    }

    /// The workspace of file `file` (the app's file id).
    pub(crate) fn owned_by(mut self, file: u64) -> Self {
        self.owner = file;
        self
    }

    /// Whether this is file `file`'s workspace: only its windows show it
    /// and send its requests.
    pub(crate) fn owns(&self, file: u64) -> bool {
        self.owner == file
    }

    /// The app sent request `token` for this workspace.
    pub(crate) fn requested(&mut self, token: u64) {
        self.awaiting.insert(token);
    }

    /// Whether the reply to request `token`, from file `file`, is this
    /// workspace's to take (once). A reply to a request of a workspace
    /// closed or replaced since, or of another file, is not, even with the
    /// same script ids.
    /// Whether it waits for an answer: a save under way, or a request
    /// whose reply has not come.
    pub(crate) fn waiting(&self) -> bool {
        self.busy || !self.awaiting.is_empty()
    }

    pub(crate) fn takes(&mut self, file: u64, token: u64) -> bool {
        self.owns(file) && self.awaiting.remove(&token)
    }

    /// `Script Workspace (test project (fmhost))`.
    pub(crate) fn title(&self) -> String {
        format!("Script Workspace ({})", self.document)
    }

    /// The file's names for step text and the pickers.
    pub(crate) fn set_names(&mut self, names: Option<Arc<CalcSchema>>) {
        let same = match (&self.names_source, &names) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.names_source = names;
            self.refresh_names();
        }
    }

    fn refresh_names(&mut self) {
        self.names = self.names_source.clone();
        if let (Some(names), Some(catalog)) = (&mut self.names, &self.catalog) {
            Arc::make_mut(names).set_scripts(
                catalog
                    .scripts
                    .iter()
                    .map(|script| om_data::meta::ScriptEntry {
                        id: script.id,
                        name: script.name.clone(),
                        kind: script.kind,
                    })
                    .collect(),
            );
        }
        self.names_revision += 1;
    }

    fn editor(&self) -> Option<&Editor> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    fn editor_mut(&mut self) -> Option<&mut Editor> {
        self.active.and_then(|i| self.tabs.get_mut(i))
    }

    fn tab_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id() == id)
    }

    pub(crate) fn accept_catalog(&mut self, result: Result<ScriptCatalog, DataError>) {
        self.busy = false;
        match result {
            Ok(catalog) => {
                // Open scripts keep their drafts; their saved copies follow
                // the catalog, and deleted ones close.
                self.tabs
                    .retain(|t| catalog.script(t.id()).is_some() || t.dirty());
                for tab in &mut self.tabs {
                    if let Some(script) = catalog.script(tab.id()) {
                        let draft = tab.dirty().then(|| tab.script.steps.clone());
                        // Catalog changes own the name and script options;
                        // an open tab owns only its unsaved step edits.
                        tab.script = script.clone();
                        tab.saved(script.clone());
                        if let Some(steps) = draft {
                            tab.script.steps = steps;
                        }
                    }
                }
                self.pane
                    .selected
                    .retain(|id| catalog.scripts.iter().any(|s| s.id == *id));
                let first = self.catalog.is_none();
                self.catalog = Some(catalog);
                self.refresh_names();
                self.active = self
                    .active
                    .filter(|i| *i < self.tabs.len())
                    .or((!self.tabs.is_empty()).then_some(0));
                if first && self.pane.selected.is_empty() {
                    // FileMaker opens on the first script.
                    let first = self.catalog.as_ref().and_then(|c| {
                        c.scripts
                            .iter()
                            .find(|s| s.kind == ScriptKind::Script)
                            .map(|s| s.id)
                    });
                    if let Some(id) = first {
                        self.pane.selected = BTreeSet::from([id]);
                        self.preview(id);
                    }
                }
            }
            Err(error) => self.dialog = Some(Dialog::Alert(error.message)),
        }
    }

    /// The provider's answer to a catalog change.
    pub(crate) fn accept_catalog_saved(&mut self, result: Result<ScriptCatalog, DataError>) {
        let sent = self.sent_catalog.take();
        let open = self.open_when_saved.take();
        match result {
            Ok(catalog) => {
                // New members in the order sent and in the order stored
                // are the same members.
                let ids = match (&self.catalog, sent) {
                    (Some(before), Some(sent)) => {
                        let new = |c: &ScriptCatalog| -> Vec<u64> {
                            c.scripts
                                .iter()
                                .map(|s| s.id)
                                .filter(|id| before.scripts.iter().all(|s| s.id != *id))
                                .collect()
                        };
                        let (asked, stored) = (new(&sent), new(&catalog));
                        if asked.len() == stored.len() {
                            asked.into_iter().zip(stored).collect()
                        } else {
                            BTreeMap::new()
                        }
                    }
                    _ => BTreeMap::new(),
                };
                self.pane.remap(&ids);
                let open = open.map(|id| ids.get(&id).copied().unwrap_or(id));
                self.accept_catalog(Ok(catalog));
                if let Some(id) = open.filter(|id| {
                    self.catalog
                        .as_ref()
                        .is_some_and(|c| c.script(*id).is_some())
                }) {
                    self.open(id);
                }
            }
            Err(error) => {
                self.busy = false;
                self.pane.renaming = None;
                self.dialog = Some(Dialog::Alert(format!(
                    "This change to the scripts could not be saved.\n\n{}",
                    error.message
                )));
            }
        }
    }

    pub(crate) fn accept_saved(
        &mut self,
        result: Result<Script, DataError>,
    ) -> Vec<ScriptWorkspaceAction> {
        self.busy = false;
        let then = self.after_save.take();
        match result {
            Ok(script) => {
                if let Some(catalog) = &mut self.catalog {
                    if let Some(existing) = catalog.scripts.iter_mut().find(|s| s.id == script.id) {
                        *existing = script.clone();
                    }
                }
                if let Some(i) = self.tab_of(script.id) {
                    self.tabs[i].saved(script);
                }
                then.map(|t| self.proceed(t)).unwrap_or_default()
            }
            Err(error) => {
                let name = self
                    .editor()
                    .map(|e| e.script.name.clone())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::Alert(format!(
                    "The script “{name}” could not be saved.\n\n{}",
                    error.message
                )));
                Vec::new()
            }
        }
    }

    /// Show `id` in the preview tab (a single click in the scripts pane).
    fn preview(&mut self, id: u64) {
        if let Some(i) = self.tab_of(id) {
            self.active = Some(i);
            return;
        }
        let Some(script) = self.catalog.as_ref().and_then(|c| c.script(id)).cloned() else {
            return;
        };
        let editor = Editor::new(script, true);
        match self.tabs.iter().position(|t| t.preview && !t.dirty()) {
            Some(i) => {
                self.tabs[i] = editor;
                self.active = Some(i);
            }
            None => {
                self.tabs.push(editor);
                self.active = Some(self.tabs.len() - 1);
            }
        }
    }

    /// Open `id` for editing (a double click): its tab stays open.
    fn open(&mut self, id: u64) {
        self.preview(id);
        if let Some(editor) = self.editor_mut() {
            editor.preview = false;
        }
        self.focus = Focus::Editor;
    }

    /// Continue after a save or a Don't Save answer.
    fn proceed(&mut self, then: Then) -> Vec<ScriptWorkspaceAction> {
        match then {
            Then::Stay => Vec::new(),
            Then::Run(id) => vec![ScriptWorkspaceAction::Run(id)],
            Then::CloseTab(id) => {
                if let Some(i) = self.tab_of(id) {
                    self.tabs.remove(i);
                    self.active = if self.tabs.is_empty() {
                        None
                    } else {
                        Some(i.min(self.tabs.len() - 1))
                    };
                }
                Vec::new()
            }
            Then::CloseWorkspace => self.request_close(),
        }
    }

    /// Close the workspace, asking first for each script with unsaved
    /// changes, as FileMaker does.
    fn request_close(&mut self) -> Vec<ScriptWorkspaceAction> {
        if self.busy {
            return Vec::new();
        }
        match self.tabs.iter().position(Editor::dirty) {
            Some(i) => {
                self.active = Some(i);
                self.dialog = Some(Dialog::SaveChanges {
                    id: self.tabs[i].id(),
                    then: Then::CloseWorkspace,
                });
                Vec::new()
            }
            None => vec![ScriptWorkspaceAction::Close],
        }
    }

    /// Close a tab, asking first when it has unsaved changes.
    fn close_tab(&mut self, i: usize) -> Vec<ScriptWorkspaceAction> {
        if self.busy {
            return Vec::new();
        }
        let id = self.tabs[i].id();
        if self.tabs[i].dirty() {
            self.active = Some(i);
            self.dialog = Some(Dialog::SaveChanges {
                id,
                then: Then::CloseTab(id),
            });
            Vec::new()
        } else {
            self.proceed(Then::CloseTab(id))
        }
    }

    /// Save the active script.
    fn save(&mut self, then: Then) -> Vec<ScriptWorkspaceAction> {
        if self.busy || !self.catalog.as_ref().is_some_and(|c| c.writable) {
            return Vec::new();
        }
        let Some(editor) = self.editor() else {
            return Vec::new();
        };
        if !editor.dirty() {
            return self.proceed(then);
        }
        let script = editor.script.clone();
        self.busy = true;
        self.after_save = Some(then);
        vec![ScriptWorkspaceAction::Save(script)]
    }

    /// Run the active script, saving it first when it has changes.
    fn run(&mut self) -> Vec<ScriptWorkspaceAction> {
        let Some(editor) = self.editor() else {
            return Vec::new();
        };
        let id = editor.id();
        if editor.dirty() {
            self.dialog = Some(Dialog::SaveChanges {
                id,
                then: Then::Run(id),
            });
            return Vec::new();
        }
        vec![ScriptWorkspaceAction::Run(id)]
    }

    /// Store a changed catalog.
    fn change_catalog(&mut self, catalog: ScriptCatalog) -> Vec<ScriptWorkspaceAction> {
        if self.busy || !self.catalog.as_ref().is_some_and(|c| c.writable) {
            return Vec::new();
        }
        self.busy = true;
        self.sent_catalog = Some(catalog.clone());
        vec![ScriptWorkspaceAction::SaveCatalog(catalog)]
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Vec<ScriptWorkspaceAction> {
        let mut actions = Vec::new();
        let title = self.title();
        if ctx.embed_viewports() {
            let area = ctx.content_rect().shrink(12.0);
            let frame = egui::Frame::new()
                .fill(PANE_BG)
                .stroke(Stroke::new(1.0, theme::DIVIDER))
                .corner_radius(CornerRadius::same(10))
                .shadow(ctx.global_style().visuals.window_shadow);
            // No resize grip: FileMaker's window resizes from its edges.
            let corner = ctx.global_style().visuals.resize_corner_size;
            ctx.global_style_mut(|s| s.visuals.resize_corner_size = 0.0);
            egui::Window::new(&title)
                .id(Id::unique("script_workspace_window"))
                .title_bar(false)
                .frame(frame)
                .default_rect(area)
                .min_size(vec2(640.0, 360.0))
                .resizable(true)
                .show(ctx, |ui| {
                    ui.set_min_size(area.size() - vec2(2.0, 2.0));
                    if self.title_bar(ui, &title) {
                        actions.extend(self.request_close());
                    }
                    self.contents(ui, &mut actions);
                });
            ctx.global_style_mut(|s| s.visuals.resize_corner_size = corner);
            self.dialogs(ctx, &mut actions);
        } else {
            let builder = egui::ViewportBuilder::default()
                .with_title(&title)
                .with_inner_size([1100.0, 700.0])
                .with_min_inner_size([640.0, 360.0]);
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("script_workspace"),
                builder,
                |ctx, _class| {
                    if ctx.input(|i| i.viewport().close_requested()) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                        actions.extend(self.request_close());
                    }
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(PANE_BG))
                        .show(ctx, |ui| self.contents(ui, &mut actions));
                    self.dialogs(ctx, &mut actions);
                },
            );
        }
        actions
    }

    /// The in-app window's title bar: the window buttons and the centered
    /// title. True when its close button was clicked.
    fn title_bar(&mut self, ui: &mut Ui, title: &str) -> bool {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), TITLE_BAR), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            CornerRadius {
                nw: 10,
                ne: 10,
                sw: 0,
                se: 0,
            },
            BAR_BG,
        );
        let mut closed = false;
        for (n, color) in [
            Color32::from_rgb(0xFF, 0x5F, 0x57),
            Color32::from_rgb(0xFE, 0xBC, 0x2E),
            Color32::from_rgb(0x28, 0xC8, 0x40),
        ]
        .into_iter()
        .enumerate()
        {
            let center = pos2(rect.left() + 16.0 + n as f32 * 20.0, rect.center().y);
            painter.circle_filled(center, 6.0, color);
            if n == 0 {
                let close = ui.interact(
                    Rect::from_center_size(center, vec2(14.0, 14.0)),
                    ui.scope_id().with("close_window"),
                    Sense::click(),
                );
                close.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Close Window"));
                closed = close.clicked();
            }
        }
        ui.put(
            Rect::from_center_size(rect.center(), vec2(rect.width() - 160.0, rect.height())),
            egui::Label::new(
                egui::RichText::new(title)
                    .font(FontId::new(theme::BODY, theme::semibold()))
                    .color(theme::TEXT),
            ),
        );
        closed
    }

    /// A toolbar button with a glyph.
    fn tool(
        ui: &mut Ui,
        rect: Rect,
        glyph: Glyph,
        label: &str,
        enabled: bool,
        on: bool,
        tip: &str,
    ) -> bool {
        let response = ui.interact(
            rect,
            ui.scope_id().with(label),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if on {
            ui.painter()
                .rect_filled(rect.expand(2.0), 5.0, Color32::from_gray(0xDC));
        }
        let color = match (enabled, on) {
            (false, _) => theme::DISABLED,
            (true, true) => theme::SELECTED,
            (true, false) => Color32::from_gray(0x6A),
        };
        glyphs::paint(ui.painter(), rect, glyph, color);
        response.widget_info(|| {
            if label.starts_with("Show") {
                WidgetInfo::selected(Role::CheckBox, enabled, on, label)
            } else {
                WidgetInfo::labeled(Role::Button, enabled, label)
            }
        });
        let clicked = response.clicked() && enabled;
        response.on_hover_text(tip);
        clicked
    }

    fn toolbar(&mut self, ui: &mut Ui, actions: &mut Vec<ScriptWorkspaceAction>) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), TOOLBAR), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, BAR_BG);
        let at = |x: f32| Rect::from_center_size(pos2(x, rect.center().y), vec2(18.0, 18.0));
        let idle = !self.busy;
        let catalog = self.catalog.clone();
        if Self::tool(
            ui,
            at(rect.left() + 22.0),
            Glyph::Plus,
            "New Script",
            idle && catalog.as_ref().is_some_and(|c| c.writable),
            false,
            "New Script",
        ) {
            if let Some(catalog) = &catalog {
                let mut requests = Vec::new();
                self.pane
                    .create(catalog, ScriptKind::Script, true, &mut requests);
                self.pane_requests(ui.ctx(), requests, actions);
            }
        }
        let runnable = idle && self.editor().is_some();
        if Self::tool(
            ui,
            at(rect.left() + 74.0),
            Glyph::Run,
            "Run Script",
            runnable,
            false,
            "Run Script",
        ) {
            actions.extend(self.run());
        }
        Self::tool(
            ui,
            at(rect.left() + 114.0),
            Glyph::Debug,
            "Debug Script",
            false,
            false,
            "The Script Debugger is not available in OpenMaker yet.",
        );
        let compat_rect = at(rect.right() - 98.0);
        let compat = ui.interact(
            compat_rect,
            ui.scope_id().with("compatibility"),
            Sense::click(),
        );
        glyphs::paint(
            ui.painter(),
            compat_rect,
            Glyph::Compatibility,
            if self.compatibility.is_some() {
                theme::SELECTED
            } else {
                Color32::from_gray(0x6A)
            },
        );
        compat.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Show Compatibility"));
        let popup_id = ui.scope_id().with("compatibility_menu");
        if compat.clicked() {
            egui::Popup::toggle_id(ui.ctx(), popup_id);
        }
        egui::Popup::menu(&compat).id(popup_id).show(|ui| {
            ui.set_min_width(180.0);
            let mut choice = |ui: &mut Ui, label: &str, client: Option<Client>| {
                if ui
                    .selectable_label(self.compatibility == client, label)
                    .clicked()
                {
                    self.compatibility = client;
                    ui.close();
                }
            };
            choice(ui, "All Products", None);
            choice(ui, "FileMaker Pro (macOS)", Some(Client::Mac));
            choice(ui, "FileMaker Pro (Windows)", Some(Client::Windows));
            choice(ui, "FileMaker Server", Some(Client::Server));
            choice(ui, "FileMaker Go", Some(Client::Go));
            choice(ui, "FileMaker WebDirect", Some(Client::WebDirect));
            choice(ui, "Custom Web Publishing", Some(Client::CustomWeb));
        });
        let scripts = self.show_scripts;
        if Self::tool(
            ui,
            at(rect.right() - 62.0),
            Glyph::LeftPane,
            "Show Scripts Pane",
            true,
            scripts,
            "Show or hide the scripts pane",
        ) {
            self.show_scripts = !self.show_scripts;
        }
        let steps = self.show_steps;
        if Self::tool(
            ui,
            at(rect.right() - 28.0),
            Glyph::RightPane,
            "Show Steps Pane",
            true,
            steps,
            "Show or hide the steps pane",
        ) {
            self.show_steps = !self.show_steps;
        }
    }

    fn contents(&mut self, ui: &mut Ui, actions: &mut Vec<ScriptWorkspaceAction>) {
        clipboard::activate(ui.ctx(), self.clipboard_scope, self.names.clone());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // Whether a text field had the keyboard as the frame began: the
        // Return that ends an inline edit must not start a new step.
        let panes_enabled = self.dialog.is_none() && !self.busy;
        let keys_free = panes_enabled && !ui.ctx().egui_wants_keyboard_input();
        self.toolbar(ui, actions);
        if let Some(catalog) = &self.catalog {
            if !catalog.writable {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(catalog.unavailable_reason.as_deref().unwrap_or(
                            "Scripts can be drafted locally. Saving is unavailable for this file.",
                        ))
                        .small(),
                    )
                    .wrap(),
                );
            }
        }
        let body = ui.available_rect_before_wrap();
        ui.painter()
            .hline(body.x_range(), body.top(), Stroke::new(1.0, RULE));
        let scripts_width = if self.show_scripts { SCRIPTS_PANE } else { 0.0 };
        let steps_width = if self.show_steps { STEPS_PANE } else { 0.0 };
        let scripts_rect = Rect::from_min_size(body.min, vec2(scripts_width, body.height()));
        let steps_rect = Rect::from_min_max(pos2(body.right() - steps_width, body.top()), body.max);
        let editor_rect = Rect::from_min_max(
            pos2(scripts_rect.right(), body.top()),
            pos2(steps_rect.left(), body.bottom()),
        );
        let catalog = self.catalog.clone().unwrap_or_default();
        if self.show_scripts {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(scripts_rect)
                    .id_salt("scripts"),
            );
            child.set_clip_rect(scripts_rect);
            if child.rect_contains_pointer(scripts_rect) && child.input(|i| i.pointer.any_pressed())
            {
                self.focus = Focus::Scripts;
            }
            let open: BTreeSet<u64> = self.tabs.iter().map(Editor::id).collect();
            let requests = child.add_enabled_ui(panes_enabled, |ui| {
                self.pane.ui(
                    ui,
                    &catalog,
                    catalog.writable,
                    keys_free && self.focus == Focus::Scripts,
                    &open,
                )
            });
            self.pane_requests(ui.ctx(), requests.inner, actions);
            ui.painter()
                .vline(scripts_rect.right(), body.y_range(), Stroke::new(1.0, RULE));
        }
        {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(editor_rect)
                    .id_salt("editor"),
            );
            child.set_clip_rect(editor_rect);
            if child.rect_contains_pointer(editor_rect) && child.input(|i| i.pointer.any_pressed())
            {
                self.focus = Focus::Editor;
            }
            self.editor_area(&mut child, editor_rect, actions);
        }
        if self.show_steps {
            ui.painter()
                .vline(steps_rect.left(), body.y_range(), Stroke::new(1.0, RULE));
            let mut child =
                ui.new_child(egui::UiBuilder::new().max_rect(steps_rect).id_salt("steps"));
            child.set_clip_rect(steps_rect);
            if child.rect_contains_pointer(steps_rect) && child.input(|i| i.pointer.any_pressed()) {
                self.focus = Focus::Steps;
            }
            let insert = child
                .add_enabled_ui(panes_enabled, |ui| {
                    self.steps.ui(
                        ui,
                        self.compatibility,
                        keys_free && self.focus == Focus::Steps,
                    )
                })
                .inner;
            if let (Some(id), Some(editor)) = (insert, self.editor_mut()) {
                editor.insert_new(id);
                self.focus = Focus::Editor;
            }
        }
        ui.advance_cursor_after_rect(body);
        self.keys(ui, keys_free, actions);
    }

    /// The tab bar and the active script's steps.
    fn editor_area(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<ScriptWorkspaceAction>) {
        ui.painter().rect_filled(rect, 0.0, theme::CONTENT_BG);
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TAB_BAR));
        ui.painter().rect_filled(bar, 0.0, BAR_BG);
        ui.painter()
            .hline(bar.x_range(), bar.bottom(), Stroke::new(1.0, RULE));
        let count = self.tabs.len().max(1);
        let width = (bar.width() - 30.0) / count as f32;
        let mut close = None;
        for (i, tab) in self.tabs.iter().enumerate() {
            let tab_rect = Rect::from_min_size(
                pos2(bar.left() + i as f32 * width, bar.top()),
                vec2(width, TAB_BAR),
            );
            let active = self.active == Some(i);
            if active && self.tabs.len() > 1 {
                ui.painter()
                    .rect_filled(tab_rect, 0.0, Color32::from_gray(0xF7));
            }
            if i > 0 {
                ui.painter()
                    .vline(tab_rect.left(), tab_rect.y_range(), Stroke::new(1.0, RULE));
            }
            let mut name = tab.script.name.clone();
            if tab.dirty() {
                name.push_str(" *");
            }
            // The tab's name, gray as 35 shows it.
            let mut job = egui::text::LayoutJob::default();
            job.append(
                &name,
                0.0,
                egui::TextFormat {
                    font_id: theme::small(),
                    color: if active { theme::TEXT } else { theme::CAPTION },
                    ..Default::default()
                },
            );
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let response = ui.interact(
                tab_rect,
                ui.scope_id().with(("tab", tab.id())),
                Sense::click(),
            );
            ui.painter()
                .galley(tab_rect.center() - galley.size() / 2.0, galley, theme::TEXT);
            response.widget_info(|| {
                WidgetInfo::selected(Role::Button, true, active, format!("Tab {name}"))
            });
            if response.clicked() {
                self.active = Some(i);
            }
            if response.double_clicked() || response.middle_clicked() {
                close = Some(i);
            }
            if response.hovered() || active && self.tabs.len() > 1 {
                let x_rect = Rect::from_center_size(
                    pos2(tab_rect.left() + 12.0, tab_rect.center().y),
                    vec2(12.0, 12.0),
                );
                let x = ui.interact(
                    x_rect,
                    ui.scope_id().with(("close_tab", tab.id())),
                    Sense::click(),
                );
                glyphs::paint(ui.painter(), x_rect, Glyph::Close, theme::WEAK_TEXT);
                x.widget_info(|| {
                    WidgetInfo::labeled(Role::Button, true, format!("Close {}", tab.script.name))
                });
                if x.clicked() {
                    close = Some(i);
                }
            }
        }
        if let Some(i) = close {
            actions.extend(self.close_tab(i));
        }
        // The preview eye (35).
        let preview = self.editor().is_some_and(|e| e.preview);
        let eye_rect =
            Rect::from_center_size(pos2(bar.right() - 14.0, bar.center().y), vec2(16.0, 16.0));
        if self.editor().is_some() {
            let eye = ui.interact(eye_rect, ui.scope_id().with("preview"), Sense::click());
            glyphs::paint(
                ui.painter(),
                eye_rect,
                Glyph::Eye,
                if preview {
                    Color32::BLACK
                } else {
                    theme::DISABLED
                },
            );
            eye.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, preview, "Preview"));
            if eye.clicked() {
                if let Some(editor) = self.editor_mut() {
                    editor.preview = false;
                }
            }
        }
        let steps_rect = Rect::from_min_max(pos2(rect.left(), bar.bottom() + 1.0), rect.max);
        let focused = self.focus == Focus::Editor;
        let names = self.names.clone();
        let names_revision = self.names_revision;
        let compatibility = self.compatibility;
        let Some(active) = self.active.filter(|i| *i < self.tabs.len()) else {
            return;
        };
        // A refused edit (red), else what a paste could not read (amber).
        let banner = match (&self.tabs[active].refusal, &self.tabs[active].notice) {
            (Some(refusal), _) => Some((
                refusal.clone(),
                Color32::from_rgb(0xFD, 0xEC, 0xEA),
                Color32::from_rgb(0xAF, 0x23, 0x23),
            )),
            (None, Some(notice)) => Some((
                notice.clone(),
                Color32::from_rgb(0xFF, 0xF4, 0xD6),
                Color32::from_rgb(0x7A, 0x52, 0x00),
            )),
            (None, None) => None,
        };
        let footer = if banner.is_some() { 22.0 } else { 0.0 };
        let list_rect = Rect::from_min_max(
            steps_rect.min,
            pos2(steps_rect.right(), steps_rect.bottom() - footer),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(list_rect)
                .id_salt(("steps_of", self.tabs[active].id())),
        );
        let step_names: &dyn StepNames = match &names {
            Some(schema) => schema.as_ref(),
            None => &NoStepNames,
        };
        let requests = child
            .add_enabled_ui(!self.busy, |ui| {
                self.tabs[active].ui(ui, step_names, names_revision, focused, compatibility)
            })
            .inner;
        if let Some((message, fill, text)) = banner {
            let banner = Rect::from_min_max(
                pos2(steps_rect.left(), steps_rect.bottom() - footer),
                steps_rect.max,
            );
            ui.painter().rect_filled(banner, 0.0, fill);
            ui.put(
                banner.shrink2(vec2(8.0, 0.0)),
                egui::Label::new(egui::RichText::new(message).size(theme::SMALL).color(text)),
            );
        }
        for request in requests {
            self.editor_request(request);
        }
    }

    fn editor_request(&mut self, request: EditorRequest) {
        let Some(active) = self.active else {
            return;
        };
        let id = self.tabs[active].id();
        match request {
            EditorRequest::Focus => self.focus = Focus::Editor,
            EditorRequest::Options(index) => {
                let step = self.tabs[active].script.steps[index].clone();
                self.dialog = Some(Dialog::Options {
                    id,
                    index,
                    dialog: Box::new(options::StepOptions::new(step, self.names.clone())),
                });
            }
            EditorRequest::Calculation { index, target } => {
                let step = &self.tabs[active].script.steps[index];
                let calc = parameter_calculation(step, target);
                self.dialog = Some(Dialog::Calculation {
                    id,
                    index,
                    target,
                    dialog: Box::new(calculation::SpecifyCalculation::new(
                        // A step's calculation is not a field's: FileMaker
                        // names a field only in a field definition's title.
                        "Specify Calculation".to_owned(),
                        calc.as_ref(),
                        self.names.clone(),
                    )),
                });
            }
            EditorRequest::CompileInline {
                index,
                target,
                text,
                original,
            } => match calculation::compile(&text, self.names.as_deref()) {
                Ok(calc) => self.set_step_calculation(
                    active,
                    index,
                    target,
                    (!text.trim().is_empty()).then_some(&calc),
                ),
                Err(message) => {
                    self.tabs[active].inline = Some(editor::Inline::Calculation {
                        index,
                        target,
                        text,
                        original,
                    });
                    self.dialog = Some(Dialog::Alert(message));
                }
            },
        }
    }

    /// The file's names, or none.
    fn names(&self) -> &dyn StepNames {
        match &self.names {
            Some(schema) => schema.as_ref(),
            None => &NoStepNames,
        }
    }

    /// Copy whole scripts (their open drafts when they have one).
    fn copy_scripts(&self, ctx: &egui::Context, ids: &[u64]) {
        let Some(catalog) = &self.catalog else {
            return;
        };
        // In the order the scripts pane shows them (the catalog's), not the
        // order of their ids.
        let scripts = catalog
            .scripts
            .iter()
            .filter(|s| ids.contains(&s.id))
            .map(|s| match self.tab_of(s.id) {
                Some(i) => self.tabs[i].script.steps.clone(),
                None => s.steps.clone(),
            })
            .collect();
        clipboard::put_scripts(ctx, scripts);
    }

    /// Paste the system clipboard's `text` after the tab's selection, as
    /// one undoable edit: this workspace's own copy whole, other text
    /// (OpenMaker's step text or FileMaker's XML) read against the file's
    /// names. Either is held to the same limits.
    fn paste_steps(&mut self, ctx: &egui::Context, tab: usize, text: &str) {
        let copied = clipboard::get(ctx, text);
        let pasted = if copied.is_empty() {
            parse_steps(text, self.names(), Origin::Unknown)
                .map(|pasted| self.tabs[tab].paste_text(pasted))
        } else {
            paste_limits(text, copied.len()).map(|()| {
                self.tabs[tab].paste(&copied);
            })
        };
        if let Err(reason) = pasted {
            self.tabs[tab].refusal = Some(format!("Nothing was pasted. {reason}"));
        }
        self.focus = Focus::Editor;
    }

    fn set_step_calculation(
        &mut self,
        tab: usize,
        index: usize,
        target: CalculationTarget,
        calc: Option<&om_data::Calculation>,
    ) {
        let mut step = self.tabs[tab].script.steps[index].clone();
        match set_parameter_calculation(&mut step, target, calc) {
            Ok(()) => {
                self.tabs[tab].set_step(index, step);
            }
            Err(message) => self.dialog = Some(Dialog::Alert(message)),
        }
    }

    fn pane_requests(
        &mut self,
        ctx: &egui::Context,
        requests: Vec<PaneRequest>,
        actions: &mut Vec<ScriptWorkspaceAction>,
    ) {
        for request in requests {
            match request {
                PaneRequest::Preview(id) => self.preview(id),
                PaneRequest::Open(id) => {
                    // A new script opens once the catalog holds it.
                    if self
                        .catalog
                        .as_ref()
                        .is_some_and(|c| c.script(id).is_some())
                    {
                        self.open(id);
                    } else if self
                        .sent_catalog
                        .as_ref()
                        .is_some_and(|c| c.script(id).is_some())
                    {
                        self.open_when_saved = Some(id);
                    }
                }
                PaneRequest::Change(catalog) => actions.extend(self.change_catalog(catalog)),
                PaneRequest::Delete(ids) => self.dialog = Some(Dialog::ConfirmDelete(ids)),
                PaneRequest::Run(id) => actions.push(ScriptWorkspaceAction::Run(id)),
                PaneRequest::CopyText(ids) => self.copy_scripts(ctx, &ids),
            }
        }
    }

    /// The workspace's keyboard: its commands, then the focused pane's.
    fn keys(&mut self, ui: &mut Ui, keys_free: bool, actions: &mut Vec<ScriptWorkspaceAction>) {
        if self.dialog.is_some() || self.busy {
            return;
        }
        let shortcut = |ui: &mut Ui, modifiers: Modifiers, key: Key| {
            ui.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(modifiers, key)))
        };
        let command_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
        if shortcut(ui, command_shift, Key::W) {
            actions.extend(self.request_close());
            return;
        }
        if shortcut(ui, Modifiers::COMMAND, Key::S) {
            actions.extend(self.save(Then::Stay));
            return;
        }
        if shortcut(ui, Modifiers::COMMAND, Key::R) {
            actions.extend(self.run());
            return;
        }
        if shortcut(ui, Modifiers::COMMAND, Key::W) {
            if let Some(i) = self.active {
                actions.extend(self.close_tab(i));
            }
            return;
        }
        if shortcut(ui, Modifiers::COMMAND, Key::N) {
            if let Some(catalog) = self.catalog.clone() {
                let mut requests = Vec::new();
                self.pane
                    .create(&catalog, ScriptKind::Script, true, &mut requests);
                self.pane_requests(ui.ctx(), requests, actions);
            }
            return;
        }
        if !keys_free || ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        if self.focus == Focus::Scripts {
            // ⌘C in the scripts pane copies the selected scripts' steps.
            if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
                let ids: Vec<u64> = self.pane.selected.iter().copied().collect();
                self.copy_scripts(ui.ctx(), &ids);
            }
            return;
        }
        if self.focus != Focus::Editor {
            return;
        }
        // The clipboard, from the system's copy, cut and paste events.
        let events: Vec<egui::Event> = ui.input(|i| i.events.clone());
        let ctx = ui.ctx().clone();
        let Some(active) = self.active.filter(|i| *i < self.tabs.len()) else {
            return;
        };
        for event in &events {
            match event {
                egui::Event::Copy => clipboard::put(&ctx, &self.tabs[active], false),
                egui::Event::Cut => {
                    clipboard::put(&ctx, &self.tabs[active], true);
                    self.tabs[active].delete();
                }
                egui::Event::Paste(text) => self.paste_steps(&ctx, active, text),
                _ => {}
            }
        }
        let requests = self.tabs[active].keys(ui);
        for request in requests {
            self.editor_request(request);
        }
    }

    /// The workspace's dialogs: save prompts, alerts, delete confirmation,
    /// step options and Specify Calculation.
    fn dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<ScriptWorkspaceAction>) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        match dialog {
            Dialog::SaveChanges { id, then } => {
                let name = self
                    .tab_of(id)
                    .map(|i| self.tabs[i].script.name.clone())
                    .unwrap_or_default();
                let writable = self.catalog.as_ref().is_some_and(|c| c.writable);
                let buttons: &[&str] = if writable {
                    &["Don't Save", "Cancel", "Save"]
                } else {
                    &["Don't Save", "Cancel"]
                };
                let answer = alert(ctx, &format!("Save changes to script “{name}”?"), buttons);
                match answer {
                    Some(0) => {
                        if let Some(i) = self.tab_of(id) {
                            self.tabs[i].revert();
                        }
                        actions.extend(self.proceed(then));
                    }
                    Some(1) => {}
                    Some(_) => {
                        if let Some(i) = self.tab_of(id) {
                            self.active = Some(i);
                        }
                        actions.extend(self.save(then));
                    }
                    None => self.dialog = Some(Dialog::SaveChanges { id, then }),
                }
            }
            Dialog::Alert(message) => {
                if alert(ctx, &message, &["OK"]).is_none() {
                    self.dialog = Some(Dialog::Alert(message));
                }
            }
            Dialog::ConfirmDelete(ids) => {
                let catalog = self.catalog.clone().unwrap_or_default();
                let names: Vec<&str> = ids
                    .iter()
                    .filter_map(|id| catalog.scripts.iter().find(|s| s.id == *id))
                    .map(|s| s.name.as_str())
                    .collect();
                let message = match names.as_slice() {
                    [one] => format!("Are you sure you want to permanently delete “{one}”?"),
                    _ => format!(
                        "Are you sure you want to permanently delete these {} items?",
                        names.len()
                    ),
                };
                match alert(ctx, &message, &["Cancel", "Delete"]) {
                    Some(1) => {
                        let mut next = catalog;
                        catalog_pane::delete(&mut next, &ids);
                        self.pane.selected.clear();
                        actions.extend(self.change_catalog(next));
                    }
                    Some(_) => {}
                    None => self.dialog = Some(Dialog::ConfirmDelete(ids)),
                }
            }
            Dialog::Options {
                id,
                index,
                mut dialog,
            } => match dialog.show(ctx) {
                Some(options::OptionsOutcome::Ok(step)) => {
                    if let Some(i) = self.tab_of(id) {
                        self.tabs[i].set_step(index, step);
                    }
                }
                Some(options::OptionsOutcome::Cancel) => {}
                None => self.dialog = Some(Dialog::Options { id, index, dialog }),
            },
            Dialog::Calculation {
                id,
                index,
                target,
                mut dialog,
            } => match dialog.show(ctx) {
                Some(calculation::CalcOutcome::Ok(calc)) => {
                    if let Some(i) = self.tab_of(id) {
                        self.set_step_calculation(i, index, target, Some(&calc));
                    }
                }
                Some(calculation::CalcOutcome::Cancel) => {}
                None => {
                    self.dialog = Some(Dialog::Calculation {
                        id,
                        index,
                        target,
                        dialog,
                    });
                }
            },
        }
    }
}

/// A FileMaker alert: the message and buttons, the last one the default
/// (Return); Escape picks a Cancel button. The answer is the button's
/// index.
pub(crate) fn alert(ctx: &egui::Context, message: &str, buttons: &[&str]) -> Option<usize> {
    let mut answer = None;
    let frame = egui::Frame::new()
        .fill(theme::DIALOG_BG)
        .stroke(Stroke::new(1.0, theme::DIVIDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(20)
        .shadow(ctx.global_style().visuals.window_shadow);
    egui::Modal::new(Id::unique(("script_workspace_alert", message)))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(24))
        .show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(
                egui::RichText::new(message)
                    .size(theme::BODY)
                    .color(theme::TEXT),
            );
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (n, label) in buttons.iter().enumerate().rev() {
                    let default = n + 1 == buttons.len();
                    if widgets::dialog_button(ui, label, default, true).clicked() {
                        answer = Some(n);
                    }
                    ui.add_space(8.0);
                }
            });
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                answer = Some(buttons.len() - 1);
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                answer = buttons.iter().position(|b| *b == "Cancel").or(answer);
            }
        });
    answer
}

#[cfg(test)]
pub(crate) mod tests;
