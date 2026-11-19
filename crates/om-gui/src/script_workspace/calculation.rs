//! Specify Calculation (capture 42) for a step's calculation option, and
//! the compiler the workspace's inline edits share.
//!
//! The dialog is a movable, resizable window over the workspace: under its
//! title bar the context popup, the cursor's line and column, Help and the
//! functions pane toggle; then the fields pane, the formula editor with
//! FileMaker's syntax colors, the operator strip and the functions pane
//! with its Description panel; at the bottom the calculation field's
//! options where they apply (not for script steps), Cancel and OK.
//!
//! OK compiles the text with `om-formula`. A formula that does not compile
//! keeps the dialog open: FileMaker's alert for the error, then the
//! error's range selected in the editor.
//!
//! Typing a name opens a list of the names completing it under the cursor
//! (`om_data::formula_editor::completion`, the query `om-author`'s
//! `complete_calculation` answers too): Up and Down move in it, Return,
//! Tab or a click write the chosen name over the typed prefix, Escape
//! closes it. Its look and keys are OpenMaker's (docs/desktop-client.md).

mod fields;
mod functions;
mod storage;
mod syntax;

use std::ops::Range;
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange};
use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Margin, Modifiers,
    Painter, Rect, Response, Role, Sense, Stroke, StrokeKind, TextBuffer, Ui, Vec2, WidgetInfo,
};
use om_data::calc::ValueType;
use om_data::formula::{Field, Names, NoNames, Token};
use om_data::formula_editor::{Candidate, CandidateKind, Completion, Scope};
use om_data::host::CalcSchema;
use om_data::scripts::display::{calculation_text, NoStepNames, StepNames};
use om_data::Calculation;

use super::glyphs::{self, Glyph};
use crate::{theme, widgets};

use fields::FieldsPane;
use functions::FunctionsPane;
use syntax::Insertion;

/// The answer of the dialog.
pub(crate) enum CalcOutcome {
    Cancel,
    Ok(Calculation),
}

/// A formula that does not compile: FileMaker's alert text for the error
/// code (catalogs/error-codes.csv) and the range FileMaker selects, in
/// characters of the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompileError {
    pub message: String,
    pub range: Range<usize>,
}

/// Compile formula text as a step calculation (no context occurrence, as
/// FileMaker stores step calculations: calc-encoding.md 1). An error is
/// FileMaker's message for its code.
pub(crate) fn compile(text: &str, names: Option<&CalcSchema>) -> Result<Calculation, String> {
    compile_in(text, names, None, false).map_err(|e| e.message)
}

/// Compile with unqualified field names read as fields of `occurrence`
/// (the dialog's context popup), the decimal separator of the file's
/// locale, and `Self` where field calculations allow it. The streams are
/// stored with occurrence 0, as script steps store them.
fn compile_in(
    text: &str,
    names: Option<&CalcSchema>,
    occurrence: Option<u64>,
    support_self: bool,
) -> Result<Calculation, CompileError> {
    let no_names = NoNames;
    let (calc_names, comma): (&dyn Names, bool) = match names {
        Some(schema) => (schema.names(), schema.locale.decimal_separator == ','),
        None => (&no_names, false),
    };
    let ctx = om_data::formula::Context {
        occurrence,
        support_self,
        decimal_comma: comma,
        ..om_data::formula::Context::new(calc_names)
    };
    match om_data::formula::compile(text, &ctx) {
        Ok(compiled) => Ok(Calculation::new(
            Some(om_data::formula::formatted::encode(&compiled.tokens)),
            om_data::formula::polish::encode(&compiled.expr),
        )),
        Err(error) => {
            let code = i32::from(error.code.number());
            let message = om_data::runtime::ErrorCode(code)
                .message()
                .unwrap_or("There is an error in this calculation.");
            Err(CompileError {
                message: message.to_owned(),
                range: syntax::char_range(text, error.span),
            })
        }
    }
}

/// A calculation field's options (42's bottom row): shown only where they
/// apply, not for script step calculations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldOptions {
    pub result: ValueType,
    pub repetitions: u32,
    /// The bottom checkbox: "Do not evaluate if all referenced fields are
    /// empty", or for a validation calculation "Validate only if field has
    /// been modified".
    pub skip_when_empty: bool,
    /// Storage Options; `None` where they cannot be changed (the button is
    /// then unavailable).
    pub storage: Option<om_data::calc_field::CalcStorage>,
    /// A field option's formula rather than a calculation field's: the
    /// result type and repetitions are the field's, shown unavailable.
    pub option: Option<OptionKind>,
}

/// Which option of a normal field a formula is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OptionKind {
    AutoEnter,
    Validation,
    /// "Display custom message if validation fails".
    Message,
}

// Metrics of capture 42, in points.
/// The window's size in 42.
const SIZE: Vec2 = vec2(1397.0, 836.0);
const MIN_SIZE: Vec2 = vec2(780.0, 440.0);
const TITLE_BAR: f32 = 28.0;
/// The context row, between the title bar's rule and its own.
const TOP_ROW: f32 = 30.0;
/// The bottom bar with the field options (42), and without them.
const FIELD_BAR: f32 = 78.0;
const STEP_BAR: f32 = 60.0;
const FIELDS_PANE: f32 = 223.0;
const OPERATORS: f32 = 34.0;
const FUNCTIONS_PANE: f32 = 242.0;
/// The editor's monospaced text and its line pitch (42: 13 pt lines).
const EDITOR_FONT: f32 = 11.0;
const LINE: f32 = 13.0;
/// The lists' names and signatures.
pub(super) const LIST_FONT: f32 = 11.0;
/// Labels and the Description text.
pub(super) const BODY_TEXT: f32 = 11.0;

/// The rules between the panes (42).
pub(super) const RULE: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xE6);
const TOP_RULE: Color32 = Color32::from_rgb(0xD9, 0xD9, 0xD9);
const TITLE_BG: Color32 = Color32::from_rgb(0xF4, 0xF4, 0xF4);
const TITLE_RULE: Color32 = Color32::from_rgb(0xCA, 0xCA, 0xCA);
const STRIP_BG: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xEC);
const WINDOW_BORDER: Color32 = Color32::from_rgb(0xA9, 0xA9, 0xAC);
const OPERATOR_TEXT: Color32 = Color32::from_rgb(0x6A, 0x6A, 0x6A);
/// The lists' sort and view bars (42).
pub(super) const BARS: Color32 = Color32::from_rgb(0x78, 0x78, 0x78);
/// The functions pane toggle's blue (42).
const PANE_BLUE: Color32 = Color32::from_rgb(0x25, 0x4A, 0xDB);
/// The blue of default buttons and popup steppers, top to bottom (42).
const BLUE_TOP: Color32 = Color32::from_rgb(0x3D, 0x4F, 0xD1);
const BLUE_BOTTOM: Color32 = Color32::from_rgb(0x14, 0x29, 0xC8);

const HELP_URL: &str = "https://help.claris.com/en/pro-help/content/specify-calculation.html";

/// A selection to put in the editor, and whether to give it the focus.
/// It waits for the frame after the one that made it: egui takes the
/// focus from a widget when a click lands elsewhere in the same frame,
/// and gives none to a window under a modal that closed in the last one
/// (the error alert).
struct Pending {
    selection: Range<usize>,
    focus: bool,
    ready: bool,
}

impl Pending {
    fn next_frame(selection: Range<usize>, focus: bool) -> Self {
        Self {
            selection,
            focus,
            ready: false,
        }
    }
}

/// The completion list under the cursor while a name is typed.
struct CompletionList {
    completion: Completion,
    /// The highlighted row.
    row: usize,
    /// Scroll the highlighted row into view.
    reveal: bool,
    /// The cursor's rect where the typed name starts: the list hangs
    /// under it.
    anchor: Option<Rect>,
    /// Where the list was drawn: a press there keeps it open.
    rect: Option<Rect>,
}

pub(crate) struct SpecifyCalculation {
    title: String,
    /// The editor's text (line breaks as LF, syntax::to_editor).
    text: String,
    /// The text as opened, and the source it shows: OK on unchanged text
    /// compiles the source exactly as it was printed.
    opened: String,
    opened_source: String,
    names: Option<Arc<CalcSchema>>,
    /// The context popup: the occurrence unqualified names compile against.
    context: Option<u64>,
    /// A calculation field's options; `None` for script steps.
    field: Option<FieldOptions>,
    repetitions: String,
    fields: FieldsPane,
    functions: FunctionsPane,
    /// The functions pane's list: FileMaker's and the file's.
    function_list: Vec<functions::Entry>,
    show_functions: bool,
    /// The window on screen; placed at its first frame.
    rect: Option<Rect>,
    /// The rect before the zoom button filled the screen.
    unzoomed: Option<Rect>,
    /// The window's rect when a move or resize drag began: the window
    /// follows the pointer's whole way from where it was pressed.
    dragging: Option<Rect>,
    /// The editor's selection (characters), kept while the editor has the
    /// focus: list and button clicks take the focus but insert here.
    selection: Range<usize>,
    pending: Option<Pending>,
    /// FileMaker's alert for a formula that does not compile, and the
    /// range to select once it is dismissed.
    alert: Option<(String, Range<usize>)>,
    /// The names completing the one typed at the cursor, while listed.
    completion: Option<CompletionList>,
    /// Storage Options, while open.
    storage: Option<storage::StorageOptions>,
    opened_now: bool,
}

fn editor_id() -> Id {
    Id::unique("specify_calculation_editor")
}

/// The context of a step calculation: the occurrence its unqualified
/// fields were compiled against, else the file's first occurrence.
fn initial_context(calculation: Option<&Calculation>, schema: Option<&CalcSchema>) -> Option<u64> {
    let unqualified = calculation
        .and_then(|c| c.formatted.as_deref())
        .and_then(|bytes| om_data::formula::formatted::decode(bytes).ok())
        .and_then(|tokens| {
            tokens.iter().find_map(|t| match t {
                Token::Field(f) if f.flags & Field::TABLE_PRESENT == 0 => f.occurrence.id(),
                _ => None,
            })
        });
    unqualified
        .filter(|o| schema.is_none_or(|s| s.occurrence(*o).is_some()))
        .or_else(|| schema.and_then(|s| s.occurrences_in_order().first().map(|o| o.id)))
}

impl SpecifyCalculation {
    pub(crate) fn new(
        title: String,
        calculation: Option<&Calculation>,
        names: Option<Arc<CalcSchema>>,
    ) -> Self {
        let step_names: &dyn StepNames = match &names {
            Some(schema) => schema.as_ref(),
            None => &NoStepNames,
        };
        let source = calculation
            .map(|c| calculation_text(c, step_names))
            .unwrap_or_default();
        let text = syntax::to_editor(&source);
        let context = initial_context(calculation, names.as_deref());
        let function_list = functions::listed(names.as_deref());
        Self {
            title,
            opened: text.clone(),
            text,
            opened_source: source,
            names,
            context,
            field: None,
            repetitions: String::new(),
            fields: FieldsPane::default(),
            functions: FunctionsPane::default(),
            function_list,
            show_functions: true,
            rect: None,
            unzoomed: None,
            dragging: None,
            selection: 0..0,
            pending: None,
            alert: None,
            completion: None,
            storage: None,
            opened_now: true,
        }
    }

    /// The dialog opened on formula text: a calculation field's, as
    /// Manage Database's schema shows it.
    pub(crate) fn with_text(title: String, source: &str, names: Option<Arc<CalcSchema>>) -> Self {
        let mut dialog = Self::new(title, None, names);
        dialog.text = syntax::to_editor(source);
        dialog.opened.clone_from(&dialog.text);
        dialog.opened_source = source.to_owned();
        dialog
    }

    /// Show FileMaker's alert over the dialog: why the change OK made was
    /// refused. The dialog stays open.
    pub(crate) fn show_alert(&mut self, message: &str) {
        self.alert = Some((message.to_owned(), self.selection.clone()));
    }

    /// For a calculation field of the table `occurrence` shows: its options
    /// under the editor, `Self` allowed, fields of its table inserted
    /// without their table. Manage Database opens it this way.
    pub(crate) fn for_field(mut self, occurrence: u64, options: FieldOptions) -> Self {
        self.context = Some(occurrence);
        self.repetitions = options.repetitions.to_string();
        self.field = Some(options);
        self
    }

    /// For a calculation field, the formula's text as OK compiled it and
    /// the occurrence it is evaluated from.
    pub(crate) fn field_formula(&self) -> Option<(String, u64)> {
        self.field.as_ref()?;
        let source = if self.text == self.opened {
            self.opened_source.clone()
        } else {
            syntax::to_source(&self.text)
        };
        Some((source, self.context?))
    }

    /// The field options as edited (`None` for script steps).
    pub(crate) fn field_options(&self) -> Option<FieldOptions> {
        self.field.clone().map(|mut options| {
            options.repetitions = self
                .repetitions
                .trim()
                .parse::<u32>()
                .map_or(options.repetitions, |n| n.clamp(1, 32000));
            options
        })
    }

    /// OK: the editor's text compiled. Unchanged text compiles exactly the
    /// source it was opened with, so the stored streams come back the same.
    fn accept(&self) -> Result<Calculation, CompileError> {
        let source = if self.text == self.opened {
            self.opened_source.clone()
        } else {
            syntax::to_source(&self.text)
        };
        let mut calc = compile_in(
            &source,
            self.names.as_deref(),
            self.context,
            self.field.is_some(),
        )?;
        if let (Some(_), Some(occurrence)) = (&self.field, self.context) {
            calc.context = Some(om_data::formula::Key::from_id(occurrence).0);
        }
        Ok(calc)
    }

    /// Insert at the editor's selection and give the editor the focus.
    fn insert(&mut self, insertion: &Insertion) {
        let selection = insertion.apply(&mut self.text, self.selection.clone());
        self.pending = Some(Pending::next_frame(selection, true));
        self.completion = None;
    }

    /// The names completing the one that ends at the cursor, listed when
    /// there are any.
    fn complete(&mut self) {
        let scope = Scope {
            occurrence: self.context,
            unqualified: self.field.is_some(),
        };
        let caret = self.selection.is_empty().then_some(self.selection.end);
        self.completion = caret
            .and_then(|c| {
                om_data::formula_editor::complete(&self.text, c, scope, self.names.as_deref()).ok()
            })
            .filter(|c| c.refused.is_none() && !c.candidates.is_empty())
            .map(|completion| CompletionList {
                completion,
                row: 0,
                reveal: true,
                anchor: None,
                rect: None,
            });
    }

    /// Write candidate `row` over the typed name, close the list and
    /// select what the candidate selects (now, before the editor is drawn,
    /// or in the next frame).
    fn choose(&mut self, row: usize, now: bool) {
        let Some(list) = self.completion.take() else {
            return;
        };
        if let Some(candidate) = list.completion.candidates.get(row) {
            let selection = list.completion.apply(candidate, &mut self.text);
            self.pending = Some(Pending {
                selection,
                focus: true,
                ready: now,
            });
        }
    }

    /// The list's keys, taken before the editor sees them: Up and Down
    /// move, Return or Tab choose, Escape closes the list (not the
    /// dialog).
    fn completion_keys(&mut self, ctx: &egui::Context) {
        let Some(list) = &mut self.completion else {
            return;
        };
        let pressed = |key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        if pressed(Key::Escape) {
            self.completion = None;
            return;
        }
        let last = list.completion.candidates.len().saturating_sub(1);
        if pressed(Key::ArrowDown) {
            list.row = (list.row + 1).min(last);
            list.reveal = true;
        }
        if pressed(Key::ArrowUp) {
            list.row = list.row.saturating_sub(1);
            list.reveal = true;
        }
        if pressed(Key::Enter) || pressed(Key::Tab) {
            let row = list.row;
            self.choose(row, true);
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<CalcOutcome> {
        let screen = ctx.content_rect();
        let rect = self.rect.unwrap_or_else(|| {
            Rect::from_center_size(screen.center(), SIZE.min(screen.size() - vec2(24.0, 24.0)))
        });
        let rect = fit(rect, screen);
        self.rect = Some(rect);
        let mut outcome = None;
        let frame = egui::Frame::new()
            .fill(theme::DIALOG_BG)
            .stroke(Stroke::new(1.0, WINDOW_BORDER))
            .corner_radius(CornerRadius::same(10))
            .shadow(ctx.global_style().visuals.window_shadow);
        let area = egui::Area::new(Id::unique("specify_calculation"))
            .kind(egui::UiKind::Modal)
            .sense(Sense::hover())
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .interactable(true);
        // The open list owns its keys before the modal and editor handle them.
        if self.alert.is_none()
            && ctx.memory(|m| m.top_modal_layer() == Some(area.layer()))
            && !egui::Popup::is_any_open(ctx)
        {
            self.completion_keys(ctx);
        }
        let modal = egui::Modal::new(Id::unique("specify_calculation"))
            .area(area)
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(24))
            .show(ctx, |ui| self.window(ui, rect, &mut outcome));
        if let Some(dialog) = &mut self.storage {
            match dialog.show(ctx) {
                Some(storage::Answer::Ok(chosen)) => {
                    if let Some(field) = &mut self.field {
                        field.storage = Some(chosen);
                    }
                    self.storage = None;
                }
                Some(storage::Answer::Cancel) => self.storage = None,
                None => {}
            }
        } else if let Some((message, range)) = self.alert.clone() {
            if super::alert(ctx, &message, &["OK"]).is_some() {
                self.alert = None;
                self.pending = Some(Pending::next_frame(range, true));
            }
        } else if modal.is_top_modal
            && !modal.any_popup_open
            && ctx.input(|i| i.key_pressed(Key::Escape))
        {
            outcome = Some(CalcOutcome::Cancel);
        }
        outcome
    }

    fn window(&mut self, ui: &mut Ui, rect: Rect, outcome: &mut Option<CalcOutcome>) {
        let (full, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
        let title = Rect::from_min_size(full.min, vec2(full.width(), TITLE_BAR));
        let top = Rect::from_min_size(
            pos2(full.left(), title.bottom() + 1.0),
            vec2(full.width(), TOP_ROW),
        );
        let bar = if self.field.is_some() {
            FIELD_BAR
        } else {
            STEP_BAR
        };
        let bottom = Rect::from_min_max(pos2(full.left(), full.bottom() - bar), full.max);
        let middle = Rect::from_min_max(
            pos2(full.left(), top.bottom() + 2.0),
            pos2(full.right(), bottom.top() - 1.0),
        );
        let painter = ui.painter().clone();
        painter.hline(
            full.x_range(),
            top.bottom() + 0.5,
            Stroke::new(1.0, TOP_RULE),
        );
        painter.hline(full.x_range(), bottom.top() - 0.5, Stroke::new(1.0, RULE));
        // The columns, left to right: fields, editor, operators, functions.
        let left = Rect::from_min_size(middle.min, vec2(FIELDS_PANE, middle.height()));
        let functions = self.show_functions.then(|| {
            Rect::from_min_max(
                pos2(middle.right() - FUNCTIONS_PANE, middle.top()),
                middle.max,
            )
        });
        let strip_right = functions.map_or(middle.right(), |f| f.left() - 1.0);
        let strip = Rect::from_min_max(
            pos2(strip_right - OPERATORS, middle.top()),
            pos2(strip_right, middle.bottom()),
        );
        let editor = Rect::from_min_max(
            pos2(left.right() + 1.0, middle.top()),
            pos2(strip.left() - 1.0, middle.bottom()),
        );
        for x in [left.right(), strip.left() - 1.0]
            .into_iter()
            .chain(functions.map(|f| f.left() - 1.0))
        {
            painter.vline(
                x + 0.5,
                middle.top() - 1.0..=middle.bottom(),
                Stroke::new(1.0, RULE),
            );
        }
        let schema = self.names.clone();
        if let Some(insertion) = self.fields.ui(
            ui,
            left,
            schema.as_deref(),
            self.context,
            self.field.is_some(),
        ) {
            self.insert(&insertion);
        }
        self.editor(ui, editor);
        painter.rect_filled(strip, 0.0, STRIP_BG);
        if let Some(insertion) = self.operator_strip(ui, strip) {
            self.insert(&insertion);
        }
        if let Some(rect) = functions {
            if let Some(insertion) = self.functions.ui(ui, rect, &self.function_list) {
                self.insert(&insertion);
            }
        }
        self.top_row(ui, top);
        self.bottom_bar(ui, bottom, outcome);
        self.title_bar(ui, title, full);
        self.resize_handles(ui, full);
        if let Some(row) = self.completion_list(ui, full) {
            self.choose(row, false);
        }
        if let Some(pending) = &mut self.pending {
            pending.ready = true;
        }
    }

    /// The title bar: the window buttons (only zoom acts, as on a modal
    /// FileMaker window), the centered title; dragging it moves the window.
    fn title_bar(&mut self, ui: &mut Ui, rect: Rect, full: Rect) {
        let drag = ui.interact(rect, ui.scope_id().with("calc_title"), Sense::drag());
        if let Some(offset) = self.drag_offset(ui, &drag, full) {
            let start = self.dragging.unwrap_or(full);
            self.rect = Some(start.translate(offset));
        }
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            CornerRadius {
                nw: 10,
                ne: 10,
                sw: 0,
                se: 0,
            },
            TITLE_BG,
        );
        painter.hline(
            rect.x_range(),
            rect.bottom() + 0.5,
            Stroke::new(1.0, TITLE_RULE),
        );
        let center = |n: f32| pos2(rect.left() + 14.0 + n * 20.0, rect.center().y);
        for n in 0..2 {
            painter.circle(
                center(n as f32),
                6.0,
                Color32::from_gray(0xD9),
                Stroke::new(0.5, Color32::from_gray(0xC4)),
            );
        }
        painter.circle(
            center(2.0),
            6.0,
            Color32::from_rgb(0x28, 0xC8, 0x40),
            Stroke::new(0.5, Color32::from_rgb(0x1F, 0xA8, 0x34)),
        );
        let zoom = ui.interact(
            Rect::from_center_size(center(2.0), vec2(14.0, 14.0)),
            ui.scope_id().with("calc_zoom"),
            Sense::click(),
        );
        zoom.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Zoom"));
        if zoom.clicked() {
            match self.unzoomed.take() {
                Some(previous) => self.rect = Some(previous),
                None => {
                    self.unzoomed = self.rect.or(Some(full));
                    self.rect = Some(ui.ctx().content_rect().shrink(4.0));
                }
            }
        }
        widgets::put_label(
            ui,
            Rect::from_center_size(rect.center(), vec2(rect.width() - 160.0, rect.height())),
            &self.title,
            FontId::new(theme::BODY, theme::semibold()),
            theme::TEXT,
            egui::Align::Center,
        );
    }

    /// The window's right and bottom edges and its corner resize it.
    fn resize_handles(&mut self, ui: &mut Ui, full: Rect) {
        let edges = [
            (
                "calc_resize_right",
                Rect::from_min_max(
                    pos2(full.right() - 4.0, full.top() + 10.0),
                    pos2(full.right() + 3.0, full.bottom() - 10.0),
                ),
                vec2(1.0, 0.0),
                CursorIcon::ResizeHorizontal,
            ),
            (
                "calc_resize_bottom",
                Rect::from_min_max(
                    pos2(full.left() + 10.0, full.bottom() - 4.0),
                    pos2(full.right() - 10.0, full.bottom() + 3.0),
                ),
                vec2(0.0, 1.0),
                CursorIcon::ResizeVertical,
            ),
            (
                "calc_resize_corner",
                Rect::from_min_max(full.max - vec2(12.0, 12.0), full.max + vec2(3.0, 3.0)),
                vec2(1.0, 1.0),
                CursorIcon::ResizeNwSe,
            ),
        ];
        for (salt, rect, axes, cursor) in edges {
            let response = ui.interact(rect, ui.scope_id().with(salt), Sense::drag());
            let response = response.on_hover_cursor(cursor);
            if let Some(offset) = self.drag_offset(ui, &response, full) {
                let start = self.dragging.unwrap_or(full);
                let size = (start.size() + offset * axes).max(MIN_SIZE);
                self.rect = Some(Rect::from_min_size(start.min, size));
            }
        }
    }

    /// While `response` is dragged, how far the pointer has come since the
    /// drag began; the window's rect then is kept in `dragging`.
    fn drag_offset(&mut self, ui: &Ui, response: &Response, full: Rect) -> Option<Vec2> {
        if response.drag_started() {
            self.dragging = self.rect.or(Some(full));
            self.unzoomed = None;
        }
        if response.drag_stopped() {
            self.dragging = None;
        }
        if !response.dragged() {
            return None;
        }
        let (origin, at) = ui.input(|i| (i.pointer.press_origin(), i.pointer.interact_pos()));
        Some(at? - origin?)
    }

    /// The context popup, the cursor's position, Help and the functions
    /// pane toggle (42).
    fn top_row(&mut self, ui: &mut Ui, rect: Rect) {
        let label = "Evaluate this calculation from the context of:";
        let font = FontId::proportional(BODY_TEXT);
        let label_width = widgets::text_width(ui, label, font.clone());
        widgets::put_label(
            ui,
            Rect::from_min_size(
                pos2(rect.left() + 13.0, rect.top()),
                vec2(label_width + 4.0, rect.height()),
            ),
            label,
            font.clone(),
            theme::TEXT,
            egui::Align::Min,
        );
        let schema = self.names.clone();
        let choices: Vec<(u64, String)> = schema
            .as_deref()
            .map(|s| {
                let table = self
                    .field
                    .as_ref()
                    .and(self.context)
                    .and_then(|c| s.occurrence(c))
                    .map(|o| o.table);
                s.occurrences_in_order()
                    .into_iter()
                    .filter(|o| table.is_none_or(|t| o.table == t))
                    .map(|o| (o.id, o.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let current = self
            .context
            .and_then(|c| choices.iter().find(|(id, _)| *id == c))
            .map_or_else(String::new, |(_, name)| name.clone());
        let popup = Rect::from_min_max(
            pos2(rect.left() + 256.0, rect.top() + 7.0),
            pos2(rect.right() - 380.0, rect.top() + 25.0),
        );
        let response = popup_button(
            ui,
            popup,
            "Evaluate this calculation from the context of",
            &current,
            choices.len() > 1,
        );
        if choices.len() > 1 {
            egui::Popup::menu(&response)
                .style(theme::menu_popup_style)
                .show(|ui| {
                    for (id, name) in &choices {
                        let on = self.context == Some(*id);
                        if widgets::menu_item(ui, name, None, on, true).clicked() {
                            self.context = Some(*id);
                            self.fields.occurrence = None;
                        }
                    }
                });
        }
        // "L: 1 C: 1": where the cursor (the selection's start) is.
        let (line, column) = syntax::line_and_column(&self.text, self.selection.start);
        widgets::put_label(
            ui,
            Rect::from_min_max(
                pos2(rect.right() - 200.0, rect.top() + 1.0),
                pos2(rect.right() - 82.0, rect.bottom()),
            ),
            &format!("L: {line} C: {column}"),
            FontId::proportional(theme::BODY),
            theme::TEXT,
            egui::Align::Max,
        );
        // Help: a white disc with a question mark.
        let help = Rect::from_center_size(
            pos2(rect.right() - 58.0, rect.top() + 15.5),
            vec2(22.0, 22.0),
        );
        let response = ui.interact(help, ui.scope_id().with("calc_help"), Sense::click());
        let painter = ui.painter();
        painter.circle_filled(
            help.center() + vec2(0.0, 1.0),
            11.0,
            Color32::from_gray(0xE6),
        );
        painter.circle_filled(
            help.center() + vec2(0.0, 0.5),
            11.0,
            Color32::from_gray(0xBA),
        );
        painter.circle(
            help.center(),
            10.75,
            if response.is_pointer_button_down_on() {
                Color32::from_gray(0xE4)
            } else {
                Color32::WHITE
            },
            Stroke::new(0.75, Color32::from_gray(0xDD)),
        );
        painter.text(
            help.center() + vec2(0.0, 0.5),
            Align2::CENTER_CENTER,
            "?",
            FontId::new(14.0, theme::semibold()),
            Color32::from_gray(0x26),
        );
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Help"));
        if response.clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(HELP_URL));
        }
        // Show or hide the functions pane.
        let toggle = Rect::from_min_size(
            pos2(rect.right() - 33.0, rect.top() + 6.0),
            vec2(28.0, 20.0),
        );
        let response = ui.interact(toggle, ui.scope_id().with("calc_pane"), Sense::click());
        let painter = ui.painter();
        painter.rect(
            toggle,
            CornerRadius::same(5),
            if response.is_pointer_button_down_on() {
                Color32::from_gray(0xDC)
            } else {
                theme::DIALOG_BG
            },
            Stroke::new(1.0, Color32::from_gray(0xE4)),
            StrokeKind::Inside,
        );
        let color = if self.show_functions {
            PANE_BLUE
        } else {
            Color32::from_gray(0x6A)
        };
        let o = toggle.min;
        painter.rect_stroke(
            Rect::from_min_max(o + vec2(6.5, 3.5), o + vec2(22.5, 16.5)),
            CornerRadius::ZERO,
            Stroke::new(1.0, color),
            StrokeKind::Middle,
        );
        painter.rect_filled(
            Rect::from_min_max(o + vec2(19.0, 5.0), o + vec2(21.0, 16.0)),
            CornerRadius::ZERO,
            color,
        );
        response.widget_info(|| {
            WidgetInfo::selected(
                Role::CheckBox,
                true,
                self.show_functions,
                "Show Functions Pane",
            )
        });
        if response.clicked() {
            self.show_functions = !self.show_functions;
        }
    }

    /// The formula: a colored, monospaced multiline editor.
    fn editor(&mut self, ui: &mut Ui, rect: Rect) {
        let id = editor_id();
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.opened_now) {
            // A fresh editor: no undo history from another formula, the
            // cursor at the start (42: L: 1 C: 1).
            egui::text_edit::TextEditState::default().store(&ctx, id);
            self.pending = Some(Pending::next_frame(0..0, true));
        }
        if let Some(pending) = self.pending.take_if(|p| p.ready) {
            let count = self.text.chars().count();
            let selection = pending.selection.start.min(count)..pending.selection.end.min(count);
            let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(selection.start),
                CCursor::new(selection.end),
            )));
            state.store(&ctx, id);
            if pending.focus {
                ctx.memory_mut(|m| m.request_focus(id));
            }
            self.selection = selection;
        }
        if self.alert.is_some() {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
        let schema = self.names.clone();
        let names: &dyn Names = match schema.as_deref() {
            Some(s) => s.names(),
            None => &NoNames,
        };
        let comma = schema
            .as_deref()
            .is_some_and(|s| s.locale.decimal_separator == ',');
        let font = FontId::monospace(EDITOR_FONT);
        let mut layouter = |ui: &Ui, text: &dyn TextBuffer, wrap_width: f32| {
            let mut job = syntax::layout_job(text.as_str(), &font, LINE, comma, names);
            job.wrap.max_width = wrap_width;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let output = egui::ScrollArea::vertical()
            .id_salt("calculation_editor")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                egui::TextEdit::multiline(&mut self.text)
                    .id(id)
                    .font(font.clone())
                    .frame(egui::Frame::new().inner_margin(Margin {
                        left: 5,
                        right: 5,
                        top: 1,
                        bottom: 8,
                    }))
                    .desired_width(f32::INFINITY)
                    .min_size(vec2(rect.width(), rect.height()))
                    .lock_focus(true)
                    .layouter(&mut layouter)
                    .show(ui)
            })
            .inner;
        widgets::accessible_label(ui, output.response.response.id, "Calculation");
        let before = self.selection.clone();
        if let Some(range) = output.cursor_range {
            let range = range.as_sorted_char_range();
            self.selection = range.start.0..range.end.0;
        }
        // Typing opens or refines the list; moving the cursor, or the
        // focus going anywhere but the list, closes it.
        let on_list = self
            .completion
            .as_ref()
            .and_then(|l| l.rect)
            .is_some_and(|r| ui.input(|i| i.pointer.interact_pos().is_some_and(|p| r.contains(p))));
        let typed = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_))));
        if !ctx.memory(|m| m.has_focus(id)) {
            if !on_list {
                self.completion = None;
            }
        } else if output.response.response.changed() {
            if typed || self.completion.is_some() {
                self.complete();
            }
        } else if self.selection != before {
            self.completion = None;
        }
        if let Some(list) = &mut self.completion {
            let start = CCursor::new(list.completion.replace.start);
            list.anchor = Some(
                output
                    .galley
                    .pos_from_cursor(start)
                    .translate(output.galley_pos.to_vec2()),
            );
        }
        // Egui handles keyboard focus before the next frame reaches the
        // dialog. Keep Escape while the list is open, including when typing
        // opened it in this frame, so closing it leaves the editor focused.
        let keys = egui::TextEdit::multiline(&mut String::new())
            .lock_focus(true)
            .get_event_filter();
        ctx.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    escape: self.completion.is_some(),
                    ..keys
                },
            );
        });
    }

    /// The completion list (OpenMaker's design): up to ten rows under the
    /// typed name, or over it near the window's bottom, each the name as
    /// it is written and its kind. A clicked row to choose.
    fn completion_list(&mut self, ui: &mut Ui, window: Rect) -> Option<usize> {
        const ROW: f32 = 18.0;
        const ROWS: usize = 10;
        let list = self.completion.as_mut()?;
        let anchor = list.anchor?;
        let font = FontId::proportional(LIST_FONT);
        let candidates = &list.completion.candidates;
        let widest = candidates
            .iter()
            .take(200)
            .map(|c| {
                widgets::text_width(ui, &c.label, font.clone())
                    + widgets::text_width(ui, kind_label(c), font.clone())
            })
            .fold(0.0, f32::max);
        let rows = candidates.len().min(ROWS);
        let size = vec2((widest + 44.0).clamp(220.0, 460.0), rows as f32 * ROW + 8.0);
        let mut rect = Rect::from_min_size(pos2(anchor.left() - 6.0, anchor.bottom() + 3.0), size);
        if rect.bottom() > window.bottom() - 6.0 {
            rect = rect.translate(vec2(0.0, -(size.y + anchor.height() + 6.0)));
        }
        let rect = rect.translate(vec2((window.right() - 6.0 - rect.right()).min(0.0), 0.0));
        let rect = rect.translate(vec2((window.left() + 6.0 - rect.left()).max(0.0), 0.0));
        list.rect = Some(rect);
        let painter = ui.painter();
        painter.add(
            egui::Shadow {
                offset: [0, 3],
                blur: 10,
                spread: 0,
                color: Color32::from_black_alpha(40),
            }
            .as_shape(rect, CornerRadius::same(6)),
        );
        painter.rect(
            rect,
            CornerRadius::same(6),
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_gray(0xC8)),
            StrokeKind::Inside,
        );
        let inner = rect.shrink2(vec2(0.0, 4.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        child.set_clip_rect(inner);
        let mut chosen = None;
        let reveal = std::mem::take(&mut list.reveal);
        let highlighted = list.row;
        egui::ScrollArea::vertical()
            .id_salt("calculation_completion")
            .auto_shrink([false, false])
            .max_height(inner.height())
            .show_rows(&mut child, ROW, candidates.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for n in range {
                    let candidate = &candidates[n];
                    let (row, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    let selected = n == highlighted;
                    let (text, hint) = if selected {
                        ui.painter()
                            .rect_filled(row.shrink2(vec2(4.0, 0.0)), 4.0, theme::SELECTED);
                        (Color32::WHITE, Color32::WHITE)
                    } else {
                        (theme::TEXT, Color32::from_gray(0x8A))
                    };
                    let kind = kind_label(candidate);
                    let kind_width = widgets::text_width(ui, kind, font.clone());
                    let galley = ui.ctx().fonts_mut(|fonts| {
                        let mut job = egui::text::LayoutJob::simple_singleline(
                            candidate.label.clone(),
                            font.clone(),
                            text,
                        );
                        job.wrap = egui::text::TextWrapping::truncate_at_width(
                            row.width() - kind_width - 28.0,
                        );
                        fonts.layout_job(job)
                    });
                    ui.painter().galley(
                        pos2(row.left() + 10.0, row.center().y - galley.size().y / 2.0),
                        galley,
                        text,
                    );
                    ui.painter().text(
                        pos2(row.right() - 10.0, row.center().y),
                        Align2::RIGHT_CENTER,
                        kind,
                        font.clone(),
                        hint,
                    );
                    response.widget_info(|| {
                        WidgetInfo::selected(Role::ListItem, true, selected, &candidate.label)
                    });
                    if selected && reveal {
                        response.scroll_to_me(None);
                    }
                    if response.clicked() {
                        chosen = Some(n);
                    }
                }
            });
        chosen
    }

    /// The operators between the editor and the functions (42): four
    /// groups, 15 pt apart, 12 pt more between groups.
    fn operator_strip(&mut self, ui: &mut Ui, rect: Rect) -> Option<Insertion> {
        let mut insert = None;
        let mut y = rect.top() + 4.0;
        for (group, operators) in syntax::OPERATORS.iter().enumerate() {
            if group > 0 {
                y += 12.0;
            }
            for op in operators.iter() {
                let row = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), 15.0));
                y += 15.0;
                let response =
                    ui.interact(row, ui.scope_id().with(("operator", *op)), Sense::click());
                let color = if response.is_pointer_button_down_on() {
                    Color32::BLACK
                } else if response.hovered() {
                    theme::TEXT
                } else {
                    OPERATOR_TEXT
                };
                ui.painter().text(
                    row.center(),
                    Align2::CENTER_CENTER,
                    *op,
                    FontId::proportional(theme::SMALL),
                    color,
                );
                response.widget_info(|| WidgetInfo::labeled(Role::Button, true, *op));
                if response.clicked() {
                    insert = Some(syntax::operator(op, &self.text, self.selection.clone()));
                }
            }
        }
        insert
    }

    /// The field options where they apply (42), then Cancel and OK.
    fn bottom_bar(&mut self, ui: &mut Ui, rect: Rect, outcome: &mut Option<CalcOutcome>) {
        let font = FontId::proportional(BODY_TEXT);
        if let Some(field) = &mut self.field {
            widgets::put_label(
                ui,
                Rect::from_min_size(
                    pos2(rect.left() + 13.0, rect.top() + 12.0),
                    vec2(126.0, 18.0),
                ),
                "Calculation result is",
                font.clone(),
                theme::TEXT,
                egui::Align::Min,
            );
            let types = [
                (ValueType::Text, "Text"),
                (ValueType::Number, "Number"),
                (ValueType::Date, "Date"),
                (ValueType::Time, "Time"),
                (ValueType::Timestamp, "Timestamp"),
                (ValueType::Container, "Container"),
            ];
            let name = types
                .iter()
                .find(|(t, _)| *t == field.result)
                .map_or("Text", |(_, n)| n);
            let popup = Rect::from_min_size(
                pos2(rect.left() + 144.0, rect.top() + 11.0),
                vec2(101.0, 18.0),
            );
            let own = field.option.is_none();
            let response = popup_button(ui, popup, "Calculation result is", name, own);
            egui::Popup::menu(&response)
                .style(theme::menu_popup_style)
                .show(|ui| {
                    for (value_type, label) in types {
                        let on = field.result == value_type;
                        if widgets::menu_item(ui, label, None, on, true).clicked() {
                            field.result = value_type;
                        }
                    }
                });
            let storage_button = push_button(
                ui,
                Rect::from_min_size(
                    pos2(rect.left() + 282.0, rect.top() + 11.0),
                    vec2(113.0, 17.0),
                ),
                "Storage Options…",
                false,
                field.storage.is_some(),
            );
            if storage_button.clicked() {
                if let Some(chosen) = field.storage.clone() {
                    let name = self
                        .title
                        .strip_prefix("Specify Calculation “")
                        .and_then(|t| t.strip_suffix('”'))
                        .unwrap_or_default();
                    self.storage = Some(storage::StorageOptions::new(name, chosen, field.result));
                }
            }
            widgets::put_label(
                ui,
                Rect::from_min_size(
                    pos2(rect.left() + 13.0, rect.top() + 37.0),
                    vec2(126.0, 20.0),
                ),
                "Number of repetitions:",
                font.clone(),
                theme::TEXT,
                egui::Align::Min,
            );
            // 42: a 47 x 22 pt field, the text 7 pt in.
            let field_rect = Rect::from_min_size(
                pos2(rect.left() + 142.0, rect.top() + 35.0),
                vec2(47.0, 22.0),
            );
            bezel(ui.painter(), field_rect, 0, Color32::WHITE);
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(field_rect.shrink2(vec2(6.0, 1.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let edit = child.add_enabled(
                own,
                egui::TextEdit::singleline(&mut self.repetitions)
                    .font(font.clone())
                    .frame(egui::Frame::new())
                    .desired_width(field_rect.width() - 12.0),
            );
            widgets::accessible_label(ui, edit.id, "Number of repetitions");
            // A message calculation has neither option [I: FileMaker Pro's
            // Specify Calculation for it was not captured].
            if field.option != Some(OptionKind::Message) {
                let check = Rect::from_min_size(
                    pos2(rect.left() + 223.0, rect.top() + 40.0),
                    vec2(13.0, 13.0),
                );
                let label = if field.option == Some(OptionKind::Validation) {
                    "Validate only if field has been modified"
                } else {
                    "Do not evaluate if all referenced fields are empty"
                };
                let row = Rect::from_min_max(
                    check.min,
                    pos2(
                        check.right() + 3.0 + widgets::text_width(ui, label, font.clone()),
                        check.bottom(),
                    ),
                );
                let response =
                    ui.interact(row, ui.scope_id().with("calc_skip_empty"), Sense::click());
                if response.clicked() {
                    field.skip_when_empty = !field.skip_when_empty;
                }
                checkbox(ui.painter(), check, field.skip_when_empty);
                ui.painter().text(
                    pos2(check.right() + 3.0, check.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    font.clone(),
                    theme::TEXT,
                );
                let on = field.skip_when_empty;
                response.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, on, label));
            }
        }
        self.dialog_buttons(ui, rect, outcome);
    }

    /// Cancel and OK at the bottom right.
    fn dialog_buttons(&mut self, ui: &mut Ui, rect: Rect, outcome: &mut Option<CalcOutcome>) {
        // 42: 82 x 22 pt bezels 9 apart, 21 pt from the right and 19 from
        // the bottom.
        let ok = Rect::from_min_size(
            pos2(rect.right() - 103.0, rect.bottom() - 41.0),
            vec2(82.0, 22.0),
        );
        let cancel = ok.translate(vec2(-91.0, 0.0));
        if push_button(ui, cancel, "Cancel", false, true).clicked() {
            *outcome = Some(CalcOutcome::Cancel);
        }
        if push_button(ui, ok, "OK", true, true).clicked() {
            match self.accept() {
                Ok(calc) => *outcome = Some(CalcOutcome::Ok(calc)),
                Err(error) => {
                    self.pending = Some(Pending {
                        selection: error.range.clone(),
                        focus: false,
                        ready: true,
                    });
                    self.alert = Some((error.message, error.range));
                    ui.ctx().memory_mut(|m| m.surrender_focus(editor_id()));
                }
            }
        }
    }
}

/// What the completion list says a candidate is: a function's category,
/// `Field`, `Get` for a selector.
fn kind_label(candidate: &Candidate) -> &'static str {
    match candidate.kind {
        CandidateKind::Field => "Field",
        CandidateKind::Selector => "Get",
        _ => candidate.category.unwrap_or("Function"),
    }
}

/// Keep the window on screen, no larger than it.
fn fit(rect: Rect, screen: Rect) -> Rect {
    let size = rect
        .size()
        .min(screen.size())
        .max(MIN_SIZE.min(screen.size()));
    let min = rect
        .min
        .clamp(screen.min, (screen.max - size).max(screen.min))
        .round();
    Rect::from_min_size(min, size.round())
}

/// A dialog push button of 42's size (`rect` is its 22 pt bezel): white
/// with a light top edge, a darker bottom edge and a soft shadow, or the
/// default button's blue gradient.
fn push_button(ui: &mut Ui, rect: Rect, label: &str, default: bool, enabled: bool) -> Response {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("calc_button", label)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = response.is_pointer_button_down_on();
    let painter = ui.painter();
    let text = if default && enabled {
        painter.rect_filled(
            rect.translate(vec2(0.0, 1.0)),
            CornerRadius::same(5),
            Color32::from_rgb(0xC8, 0xCC, 0xE8),
        );
        let (top, bottom) = if pressed {
            (
                BLUE_TOP.gamma_multiply(0.85),
                BLUE_BOTTOM.gamma_multiply(0.85),
            )
        } else {
            (BLUE_TOP, BLUE_BOTTOM)
        };
        gradient(painter, rect.shrink2(vec2(0.0, 1.0)), 5, top, bottom);
        Color32::WHITE
    } else {
        bezel(
            painter,
            rect,
            5,
            if pressed {
                Color32::from_gray(0xE2)
            } else {
                Color32::WHITE
            },
        );
        if enabled {
            theme::TEXT
        } else {
            theme::DISABLED
        }
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(if rect.height() < 20.0 {
            BODY_TEXT
        } else {
            theme::BODY
        }),
        text,
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    response
}

/// macOS's raised control face: a light top and side edge, a darker
/// bottom edge and a soft shadow under it (42's buttons, popups and search
/// fields).
fn bezel(painter: &Painter, rect: Rect, radius: u8, fill: Color32) {
    painter.rect_filled(
        rect.translate(vec2(0.0, 1.0)),
        CornerRadius::same(radius),
        Color32::from_gray(0xE8),
    );
    painter.rect_filled(rect, CornerRadius::same(radius), Color32::from_gray(0xB7));
    painter.rect_filled(
        Rect::from_min_max(rect.min, rect.max - vec2(0.0, 1.0)),
        CornerRadius::same(radius),
        Color32::from_gray(0xDD),
    );
    painter.rect_filled(
        Rect::from_min_max(rect.min + vec2(1.0, 1.0), rect.max - vec2(1.0, 1.0)),
        CornerRadius::same(radius.saturating_sub(1)),
        fill,
    );
}

/// A rounded rect filled with a vertical gradient.
fn gradient(painter: &Painter, rect: Rect, radius: u8, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    let mut tessellator = egui::epaint::Tessellator::new(
        painter.pixels_per_point(),
        egui::epaint::TessellationOptions::default(),
        [1, 1],
        Vec::new(),
    );
    tessellator.tessellate_rect(
        &egui::epaint::RectShape::filled(rect, CornerRadius::same(radius), Color32::WHITE),
        &mut mesh,
    );
    for vertex in &mut mesh.vertices {
        let t = ((vertex.pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        let coverage = f32::from(vertex.color.a()) / 255.0;
        vertex.color = top.lerp_to_gamma(bottom, t).gamma_multiply(coverage);
    }
    painter.add(mesh);
}

/// A macOS popup button (42): a white bezel with the value at the left and
/// a blue stepper at the right; pale and gray when it offers no choice.
pub(super) fn popup_button(
    ui: &mut Ui,
    rect: Rect,
    label: &str,
    text: &str,
    enabled: bool,
) -> Response {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("calc_popup", label)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let painter = ui.painter();
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    if enabled {
        bezel(
            painter,
            rect,
            5,
            if open || response.is_pointer_button_down_on() {
                Color32::from_gray(0xF0)
            } else {
                Color32::WHITE
            },
        );
    } else {
        painter.rect(
            rect,
            CornerRadius::same(5),
            Color32::from_gray(0xF8),
            Stroke::new(1.0, Color32::from_gray(0xE5)),
            StrokeKind::Inside,
        );
    }
    let color = if enabled {
        theme::TEXT
    } else {
        Color32::from_gray(0xA8)
    };
    let stepper = Rect::from_min_size(
        pos2(rect.right() - 15.0, rect.top() + 3.0),
        vec2(12.0, 12.0),
    );
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 7.0, rect.top()),
        pos2(stepper.left() - 4.0, rect.bottom()),
    );
    let galley = ui.ctx().fonts_mut(|fonts| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            text.to_owned(),
            FontId::proportional(BODY_TEXT),
            color,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width());
        fonts.layout_job(job)
    });
    painter.galley(
        pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    let arrows = if enabled {
        gradient(painter, stepper, 3, BLUE_TOP, BLUE_BOTTOM);
        Color32::WHITE
    } else {
        Color32::from_gray(0xB4)
    };
    let c = stepper.center();
    let stroke = Stroke::new(1.25, arrows);
    painter.line(
        vec![
            c + vec2(-2.5, -1.5),
            c + vec2(0.0, -4.0),
            c + vec2(2.5, -1.5),
        ],
        stroke,
    );
    painter.line(
        vec![c + vec2(-2.5, 1.5), c + vec2(0.0, 4.0), c + vec2(2.5, 1.5)],
        stroke,
    );
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::ComboBox, enabled, label);
        info.current_text_value = Some(text.to_owned());
        info
    });
    response
}

/// A search field in `rect` (42: white, rounded, a magnifier and
/// "Search").
pub(super) fn search_box(ui: &mut Ui, rect: Rect, text: &mut String, label: &str) {
    let painter = ui.painter();
    bezel(painter, rect, 5, Color32::WHITE);
    crate::icons::paint(
        painter,
        Rect::from_center_size(pos2(rect.left() + 13.5, rect.center().y), vec2(14.0, 14.0)),
        crate::icons::Icon::Search,
        Color32::from_gray(0x3C),
        Color32::WHITE,
    );
    let field = Rect::from_min_max(
        pos2(rect.left() + 25.0, rect.top()),
        pos2(rect.right() - 4.0, rect.bottom()),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(field)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let edit = child.add(
        egui::TextEdit::singleline(text)
            .hint_text(
                egui::RichText::new("Search")
                    .size(BODY_TEXT)
                    .color(Color32::from_gray(0xBE)),
            )
            .font(FontId::proportional(BODY_TEXT))
            .frame(egui::Frame::new())
            .desired_width(field.width()),
    );
    widgets::accessible_label(ui, edit.id, label);
}

/// The sort and view button of the lists (42): three rising bars, 3 pt
/// wide and 5 apart, centered in `rect`.
pub(super) fn bar_chart(painter: &Painter, rect: Rect, color: Color32) {
    let left = (rect.center().x - 6.5).round();
    let bottom = (rect.center().y + 6.5).round();
    for (n, height) in [7.0, 10.0, 13.0].into_iter().enumerate() {
        let x = left + n as f32 * 5.0;
        painter.rect_filled(
            Rect::from_min_max(pos2(x, bottom - height), pos2(x + 3.0, bottom)),
            0.0,
            color,
        );
    }
}

/// The Description panel's help mark.
pub(super) fn help_glyph(painter: &Painter, rect: Rect, color: Color32) {
    glyphs::paint(painter, rect, Glyph::Help, color);
}

/// A checked or unchecked box (42: blue with a white check).
fn checkbox(painter: &Painter, rect: Rect, checked: bool) {
    if checked {
        gradient(painter, rect, 3, BLUE_TOP, BLUE_BOTTOM);
        let c = rect.center();
        painter.line(
            vec![
                c + vec2(-3.5, 0.0),
                c + vec2(-1.0, 2.8),
                c + vec2(3.5, -2.8),
            ],
            Stroke::new(1.6, Color32::WHITE),
        );
    } else {
        painter.rect(
            rect,
            CornerRadius::same(3),
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_gray(0xB4)),
            StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::calc::Locale;
    use om_data::host::FieldDef;

    fn schema() -> Arc<CalcSchema> {
        let mut schema = CalcSchema::new("Contacts", Locale::en_us());
        schema.add_table(0x8002, "Contacts");
        schema.add_field(0x8002, FieldDef::data(1, "First Name", ValueType::Text));
        schema.add_field(0x8002, FieldDef::data(2, "Last Name", ValueType::Text));
        schema.add_occurrence(0xd00002, "Contacts", 0x8002);
        schema.add_occurrence(0xd00003, "Contacts 2", 0x8002);
        Arc::new(schema)
    }

    /// Compile as FileMaker would have stored the formula, open it in the
    /// dialog, OK it unchanged.
    fn reopened(source: &str, occurrence: Option<u64>) -> (Calculation, Calculation, String) {
        let names = schema();
        let stored = compile_in(source, Some(&names), occurrence, false).unwrap();
        let dialog = SpecifyCalculation::new("t".into(), Some(&stored), Some(names));
        let text = dialog.text.clone();
        (stored, dialog.accept().unwrap(), text)
    }

    #[test]
    fn a_formula_opened_and_okayed_unchanged_keeps_its_streams() {
        for source in [
            "Get ( ScriptParameter )",
            "Case (\r\tnot IsEmpty ( Contacts::Last Name ) ;\r\tLeft ( Contacts::Last Name ; 1 ) ;\r\t\"?\" // no name\r)",
            "/* the initial */ Left ( Contacts::First Name ; 1 ) & \"a\"\"b\" & ¶ & 1.50 // done",
            "Let ( [ x = 1e3 ; $y = \"\\\\server\" ] ; x & $y )  ",
        ] {
            let (stored, okayed, text) = reopened(source, None);
            assert_eq!(okayed, stored, "{source}");
            assert!(!text.contains('\r'), "{text:?}");
        }
        // Unqualified fields compile against the occurrence they were
        // stored with, which the context popup opens on.
        let (stored, okayed, text) = reopened("First Name & \" \" & Last Name", Some(0xd00003));
        assert_eq!(text, "First Name & \" \" & Last Name");
        assert_eq!(okayed, stored);
    }

    #[test]
    fn edited_text_compiles_with_returns_as_filemaker_stores_them() {
        let names = schema();
        let mut dialog = SpecifyCalculation::new("t".into(), None, Some(names.clone()));
        dialog.text = "Abs (\n  -1\n)".into();
        let calc = dialog.accept().unwrap();
        assert_eq!(
            calc,
            compile_in("Abs (\r  -1\r)", Some(&names), None, false).unwrap()
        );
        assert_eq!(calc.context.as_deref(), Some(&Calculation::NO_CONTEXT[..]));
        // The context popup reads unqualified names.
        dialog.text = "First Name".into();
        dialog.context = Some(0xd00003);
        let calc = dialog.accept().unwrap();
        let reference =
            om_data::formula::formatted::decode(calc.formatted.as_deref().unwrap()).unwrap();
        assert_eq!(reference, [Token::Field(Field::new(0xd00003, 1, false))]);
    }

    #[test]
    fn errors_carry_filemakers_message_and_the_character_range_to_select() {
        let names = schema();
        let error = compile_in("\"é\" & Abs ( 1 ; 2 )", Some(&names), None, false).unwrap_err();
        assert_eq!(error.message, "Too many parameters in the function");
        assert_eq!(error.range, 14..15);
        let error = compile_in("Nobody", Some(&names), Some(0xd00002), false).unwrap_err();
        assert_eq!(error.message, "Field is missing");
        assert_eq!(error.range, 0..6);
        assert_eq!(
            compile("1 +", None).unwrap_err(),
            "Number, text constant, field name, or \"(\" expected"
        );
    }

    #[test]
    fn the_context_is_the_stored_unqualified_fields_or_the_first_occurrence() {
        let names = schema();
        let unqualified = compile_in("Last Name", Some(&names), Some(0xd00003), false).unwrap();
        assert_eq!(
            initial_context(Some(&unqualified), Some(&names)),
            Some(0xd00003)
        );
        let qualified = compile_in("Contacts 2::Last Name", Some(&names), None, false).unwrap();
        assert_eq!(
            initial_context(Some(&qualified), Some(&names)),
            Some(0xd00002)
        );
        assert_eq!(initial_context(None, None), None);
    }

    #[test]
    fn field_calculations_store_their_context_and_options() {
        let names = schema();
        let options = FieldOptions {
            result: ValueType::Text,
            repetitions: 1,
            skip_when_empty: true,
            storage: None,
            option: None,
        };
        let mut dialog = SpecifyCalculation::new("t".into(), None, Some(names))
            .for_field(0xd00002, options.clone());
        dialog.text = "Left ( First Name ; 1 )".into();
        let calc = dialog.accept().unwrap();
        assert_eq!(calc.context, Some(vec![3, 0xd0, 0, 2]));
        dialog.repetitions = "3".into();
        assert_eq!(
            dialog.field_options(),
            Some(FieldOptions {
                repetitions: 3,
                ..options
            })
        );
        // Self is allowed in field calculations only.
        dialog.text = "Self".into();
        assert!(dialog.accept().is_ok());
        assert!(compile("Self", None).is_err());
    }

    #[test]
    fn insertions_replace_the_selection_and_select_what_follows() {
        let mut dialog = SpecifyCalculation::new("t".into(), None, Some(schema()));
        dialog.text = "1 + x".into();
        dialog.selection = 4..5;
        dialog.insert(&functions::catalog()[0].insertion());
        assert_eq!(dialog.text, "1 + Abs ( number )");
        assert_eq!(dialog.pending.as_ref().unwrap().selection, 10..16);
        dialog.selection = 10..16;
        let reference = fields::reference(&schema(), 0xd00002, 1, true);
        dialog.insert(&Insertion::caret_after(reference));
        assert_eq!(dialog.text, "1 + Abs ( Contacts::First Name )");
        assert!(dialog.accept().is_ok());
        // A chosen completion replaces the typed name only.
        dialog.text = "Le & x".into();
        dialog.selection = 2..2;
        dialog.complete();
        let listed = &dialog.completion.as_ref().unwrap().completion.candidates;
        let left = listed.iter().position(|c| c.name == "Left").unwrap();
        dialog.choose(left, false);
        assert_eq!(dialog.text, "Left ( text ; numberOfCharacters ) & x");
        assert_eq!(dialog.pending.as_ref().unwrap().selection, 7..11);
        assert!(dialog.completion.is_none());
    }
}
