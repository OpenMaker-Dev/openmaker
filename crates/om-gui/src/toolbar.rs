//! The status toolbar: the 54 pt band under the title with the
//! record/request/page navigation group at the left and the mode's command
//! buttons (glyph above caption) to its right.
//!
//! Positions follow FileMaker's Browse toolbar in capture 27 (x from the
//! window's left edge): chevrons at 27 and 57, the number box at 78-149, the
//! found-set ring at 169, then Show All at 335, New Record 418, Delete Record
//! 502, Find 582, Sort 653, Share 705 and Quick Find from 745.

use egui::{pos2, vec2, Align2, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui};

use om_data::scripts::save_records::Format as SaveFormat;

use crate::app::{Action, FindState, LayoutTool, Mode};
use crate::icons::{self, Icon};
use crate::session::Session;
use crate::theme;
use crate::widgets::{self, toolbar_button, TOOLBAR_ICON_Y};

/// Mutable toolbar state owned by the app.
#[derive(Default)]
pub(crate) struct ToolbarState {
    /// The Quick Find text.
    pub quick_find: String,
    /// The navigation number box while it is being edited.
    pub nav_text: String,
}

/// The left navigation group's model: a position within some count of
/// records, requests or pages.
struct Nav {
    /// Zero-based position.
    current: u64,
    /// How many there are, once known.
    count: Option<u64>,
    /// The second line of the count text.
    detail: &'static str,
    caption: &'static str,
    /// The accessible name of the number box.
    number_label: &'static str,
    /// Records are loading; the ring shows activity.
    loading: bool,
}

/// Everything the toolbar reads.
pub(crate) struct ToolbarContext<'a> {
    pub mode: Mode,
    pub session: &'a Session,
    pub find: &'a FindState,
    pub tool: LayoutTool,
    pub inspector: bool,
    pub objects: bool,
    pub record_create: bool,
    pub record_delete: bool,
    /// The Save Records as commands that can run: the Share menu's and the
    /// Preview toolbar's Save as Excel and Save as PDF.
    pub save_records: &'a [SaveFormat],
    /// The account may print (Print, Page Setup; session-identity.md 2.3).
    pub print: bool,
    /// The toolbar can be used: not under Minimum menus (2.3), which leave
    /// none of its commands.
    pub usable: bool,
    /// Its record commands (New and Delete Record, Find, Sort, Quick Find)
    /// are available: all menu commands; Editing only leaves them out and
    /// keeps record navigation and Show All (FileMaker Pro 26.0.3, #137's
    /// reading).
    pub commands: bool,
    /// The found set after a find, omit or sort.
    pub found_set: Option<om_data::found::FoundSet>,
}

impl ToolbarContext<'_> {
    /// A find or omit left part of the table out.
    fn partial(&self) -> bool {
        self.found_set.is_some_and(|f| f.found < f.total)
    }

    /// The found-set line: Total or Found, Sorted or Unsorted.
    fn detail(&self) -> &'static str {
        let sorted = self.found_set.is_some_and(|f| f.sorted);
        match (self.partial(), sorted) {
            (false, false) => "Total (Unsorted)",
            (false, true) => "Total (Sorted)",
            (true, false) => "Found (Unsorted)",
            (true, true) => "Found (Sorted)",
        }
    }
}

pub(crate) fn show(
    ui: &mut Ui,
    context: &ToolbarContext<'_>,
    state: &mut ToolbarState,
    actions: &mut Vec<Action>,
) {
    let frame = egui::Frame::new().fill(theme::TOOLBAR_BG);
    egui::Panel::top("status_toolbar")
        .frame(frame)
        .exact_size(theme::TOOLBAR_HEIGHT)
        .show_separator_line(false)
        .resizable(false)
        .show(ui, |ui| {
            if !context.usable {
                ui.disable();
            }
            let bar = ui.max_rect();
            match context.mode {
                Mode::Browse => browse(ui, bar, context, state, actions),
                Mode::Find => find(ui, bar, context, state, actions),
                Mode::Layout => layout(ui, bar, context, actions),
                Mode::Preview => preview(ui, bar, context, state, actions),
            }
        });
}

/// A toolbar column of `width` centered at `x` (window-relative).
fn column(bar: Rect, x: f32, width: f32) -> Rect {
    Rect::from_center_size(
        pos2(bar.left() + x, bar.center().y),
        vec2(width, bar.height()),
    )
}

fn browse(
    ui: &mut Ui,
    bar: Rect,
    context: &ToolbarContext<'_>,
    state: &mut ToolbarState,
    actions: &mut Vec<Action>,
) {
    let session = context.session;
    let nav = Nav {
        current: session.current,
        count: session.total(),
        detail: context.detail(),
        caption: "Records",
        number_label: "Record Number",
        loading: session.is_loading_records(),
    };
    if let Some(index) = navigation(ui, bar, &nav, &mut state.nav_text) {
        actions.push(Action::GoToRecord(index));
    }

    if toolbar_button(
        ui,
        column(bar, 335.0, 76.0),
        Icon::ShowAll,
        "Show All",
        context.partial(),
        false,
    )
    .on_disabled_hover_text("All records are already showing")
    .clicked()
    {
        actions.push(Action::Step(crate::app::steps::SHOW_ALL, 0));
    }
    if toolbar_button(
        ui,
        column(bar, 418.0, 80.0),
        Icon::NewRecord,
        "New Record",
        context.record_create && context.commands,
        false,
    )
    .on_disabled_hover_text(
        "Record writes are unavailable on this connection or a save is in progress.",
    )
    .clicked()
    {
        actions.push(Action::NewRecord);
    }
    if toolbar_button(
        ui,
        column(bar, 502.0, 84.0),
        Icon::DeleteRecord,
        "Delete Record",
        context.record_delete
            && context.commands
            && context.session.total().is_some_and(|total| total > 0),
        false,
    )
    .on_disabled_hover_text(
        "Record writes are unavailable on this connection or no record is selected.",
    )
    .clicked()
    {
        actions.push(Action::DeleteRecord);
    }

    // Find is a split button: the glyph enters Find mode, the chevron holds
    // find options.
    let find = column(bar, 594.0, 48.0);
    let glyph = Rect::from_center_size(
        pos2(bar.left() + 582.0, bar.top() + TOOLBAR_ICON_Y),
        vec2(24.0, 24.0),
    );
    let find_enabled = context.commands && ui.is_enabled();
    let sense = if find_enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.allocate_rect(glyph, sense);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(
            glyph.center(),
            egui::Vec2::splat(widgets::TOOLBAR_ICON_SIZE),
        ),
        Icon::Find,
        if !find_enabled {
            theme::DISABLED
        } else if response.is_pointer_button_down_on() {
            theme::TEXT
        } else {
            theme::ICON
        },
        theme::TOOLBAR_BG,
    );
    let caption = ui.allocate_rect(
        Rect::from_center_size(
            pos2(find.center().x, bar.top() + widgets::TOOLBAR_CAPTION_Y),
            vec2(find.width(), 14.0),
        ),
        sense,
    );
    widgets::toolbar_caption(ui, find, "Find", find_enabled);
    let response = response.union(caption);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, find_enabled, "Find"));
    if response.clicked() {
        actions.push(Action::SetMode(Mode::Find));
    }
    let options = widgets::split_chevron(
        ui,
        Rect::from_min_size(
            pos2(bar.left() + 597.0, bar.top() + TOOLBAR_ICON_Y - 9.0),
            vec2(18.0, 18.0),
        ),
        "Find options",
        find_enabled,
    );
    egui::Popup::menu(&options)
        .style(theme::menu_popup_style)
        .show(|ui| {
            // As Records > Modify Last Find: Find mode with the last find's
            // requests. Saved finds are not built yet.
            if widgets::menu_item(ui, "Modify Last Find", None, false, find_enabled).clicked() {
                actions.push(Action::Step(crate::app::steps::MODIFY_LAST_FIND, 0));
            }
            widgets::menu_separator(ui);
            widgets::menu_item(ui, "Save Current Find...", None, false, false);
            widgets::menu_item(ui, "Edit Saved Finds...", None, false, false);
        });

    if toolbar_button(
        ui,
        column(bar, 653.0, 50.0),
        Icon::Sort,
        "Sort",
        // Not under Editing only menus, nor on a layout the account may
        // not see (#137's reading).
        context.commands
            && context
                .session
                .layout
                .is_none_or(|id| context.session.shows_layout(id)),
        false,
    )
    .clicked()
    {
        actions.push(Action::Step(crate::app::steps::SORT_RECORDS, 0));
    }
    let share = toolbar_button(
        ui,
        column(bar, 711.0, 54.0),
        Icon::Share,
        "Share",
        true,
        true,
    );
    egui::Popup::menu(&share)
        .style(theme::menu_popup_style)
        .show(|ui| {
            for (item, format) in [
                ("Save as Excel...", SaveFormat::Excel),
                ("Save as PDF...", SaveFormat::Pdf),
            ] {
                let enabled = context.save_records.contains(&format);
                if widgets::menu_item(ui, item, None, false, enabled).clicked() {
                    actions.push(Action::SaveRecordsAs(format));
                }
            }
            // Save Records as Snapshot Link (152) has no implementation.
            widgets::menu_item(ui, "Save as Snapshot Link...", None, false, false);
            // File > Print.
            if widgets::menu_item(ui, "Print...", None, false, context.print).clicked() {
                actions.push(Action::Step(43, 0));
            }
        });

    let enabled = layout_quick_find(context.session);
    quick_find(ui, bar, 745.0, enabled && context.commands, state, actions);
}

/// The shown layout has loaded and allows Quick Find: Layout Setup's
/// "Enable Quick Find" (layout-model.md 4.2).
pub(crate) fn layout_quick_find(session: &crate::session::Session) -> bool {
    match &session.render {
        crate::session::RenderState::Loaded(render) => render
            .layout
            .def
            .as_ref()
            .is_none_or(om_layout::LayoutDef::quick_find),
        _ => false,
    }
}

/// The Quick Find search field filling the rest of the toolbar; Return
/// runs Perform Quick Find with its text. On a layout with Quick Find
/// turned off, or when the menu set removes Find, it is disabled, as
/// FileMaker Pro 26.0.3 disables it under Editing only (#541): drawn so
/// and reported so, taking no text.
fn quick_find(
    ui: &mut Ui,
    bar: Rect,
    left: f32,
    enabled: bool,
    state: &mut ToolbarState,
    actions: &mut Vec<Action>,
) {
    let right = bar.right() - 6.0;
    let left = bar.left() + left;
    if right - left < 80.0 {
        return;
    }
    let rect = Rect::from_min_max(
        pos2(left, bar.top() + TOOLBAR_ICON_Y - 11.0),
        pos2(right, bar.top() + TOOLBAR_ICON_Y + 11.0),
    );
    // Each window's toolbar has its own field (app/windows.rs).
    let id = ui.scope_id().with("quick_find");
    let focused = ui.memory(|memory| memory.has_focus(id));
    // Disabled, the box keeps its shape on the toolbar's own grey, with
    // grey icons and hint [I: FileMaker Pro's disabled box was not compared
    // by screenshot].
    let (fill, icon) = if enabled {
        (theme::SEARCH_FILL, theme::ICON)
    } else {
        (theme::TOOLBAR_BG, theme::DISABLED)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(6),
        if focused && enabled {
            theme::CONTROL_BG
        } else {
            fill
        },
        Stroke::new(
            1.0,
            if focused && enabled {
                theme::FOCUS
            } else {
                theme::SEARCH_BORDER
            },
        ),
        StrokeKind::Inside,
    );
    // The magnifier-with-chevron that opens recent searches in FileMaker.
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.left_center() + vec2(12.0, 0.0), vec2(14.0, 14.0)),
        Icon::Search,
        icon,
        fill,
    );
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.left_center() + vec2(23.0, 1.0), vec2(8.0, 8.0)),
        Icon::ChevronDown,
        icon,
        fill,
    );
    let text_rect = Rect::from_min_max(rect.min + vec2(30.0, 0.0), rect.max - vec2(6.0, 0.0));
    let hint = if enabled {
        theme::WEAK_TEXT
    } else {
        theme::DISABLED
    };
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            ui.put(
                text_rect,
                egui::TextEdit::singleline(&mut state.quick_find)
                    .id(id)
                    .interactive(enabled)
                    .hint_text(egui::RichText::new("Search").color(hint))
                    .font(theme::body())
                    .frame(egui::Frame::NONE)
                    .align(Align2::LEFT_CENTER)
                    .desired_width(text_rect.width()),
            )
        })
        .inner;
    widgets::accessible_label(ui, response.id, "Quick Find");
    if enabled
        && response.lost_focus()
        && ui.input(|input| input.key_pressed(egui::Key::Enter))
        && !state.quick_find.trim().is_empty()
    {
        actions.push(Action::QuickFind(state.quick_find.clone()));
    }
}

fn find(
    ui: &mut Ui,
    bar: Rect,
    context: &ToolbarContext<'_>,
    state: &mut ToolbarState,
    actions: &mut Vec<Action>,
) {
    let find = context.find;
    let nav = Nav {
        current: find.current_index() as u64,
        count: Some(find.len() as u64),
        detail: "Total",
        caption: "Find Requests",
        number_label: "Request Number",
        loading: false,
    };
    if let Some(index) = navigation(ui, bar, &nav, &mut state.nav_text) {
        actions.push(Action::GoToRequest(usize::try_from(index).unwrap_or(0)));
    }
    if toolbar_button(
        ui,
        column(bar, 318.0, 80.0),
        Icon::NewRequest,
        "New Request",
        context.commands,
        false,
    )
    .clicked()
    {
        actions.push(Action::NewRequest);
    }
    if toolbar_button(
        ui,
        column(bar, 408.0, 88.0),
        Icon::DeleteRequest,
        "Delete Request",
        find.len() > 1 && context.commands,
        false,
    )
    .clicked()
    {
        actions.push(Action::DeleteRequest);
    }
    if toolbar_button(
        ui,
        column(bar, 512.0, 80.0),
        Icon::PerformFind,
        "Perform Find",
        true,
        false,
    )
    .clicked()
    {
        actions.push(Action::PerformFind);
    }
    if toolbar_button(
        ui,
        column(bar, 591.0, 70.0),
        Icon::CancelFind,
        "Cancel Find",
        true,
        false,
    )
    .clicked()
    {
        actions.push(Action::SetMode(Mode::Browse));
    }
    let saved = toolbar_button(
        ui,
        Rect::from_center_size(
            pos2(bar.right() - 44.0, bar.center().y),
            vec2(70.0, bar.height()),
        ),
        Icon::SavedFinds,
        "Saved Finds",
        true,
        true,
    );
    egui::Popup::menu(&saved)
        .style(theme::menu_popup_style)
        .show(|ui| {
            widgets::menu_item(ui, "No Saved Finds", None, false, false);
            widgets::menu_separator(ui);
            widgets::menu_item(ui, "Edit Saved Finds...", None, false, false);
        });
}

fn preview(
    ui: &mut Ui,
    bar: Rect,
    context: &ToolbarContext<'_>,
    state: &mut ToolbarState,
    actions: &mut Vec<Action>,
) {
    let session = context.session;
    let position = session.preview_position();
    let nav = Nav {
        current: position.map_or(0, |p| p.0),
        count: position
            .map(|p| p.1)
            .or_else(|| session.total().filter(|n| *n == 0)),
        detail: "Total",
        caption: "Pages",
        number_label: "Page Number",
        loading: session.is_loading_records(),
    };
    if let Some(index) = navigation(ui, bar, &nav, &mut state.nav_text) {
        actions.push(Action::GoToPage(index));
    }
    // The File menu's commands: Save Records as Excel and PDF, Print (43)
    // and Page Setup (42).
    let save = |format| {
        (
            Action::SaveRecordsAs(format),
            context.save_records.contains(&format),
        )
    };
    let buttons = [
        (
            303.0,
            80.0,
            Icon::SaveExcel,
            "Save as Excel",
            save(SaveFormat::Excel),
        ),
        (
            386.0,
            76.0,
            Icon::SavePdf,
            "Save as PDF",
            save(SaveFormat::Pdf),
        ),
        (
            469.0,
            60.0,
            Icon::Print,
            "Print",
            (Action::Step(43, 0), context.print),
        ),
        (
            536.0,
            70.0,
            Icon::PageSetup,
            "Page Setup",
            (Action::Step(42, 0), context.print),
        ),
    ];
    for (x, width, icon, caption, (action, enabled)) in buttons {
        if toolbar_button(ui, column(bar, x, width), icon, caption, enabled, false).clicked() {
            actions.push(action);
        }
    }
}

fn layout(ui: &mut Ui, bar: Rect, context: &ToolbarContext<'_>, actions: &mut Vec<Action>) {
    if context.objects {
        ui.painter().rect_filled(
            Rect::from_center_size(
                pos2(bar.left() + 31.0, bar.top() + TOOLBAR_ICON_Y),
                vec2(32.0, 22.0),
            ),
            CornerRadius::same(5),
            theme::HOVER_FILL,
        );
    }
    if toolbar_button(
        ui,
        column(bar, 31.0, 54.0),
        Icon::Objects,
        "Objects",
        true,
        false,
    )
    .clicked()
    {
        actions.push(Action::ToggleObjects);
    }
    if toolbar_button(
        ui,
        column(bar, 120.0, 108.0),
        Icon::NewLayout,
        "New Layout / Report",
        true,
        false,
    )
    .clicked()
    {
        actions.push(Action::NewLayout);
    }

    // The centered tool palette. FileMaker spaces it by caption; it shrinks
    // the gaps when the window is narrow.
    let tools = LayoutTool::ALL;
    let widths: Vec<f32> = tools
        .iter()
        .map(|tool| widgets::caption_width(ui, tool.label()).max(28.0))
        .collect();
    let left_edge = bar.left() + 180.0;
    let right_edge = bar.right() - 150.0;
    let natural: f32 = widths.iter().sum();
    let gap = ((right_edge - left_edge - natural) / tools.len() as f32).clamp(4.0, 22.0);
    let total = natural + gap * tools.len() as f32;
    let mut x = (bar.center().x - total / 2.0).max(left_edge);
    for (tool, width) in tools.into_iter().zip(widths) {
        let rect = Rect::from_min_size(pos2(x, bar.top()), vec2(width + gap, bar.height()));
        x += width + gap;
        let response = tool_button(ui, rect, tool, context.tool == tool);
        // The Part tool's button is also dragged onto the layout, which
        // adds a part (`layout_canvas::PartToolDrag`).
        if tool == LayoutTool::Part {
            response.dnd_set_drag_payload(crate::layout_canvas::PartToolDrag);
        }
        if tool == LayoutTool::Field && response.double_clicked() {
            actions.push(Action::LockFieldTool);
        } else if response.clicked() {
            actions.push(Action::SelectTool(tool));
        }
    }

    let manage = toolbar_button(
        ui,
        Rect::from_center_size(
            pos2(bar.right() - 112.0, bar.center().y),
            vec2(56.0, bar.height()),
        ),
        Icon::Manage,
        "Manage",
        true,
        true,
    );
    egui::Popup::menu(&manage)
        .style(theme::menu_popup_style)
        // Only Layout mode shows it, and only a design-allowed account
        // enters Layout mode.
        .show(|ui| crate::menubar::manage_items(&mut crate::menubar::Drawn(ui), true, actions));
    let inspector = Rect::from_center_size(
        pos2(bar.right() - 35.0, bar.center().y),
        vec2(60.0, bar.height()),
    );
    if context.inspector {
        let highlight = Rect::from_center_size(
            pos2(inspector.center().x, bar.top() + TOOLBAR_ICON_Y),
            vec2(32.0, 22.0),
        );
        ui.painter()
            .rect_filled(highlight, CornerRadius::same(5), theme::HOVER_FILL);
    }
    if toolbar_button(ui, inspector, Icon::Inspector, "Inspector", true, false).clicked() {
        actions.push(Action::ToggleInspector);
    }
}

/// A layout tool: like a toolbar button, but the active tool sits in a blue
/// rounded box with a white glyph.
fn tool_button(ui: &mut Ui, rect: Rect, tool: LayoutTool, selected: bool) -> Response {
    let sense = if tool == LayoutTool::Part {
        Sense::click_and_drag()
    } else {
        Sense::click()
    };
    let response = ui.allocate_rect(rect, sense);
    let icon_center = pos2(rect.center().x, rect.top() + TOOLBAR_ICON_Y);
    let split = tool.has_options();
    let glyph_center = if split {
        icon_center - vec2(6.0, 0.0)
    } else {
        icon_center
    };
    let painter = ui.painter();
    let (color, background) = if selected {
        let highlight = Rect::from_center_size(icon_center, vec2(48.0, 21.0));
        painter.rect_filled(highlight, CornerRadius::same(5), theme::SELECTED);
        (theme::ON_ACCENT, theme::SELECTED)
    } else {
        (theme::ICON, theme::TOOLBAR_BG)
    };
    icons::paint(
        painter,
        Rect::from_center_size(glyph_center, egui::Vec2::splat(widgets::TOOLBAR_ICON_SIZE)),
        tool.icon(),
        color,
        background,
    );
    if split {
        icons::paint(
            painter,
            Rect::from_center_size(glyph_center + vec2(16.0, 1.0), egui::Vec2::splat(9.0)),
            Icon::ChevronDown,
            color,
            background,
        );
    }
    widgets::toolbar_caption(ui, rect, tool.label(), true);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, selected, tool.label())
    });
    response
}

/// The record/request/page navigation group: chevrons, the number box with
/// its slider, the found-set ring and the count. Returns a newly requested
/// position.
fn navigation(ui: &mut Ui, bar: Rect, nav: &Nav, text: &mut String) -> Option<u64> {
    let mut target = None;
    let count = nav.count.unwrap_or(0);
    let has_items = count > 0;
    let icon_y = bar.top() + TOOLBAR_ICON_Y;

    let chevron = |ui: &mut Ui, x: f32, icon: Icon, label: &str, enabled: bool| {
        let rect = Rect::from_center_size(pos2(bar.left() + x, icon_y), vec2(18.0, 22.0));
        let response = ui.allocate_rect(
            rect,
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let color = if !enabled {
            theme::DISABLED
        } else if response.is_pointer_button_down_on() {
            theme::TEXT
        } else {
            theme::WEAK_TEXT
        };
        icons::paint(ui.painter(), rect, icon, color, theme::TOOLBAR_BG);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
        response.clicked()
    };
    if chevron(
        ui,
        27.0,
        Icon::ChevronLeft,
        "Previous",
        has_items && nav.current > 0,
    ) {
        target = Some(nav.current - 1);
    }
    ui.painter().vline(
        bar.left() + 42.0,
        icon_y - 8.0..=icon_y + 8.0,
        Stroke::new(1.0, theme::HAIRLINE),
    );
    if chevron(
        ui,
        57.0,
        Icon::ChevronRight,
        "Next",
        has_items && nav.current + 1 < count,
    ) {
        target = Some(nav.current + 1);
    }

    // The number box. While it is not being edited it shows the position.
    let id = ui.scope_id().with("nav_number");
    let editing = ui.memory(|memory| memory.has_focus(id));
    if !editing {
        *text = if has_items {
            (nav.current + 1).to_string()
        } else {
            String::new()
        };
    }
    let number = Rect::from_min_size(pos2(bar.left() + 78.0, bar.top() + 6.0), vec2(71.0, 14.0));
    ui.painter().rect(
        number,
        CornerRadius::ZERO,
        theme::CONTROL_BG,
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    // Let the edit be as short as the box so the number centers in it.
    let interact_height = ui.spacing().interact_size.y;
    ui.spacing_mut().interact_size.y = number.height();
    let response = ui.put(
        number.shrink2(vec2(3.0, 0.0)),
        egui::TextEdit::singleline(text)
            .id(id)
            .font(theme::small())
            .text_color(theme::CAPTION)
            .align(Align2::CENTER_CENTER)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .interactive(has_items),
    );
    ui.spacing_mut().interact_size.y = interact_height;
    widgets::accessible_label(ui, response.id, nav.number_label);
    if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
        if let Ok(position) = text.trim().parse::<u64>() {
            if position >= 1 {
                target = Some((position - 1).min(count.saturating_sub(1)));
            }
        }
    }

    if let Some(position) = slider(ui, bar, nav.current, count) {
        target = Some(position);
    }
    ring(
        ui,
        pos2(bar.left() + 169.0, icon_y),
        nav.count.is_some(),
        nav.loading,
    );

    let count_text = nav
        .count
        .map_or_else(|| "-".to_owned(), |count| count.to_string());
    let count_rect = |y: f32| {
        Rect::from_min_size(
            pos2(bar.left() + 188.0, bar.top() + y - 7.0),
            vec2(96.0, 14.0),
        )
    };
    let left = egui::Align::Min;
    widgets::put_label(
        ui,
        count_rect(11.0),
        &count_text,
        theme::small(),
        theme::TEXT,
        left,
    );
    widgets::put_label(
        ui,
        count_rect(25.0),
        nav.detail,
        theme::small(),
        theme::TEXT,
        left,
    );
    // The group caption, centered under the whole group.
    let caption = Rect::from_center_size(
        pos2(bar.left() + 153.0, bar.top() + widgets::TOOLBAR_CAPTION_Y),
        vec2(200.0, 14.0),
    );
    widgets::put_label(
        ui,
        caption,
        nav.caption,
        theme::small(),
        theme::CAPTION,
        egui::Align::Center,
    );
    target.filter(|&position| position != nav.current || nav.count.is_none())
}

/// The record slider under the number box.
fn slider(ui: &mut Ui, bar: Rect, current: u64, count: u64) -> Option<u64> {
    let track = Rect::from_min_size(pos2(bar.left() + 80.0, bar.top() + 24.0), vec2(67.0, 4.0));
    let hit = track.expand2(vec2(4.0, 5.0));
    let response = ui.interact(
        hit,
        ui.scope_id().with("nav_slider"),
        if count > 1 {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let fraction = if count > 1 {
        current as f32 / (count - 1) as f32
    } else {
        0.0
    };
    let painter = ui.painter();
    painter.rect(
        track,
        CornerRadius::same(2),
        theme::SEPARATOR,
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    let knob_x = track.left() + 2.0 + fraction * (track.width() - 4.0);
    let knob = Rect::from_center_size(pos2(knob_x, track.center().y), vec2(6.0, 10.0));
    painter.rect(
        knob,
        CornerRadius::same(3),
        theme::CONTROL_BG,
        Stroke::new(1.0, theme::BUTTON_BORDER),
        StrokeKind::Inside,
    );
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::Role::Slider, count > 1, "Current Record");
        info.value = Some((current + 1) as f64);
        info
    });
    if count > 1 && (response.dragged() || response.clicked()) {
        let x = response.interact_pointer_pos()?.x;
        let fraction = ((x - track.left()) / track.width()).clamp(0.0, 1.0);
        let position = (fraction * (count - 1) as f32).round() as u64;
        return (position != current).then_some(position);
    }
    None
}

/// The found-set ring: a full light-blue ring when every record is in the
/// found set (a darker wedge will show a partial found set once finds
/// exist), and an activity spinner while records load.
fn ring(ui: &mut Ui, center: egui::Pos2, known: bool, loading: bool) {
    let rect = Rect::from_center_size(center, vec2(24.0, 24.0));
    let response = ui.allocate_rect(rect, Sense::hover());
    if loading {
        egui::Spinner::new()
            .size(21.0)
            .color(theme::RING)
            .paint_at(ui, rect);
        return;
    }
    ui.painter().circle_stroke(
        center,
        9.5,
        Stroke::new(2.5, if known { theme::RING } else { theme::HAIRLINE }),
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "Percent in Found Set"));
}
