//! Scripted vertical movement of the document, independent of its editors
//! and nested portal/popover scroll areas. Each DocumentState owns one.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use egui::{scroll_area, Id, Pos2, Ui, Vec2};

use crate::scripting::{ScrollContext, ScrollRequest, WindowState};

#[derive(Default)]
pub(crate) struct Scroll {
    pending: Vec<Queued>,
    context: Option<ScrollContext>,
    zoom: u16,
    at_end: bool,
    metrics: Option<Metrics>,
    fields: BTreeMap<String, Vec<FieldObject>>,
    /// Accumulate a complete paint, including fixed parts before/after
    /// the body. It becomes the measured registry at the next paint.
    painting_fields: BTreeMap<String, Vec<FieldObject>>,
    calculation_revision: Option<u64>,
    focused: Option<(String, Id)>,
    page: Option<crate::session::PageKey>,
    loading: bool,
    origin: Option<Pos2>,
}

struct Queued {
    request: ScrollRequest,
    object: Option<Id>,
}

struct FieldObject {
    id: Id,
    target: FieldTarget,
}

enum FieldTarget {
    Content(RangeInclusive<f32>),
    /// Header/Footer fields already live outside this scrolling body.
    Fixed,
}

struct Metrics {
    id: Id,
    height: f32,
    viewport: f32,
    available: f32,
    available_width: f32,
}

pub(crate) struct Frame {
    at_end: bool,
    id: Id,
    offset: Option<Vec2>,
    available: f32,
    available_width: f32,
}

impl Scroll {
    #[cfg(test)]
    pub(crate) fn is_measured(&self) -> bool {
        self.metrics.is_some()
    }

    #[cfg(test)]
    pub(crate) fn measured_motion(&self, ctx: &egui::Context) -> (f32, f32, f32, usize) {
        let metrics = self.metrics.as_ref().expect("measured scroll area");
        (
            egui::scroll_area::State::load(ctx, metrics.id)
                .unwrap()
                .clamped_offset()
                .y,
            metrics.height,
            metrics.viewport,
            self.pending.len(),
        )
    }

    pub fn context(&mut self, context: ScrollContext, zoom: u16) {
        if self.context != Some(context) || self.zoom != zoom {
            let same_form = self.context.is_some_and(|old| {
                old.layout == context.layout
                    && old.mode == context.mode
                    && old.view == context.view
                    && context.view == crate::scripting::View::Form
                    && (context.mode != om_data::runtime::Mode::Preview
                        || old.preview_page == context.preview_page)
            });
            if !same_form {
                self.at_end = false;
            }
            if self.context != Some(context) {
                self.focused = None;
            }
            self.pending
                .retain(|queued| queued.request.matches_context(context));
            self.metrics = None;
            self.fields.clear();
            self.painting_fields.clear();
            self.context = Some(context);
            self.zoom = zoom;
        }
    }

    pub fn queue(&mut self, requests: Vec<ScrollRequest>, state: &WindowState) {
        self.context(state.into(), state.zoom);
        let before = self.pending.len();
        self.pending.extend(
            requests
                .into_iter()
                .filter(|r| r.matches(state))
                .map(|request| {
                    // Preserve genuine user focus across dialogs/repaints. A
                    // script's Go to Field instead resolves its new instance
                    // from the complete measured paint, not the old widget.
                    let object = self
                        .focused
                        .as_ref()
                        .filter(|(name, _)| {
                            !request.field_focus && request.field.as_ref() == Some(name)
                        })
                        .map(|(_, id)| *id);
                    Queued { request, object }
                }),
        );
        if self.pending.len() != before {
            // Calculated text can change without a record reload. Measure
            // the current content for every new batch, even when its page
            // key, record, width and zoom are unchanged.
            self.metrics = None;
        }
    }

    /// Start a surface paint with the preceding complete field registry.
    /// Record reloads can change a generic Form's height without changing
    /// its layout, record index or width. Do not consume commands on the
    /// retained old page while a primary fetch is in flight; measure the
    /// accepted page before applying them. Offscreen range fetches do not
    /// block movement of the current document.
    pub fn begin_paint(&mut self, session: &crate::session::Session) {
        self.start_fields();
        let page = session.page.as_ref().map(|page| page.key);
        let loading = session.pending.is_some();
        if self.page != page || loading || self.loading {
            self.metrics = None;
        }
        self.page = page;
        self.loading = loading;
    }

    fn start_fields(&mut self) {
        std::mem::swap(&mut self.fields, &mut self.painting_fields);
        self.painting_fields.clear();
        self.origin = None;
    }

    /// Calculation replies and context changes can alter wrapped text or
    /// visibility without replacing a record page. Measure that revision
    /// before consuming effects queued against the preceding content.
    pub fn calculations(&mut self, revision: Option<u64>) {
        if self.calculation_revision != revision {
            self.calculation_revision = revision;
            self.metrics = None;
        }
    }

    pub fn focused(&mut self, name: &str, id: Id) {
        self.focused = Some((name.to_owned(), id));
    }

    /// The renderer fulfilled a pending field-by-name focus request. Old
    /// widgets that merely still have focus must not resolve that request.
    pub fn resolve_focus(&mut self, name: &str, id: Id) {
        if !self.loading {
            for queued in &mut self.pending {
                if queued.object.is_none()
                    && queued.request.field_focus
                    && queued.request.field.as_deref() == Some(name)
                {
                    // Bind an initially unpainted Go to Field once the UI
                    // resolves it. A later visibility change cannot turn
                    // that selection into another duplicate's geometry.
                    queued.object = Some(id);
                }
            }
        }
    }

    /// Use actual scroll-area measurements. A newly selected surface gets
    /// one paint before commands consume them; no guessed page height.
    pub fn begin(
        &mut self,
        ui: &Ui,
        salt: impl egui::AsIdSalt,
        selection: Option<RangeInclusive<f32>>,
    ) -> Frame {
        let id = ui.make_persistent_id(egui::IdSalt::new(salt));
        let mut offset = None;
        if !self.pending.is_empty() && !self.loading && !ui.is_sizing_pass() {
            if let Some(metrics) = self.metrics.as_ref().filter(|m| {
                m.id == id && (m.available_width - ui.available_width()).abs() <= f32::EPSILON
            }) {
                let previous = scroll_area::State::load(ui.ctx(), id).unwrap_or_default();
                let current = previous.clamped_offset();
                // Toolbars and pause bars can change since the last paint.
                // Preserve this surface's measured header/footer overhead.
                let viewport = (metrics.viewport + ui.available_height() - metrics.available)
                    .clamp(0.0, ui.available_height().max(0.0));
                let max = (metrics.height - viewport).max(0.0);
                let mut y = current.y.clamp(0.0, max);
                let mut move_area = false;
                for Queued { request, object } in self.pending.drain(..) {
                    let targets = request
                        .field
                        .as_ref()
                        .and_then(|name| self.fields.get(name));
                    let target = match object {
                        Some(id) => targets
                            .and_then(|targets| targets.iter().find(|target| target.id == id)),
                        None => targets.and_then(|targets| targets.first()),
                    };
                    let field = match target.map(|object| &object.target) {
                        Some(FieldTarget::Content(range)) => Some(range),
                        Some(FieldTarget::Fixed) => None,
                        // A disappeared selected object must not redirect
                        // the command to another copy or a List body row.
                        None if object.is_some() => None,
                        None => selection.as_ref(),
                    };
                    let next = destination(y, max, viewport, request.choice, field);
                    if request.choice != 5 || next != y {
                        self.at_end = request.choice == 2;
                        move_area = true;
                    }
                    y = next;
                }
                // egui 0.37 has no public setter that cancels a previous
                // scroll animation/drag velocity. Reset only this area's
                // motion and restore both offsets through its public builder.
                // A visible/fixed selection is a no-op. Preserve egui's
                // released bottom anchor and manual wheel motion in that
                // case; a default State would silently reactivate End.
                if move_area {
                    scroll_area::State::default().store(ui.ctx(), id);
                    offset = Some(Vec2::new(current.x, y));
                }
            } else {
                ui.ctx().request_repaint();
            }
        }
        self.origin = None;
        Frame {
            at_end: self.at_end,
            id,
            offset,
            available: ui.available_height(),
            available_width: ui.available_width(),
        }
    }

    /// The root content origin, before a virtualized row's skipped space.
    pub fn origin(&mut self, ui: &Ui, origin: Pos2) {
        self.origin = Some(
            ui.ctx()
                .layer_transform_to_global(ui.layer_id())
                .map_or(origin, |transform| transform * origin),
        );
    }

    pub fn field(&mut self, ui: &Ui, name: &str, rect: egui::Rect) {
        self.field_with_id(
            ui,
            name,
            rect,
            ui.make_persistent_id(("scroll_field", name)),
        );
    }

    pub fn field_with_id(&mut self, ui: &Ui, name: &str, rect: egui::Rect, id: Id) {
        let rect = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .map_or(rect, |transform| transform * rect);
        let target = self.origin.map_or(FieldTarget::Fixed, |origin| {
            FieldTarget::Content((rect.top() - origin.y)..=(rect.bottom() - origin.y))
        });
        let fields = self.painting_fields.entry(name.to_owned()).or_default();
        if let Some(field) = fields.iter_mut().find(|field| field.id == id) {
            field.target = target;
        } else {
            fields.push(FieldObject { id, target });
        }
    }

    pub fn finish<R>(&mut self, frame: Frame, output: &scroll_area::ScrollAreaOutput<R>) {
        debug_assert_eq!(frame.id, output.id);
        self.origin = None;
        self.metrics = (!self.loading).then_some(Metrics {
            id: output.id,
            height: output.content_size.y,
            viewport: output.inner_rect.height(),
            available: frame.available,
            available_width: frame.available_width,
        });
    }
}

fn destination(
    y: f32,
    max: f32,
    page: f32,
    choice: u16,
    field: Option<&RangeInclusive<f32>>,
) -> f32 {
    match choice {
        1 => 0.0,
        2 => max,
        3 => (y - page).clamp(0.0, max),
        4 => (y + page).clamp(0.0, max),
        5 => field
            .map_or(y, |field| {
                if *field.start() < y {
                    *field.start()
                } else if *field.end() > y + page {
                    (*field.end() - page).min(*field.start())
                } else {
                    y
                }
            })
            .clamp(0.0, max),
        _ => y,
    }
}

impl Frame {
    /// egui releases its bottom anchor on manual scrolling. Keep it enabled
    /// after End so a late pause bar or resized viewport still shows the end.
    pub fn at_end(&self) -> bool {
        self.at_end
    }
    pub fn offset(&self) -> Option<Vec2> {
        self.offset
    }

    /// Explicit scrolling wins over a TextEdit's automatic cursor scroll
    /// in this paint. The target is already visible and has no horizontal
    /// delta; nested scroll areas have finished before this call.
    pub fn hold(&self, ui: &Ui) {
        if self.offset.is_some() {
            ui.scroll_to_rect_animation(ui.clip_rect(), None, egui::style::ScrollAnimation::none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::destination;
    #[test]
    fn ordered_pages_clamp_each_step_and_selection_only_moves_when_needed() {
        let mut y = 0.0;
        for choice in [3, 4] {
            y = destination(y, 1000.0, 100.0, choice, None);
        }
        assert_eq!(y, 100.0);
        for choice in [2, 4, 3] {
            y = destination(y, 1000.0, 100.0, choice, None);
        }
        assert_eq!(y, 900.0);
        assert_eq!(
            destination(100.0, 1000.0, 100.0, 5, Some(&(120.0..=140.0))),
            100.0
        );
        assert_eq!(
            destination(100.0, 1000.0, 100.0, 5, Some(&(400.0..=420.0))),
            320.0
        );
        assert_eq!(destination(100.0, 1000.0, 100.0, 5, None), 100.0);
    }

    fn draw(
        ctx: &egui::Context,
        scroll: &mut super::Scroll,
        initial: bool,
    ) -> (egui::Vec2, f32, f32) {
        draw_height(ctx, scroll, initial, 300.0)
    }

    fn draw_height(
        ctx: &egui::Context,
        scroll: &mut super::Scroll,
        initial: bool,
        height: f32,
    ) -> (egui::Vec2, f32, f32) {
        scroll.start_fields();
        let mut measured = None;
        let mut rendered = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, height),
                )),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let frame = scroll.begin(ui, "script_surface", None);
                    let mut area = egui::ScrollArea::both()
                        .id_salt("script_surface")
                        .stick_to_bottom(frame.at_end())
                        .auto_shrink([false, false]);
                    if initial {
                        area = area.horizontal_scroll_offset(200.0);
                    }
                    if let Some(offset) = frame.offset() {
                        area = area.scroll_offset(offset);
                    }
                    let output = area.show(ui, |ui| {
                        let origin = ui.max_rect().min;
                        scroll.origin(ui, origin);
                        scroll.field(
                            ui,
                            "Code",
                            egui::Rect::from_min_size(
                                origin + egui::vec2(500.0, 400.0),
                                egui::vec2(100.0, 30.0),
                            ),
                        );
                        ui.allocate_exact_size(egui::vec2(1600.0, 1200.0), egui::Sense::hover());
                        if initial {
                            let clip = ui.clip_rect();
                            let target = egui::Rect::from_x_y_ranges(
                                clip.x_range(),
                                (origin.y + 900.0)..=(origin.y + 920.0),
                            );
                            ui.scroll_to_rect(target, None);
                        }
                        frame.hold(ui);
                    });
                    measured = Some((
                        output.state.clamped_offset(),
                        output.content_size.y,
                        output.inner_rect.height(),
                    ));
                    scroll.finish(frame, &output);
                });
            },
        );
        rendered.textures_delta.clear();
        measured.unwrap()
    }

    #[test]
    fn vertical_commands_preserve_horizontal_position_and_use_the_measured_viewport() {
        let ctx = egui::Context::default();
        let state = crate::scripting::WindowState::default();
        let mut scroll = super::Scroll::default();
        scroll.context((&state).into(), state.zoom);
        draw(&ctx, &mut scroll, true);
        draw(&ctx, &mut scroll, false);
        let request = |choice| crate::scripting::ScrollRequest {
            choice,
            context: (&state).into(),
            field: Some("Code".into()),
            field_focus: false,
        };
        scroll.queue(vec![request(2)], &state);
        draw(&ctx, &mut scroll, false);
        let (end, height, viewport) = draw(&ctx, &mut scroll, false);
        assert!(
            (end.y - (height - viewport)).abs() < 1.0,
            "{end:?} {height} {viewport}"
        );
        assert!((end.x - 200.0).abs() < 1.0);
        scroll.queue(vec![request(4), request(3)], &state);
        draw(&ctx, &mut scroll, false);
        let (up, _, _) = draw(&ctx, &mut scroll, false);
        assert!((up.y - (end.y - viewport)).abs() < 1.0);
        assert!((up.x - 200.0).abs() < 1.0);
        scroll.queue(vec![request(5)], &state);
        draw(&ctx, &mut scroll, false);
        let (selected, _, _) = draw(&ctx, &mut scroll, false);
        assert!((selected.y - 400.0).abs() < 1.0);
        assert!((selected.x - 200.0).abs() < 1.0);
        for _ in 0..5 {
            assert!((draw(&ctx, &mut scroll, false).0.y - 400.0).abs() < 1.0);
        }
        for height in [200.0, 400.0] {
            scroll.queue(vec![request(1), request(4)], &state);
            draw_height(&ctx, &mut scroll, false, height);
            let (offset, _, viewport) = draw_height(&ctx, &mut scroll, false, height);
            assert!(
                (offset.y - viewport).abs() < 1.0,
                "resized viewport {height}: {offset:?} page {viewport}"
            );
        }
    }

    #[test]
    fn scroll_window_review_generic_form_end_survives_width_reflow() {
        use super::*;
        use crate::app::ViewMode;
        use crate::document::DocumentState;
        use crate::session::{Geometry, RenderState, Session, Target};
        use egui::{pos2, vec2, CentralPanel, RawInput, Rect};
        use om_data::{Field, FieldKind, LayoutInfo, Record, RecordPage};
        let mut session = Session::new(
            Target {
                host: "synthetic.invalid".into(),
                file: "Reflow".into(),
                local: None,
            },
            25,
        );
        let key = session
            .set_layouts(vec![LayoutInfo {
                id: 7,
                name: "Reflow".into(),
            }])
            .unwrap();
        session
            .accept_page(
                key,
                Ok(RecordPage {
                    versions: Default::default(),
                    fields: (0..8)
                        .map(|i| Field {
                            name: format!("Field {i}"),
                            kind: FieldKind::Text,
                        })
                        .collect(),
                    records: vec![Record {
                        id: 1,
                        values: (0..8)
                            .map(|_| Some("wrapped text with words ".repeat(25)))
                            .collect(),
                    }],
                    total: 1,
                }),
            )
            .unwrap();
        session.geometry = Geometry::Unavailable;
        session.render = RenderState::Unavailable;
        // End and End/Page Up both need the new content height. The latter
        // also proves remeasurement independently of bottom anchoring.
        for choices in [&[2_u16][..], &[2, 3][..]] {
            let ctx = egui::Context::default();
            let mut document = DocumentState::default();
            let state = WindowState {
                layout: Some(7),
                ..Default::default()
            };
            document.scroll.context((&state).into(), state.zoom);
            let draw = |document: &mut DocumentState, width: f32| {
                ctx.run_ui(
                    RawInput {
                        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 360.0))),
                        ..Default::default()
                    },
                    |ui| {
                        CentralPanel::default().show(ui, |ui| {
                            crate::content::browse(
                                ui,
                                &session,
                                ViewMode::Form,
                                document,
                                None,
                                &mut Vec::new(),
                            )
                        });
                    },
                )
                .drop_without_applying_deltas();
                let m = document.scroll.metrics.as_ref().unwrap();
                let offset = egui::scroll_area::State::load(&ctx, m.id)
                    .unwrap()
                    .clamped_offset();
                (offset.y, m.height, m.viewport)
            };
            draw(&mut document, 1000.0);
            let wide = draw(&mut document, 1000.0);
            document.scroll.queue(
                choices
                    .iter()
                    .map(|&choice| ScrollRequest {
                        choice,
                        context: (&state).into(),
                        field: None,
                        field_focus: false,
                    })
                    .collect(),
                &state,
            );
            draw(&mut document, 400.0);
            draw(&mut document, 400.0);
            let narrow = draw(&mut document, 400.0);
            println!("actual generic form {choices:?}: wide={wide:?}, narrow={narrow:?}");
            assert!(narrow.1 > wide.1 + 100.0);
            let pages = if choices.last() == Some(&3) { 2.0 } else { 1.0 };
            assert!(
                (narrow.0 - (narrow.1 - pages * narrow.2).max(0.0)).abs() < 1.0,
                "scroll used old Form geometry"
            );
        }
    }

    #[test]
    fn scroll_window_end_follows_a_late_viewport_height_change_until_another_move() {
        use super::*;
        let ctx = egui::Context::default();
        let state = WindowState::default();
        let mut scroll = Scroll::default();
        scroll.context((&state).into(), state.zoom);
        let request = |choice| ScrollRequest {
            choice,
            context: (&state).into(),
            field: None,
            field_focus: false,
        };
        draw_height(&ctx, &mut scroll, false, 360.0);
        scroll.queue(vec![request(2)], &state);
        draw_height(&ctx, &mut scroll, false, 360.0);
        // Pause controls can appear one paint after the worker's End reply.
        let (offset, height, viewport) = draw_height(&ctx, &mut scroll, false, 320.0);
        assert!(
            (offset.y - (height - viewport)).abs() < 1.0,
            "late viewport shrink must leave the end visible: {offset:?}, {height}, {viewport}"
        );
        scroll.queue(vec![request(3)], &state);
        draw_height(&ctx, &mut scroll, false, 320.0);
        let moved = draw_height(&ctx, &mut scroll, false, 320.0).0.y;
        let after = draw_height(&ctx, &mut scroll, false, 280.0).0.y;
        assert!(
            (moved - after).abs() < 1.0,
            "Page Up releases the End anchor"
        );
    }
}

#[cfg(test)]
mod content_and_selection_tests {
    use super::*;
    use egui::{pos2, vec2, CentralPanel, RawInput, Rect, Sense};
    fn queue(scroll: &mut Scroll, state: &WindowState, choices: &[u16], field: Option<&str>) {
        scroll.queue(
            choices
                .iter()
                .map(|&choice| ScrollRequest {
                    choice,
                    context: state.into(),
                    field: field.map(str::to_owned),
                    field_focus: false,
                })
                .collect(),
            state,
        );
    }
    fn draw(
        ctx: &egui::Context,
        scroll: &mut Scroll,
        height: f32,
        content: f32,
        wheel: bool,
        fixed: bool,
    ) -> (f32, f32, f32) {
        scroll.start_fields();
        let mut result = None;
        let mut input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, height))),
            ..Default::default()
        };
        input
            .events
            .push(egui::Event::PointerMoved(pos2(250.0, 120.0)));
        if wheel {
            input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, 150.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            });
        }
        ctx.run_ui(input, |ui| {
            CentralPanel::default().show(ui, |ui| {
                if fixed {
                    scroll.field(
                        ui,
                        "Fixed",
                        Rect::from_min_size(ui.max_rect().min, vec2(120.0, 30.0)),
                    );
                }
                let frame = scroll.begin(ui, "additional", None);
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("additional")
                    .auto_shrink([false, false])
                    .stick_to_bottom(frame.at_end());
                if let Some(offset) = frame.offset() {
                    area = area.scroll_offset(offset);
                }
                let output = area.show(ui, |ui| {
                    scroll.origin(ui, ui.max_rect().min);
                    scroll.field(
                        ui,
                        "Visible",
                        Rect::from_min_size(
                            ui.max_rect().min + vec2(0.0, 1600.0),
                            vec2(120.0, 30.0),
                        ),
                    );
                    ui.allocate_exact_size(vec2(400.0, content), Sense::hover());
                    frame.hold(ui);
                });
                result = Some((
                    output.state.clamped_offset().y,
                    output.content_size.y,
                    output.inner_rect.height(),
                ));
                scroll.finish(frame, &output);
            });
        })
        .drop_without_applying_deltas();
        result.unwrap()
    }
    #[test]
    fn scroll_window_manual_wheel_releases_anchor_and_fixed_selection_preserves_position() {
        for target in ["Fixed", "Visible"] {
            let ctx = egui::Context::default();
            let state = WindowState::default();
            let mut scroll = Scroll::default();
            scroll.context((&state).into(), 100);
            draw(&ctx, &mut scroll, 360.0, 2000.0, false, true);
            queue(&mut scroll, &state, &[2], None);
            draw(&ctx, &mut scroll, 360.0, 2000.0, false, true);
            let end = draw(&ctx, &mut scroll, 360.0, 2000.0, false, true).0;
            draw(&ctx, &mut scroll, 360.0, 2000.0, true, true);
            for _ in 0..30 {
                draw(&ctx, &mut scroll, 360.0, 2000.0, false, true);
            }
            let moved = draw(&ctx, &mut scroll, 360.0, 2000.0, false, true).0;
            assert!(
                moved < end - 50.0,
                "wheel must actually move away from End: {end} -> {moved}"
            );
            let resized = draw(&ctx, &mut scroll, 280.0, 2000.0, false, true).0;
            assert!(
                (moved - resized).abs() < 1.0,
                "wheel-released anchor returned on resize: {moved} -> {resized}"
            );
            queue(&mut scroll, &state, &[5], Some(target));
            draw(&ctx, &mut scroll, 280.0, 2000.0, false, true);
            let selected = draw(&ctx, &mut scroll, 280.0, 2000.0, false, true).0;
            let after = draw(&ctx, &mut scroll, 240.0, 2000.0, false, true).0;
            assert!(
                (selected - resized).abs() < 1.0 && (after - selected).abs() < 1.0,
                "fixed selection reactivated End after wheel: {resized} -> {selected} -> {after}"
            );
        }
    }
    #[test]
    fn scroll_window_other_movements_release_anchor_on_content_and_height_changes() {
        for choice in [1, 3, 4] {
            let ctx = egui::Context::default();
            let state = WindowState::default();
            let mut scroll = Scroll::default();
            scroll.context((&state).into(), 100);
            draw(&ctx, &mut scroll, 360.0, 2000.0, false, false);
            queue(&mut scroll, &state, &[2], None);
            draw(&ctx, &mut scroll, 360.0, 2000.0, false, false);
            draw(&ctx, &mut scroll, 360.0, 2000.0, false, false);
            queue(&mut scroll, &state, &[choice], None);
            draw(&ctx, &mut scroll, 360.0, 2000.0, false, false);
            let before = draw(&ctx, &mut scroll, 360.0, 2000.0, false, false).0;
            let after = draw(&ctx, &mut scroll, 280.0, 2500.0, false, false).0;
            assert!(
                (before - after).abs() < 1.0,
                "choice {choice} must release End: {before} -> {after}"
            );
        }
    }
    fn page(repeats: usize) -> om_data::RecordPage {
        use om_data::{Field, FieldKind, Record, RecordPage};
        RecordPage {
            versions: Default::default(),
            fields: (0..8)
                .map(|i| Field {
                    name: format!("Field {i}"),
                    kind: FieldKind::Text,
                })
                .collect(),
            records: vec![Record {
                id: 1,
                values: (0..8)
                    .map(|_| Some("wrapped text with words ".repeat(repeats)))
                    .collect(),
            }],
            total: 1,
        }
    }
    fn paint_form(
        ctx: &egui::Context,
        doc: &mut crate::document::DocumentState,
        session: &crate::session::Session,
    ) {
        ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 360.0))),
                ..Default::default()
            },
            |ui| {
                CentralPanel::default().show(ui, |ui| {
                    crate::content::browse(
                        ui,
                        session,
                        crate::app::ViewMode::Form,
                        doc,
                        None,
                        &mut Vec::new(),
                    )
                });
            },
        )
        .drop_without_applying_deltas();
    }

    fn actual_form(
        ctx: &egui::Context,
        doc: &mut crate::document::DocumentState,
        session: &crate::session::Session,
    ) -> (f32, f32, f32) {
        paint_form(ctx, doc, session);
        let m = doc.scroll.metrics.as_ref().unwrap();
        let y = egui::scroll_area::State::load(ctx, m.id)
            .unwrap()
            .clamped_offset()
            .y;
        (y, m.height, m.viewport)
    }
    #[test]
    fn scroll_window_content_growth_end_page_up_uses_current_form_geometry() {
        use crate::session::{Geometry, RenderState, Session, Target};
        use om_data::LayoutInfo;
        for premeasure in [true, false] {
            let mut session = Session::new(
                Target {
                    host: "synthetic.invalid".into(),
                    file: "Content".into(),
                    local: None,
                },
                25,
            );
            let key = session
                .set_layouts(vec![LayoutInfo {
                    id: 7,
                    name: "Content".into(),
                }])
                .unwrap();
            session.accept_page(key, Ok(page(5))).unwrap();
            session.geometry = Geometry::Unavailable;
            session.render = RenderState::Unavailable;
            let ctx = egui::Context::default();
            let mut doc = crate::document::DocumentState::default();
            let state = WindowState {
                layout: Some(7),
                ..Default::default()
            };
            doc.scroll.context((&state).into(), 100);
            actual_form(&ctx, &mut doc, &session);
            let old = actual_form(&ctx, &mut doc, &session);
            let key = session.refresh().unwrap();
            session.accept_page(key, Ok(page(25))).unwrap();
            if premeasure {
                actual_form(&ctx, &mut doc, &session);
                actual_form(&ctx, &mut doc, &session);
            }
            queue(&mut doc.scroll, &state, &[2, 3], None);
            actual_form(&ctx, &mut doc, &session);
            actual_form(&ctx, &mut doc, &session);
            let new = actual_form(&ctx, &mut doc, &session);
            let expected = (new.1 - 2.0 * new.2).max(0.0);
            println!("content premeasure={premeasure} old={old:?} new={new:?} expected={expected}");
            assert!(new.1 > old.1 + 500.0, "fixture must grow actual content");
            assert!(
                (new.0 - expected).abs() < 1.0,
                "End/PageUp used stale Form content height after same-record refresh"
            );
        }
    }
    #[test]
    fn scroll_window_delayed_refresh_measures_the_accepted_page() {
        use crate::session::{Geometry, RenderState, Session, Target};
        use om_data::LayoutInfo;
        let mut session = Session::new(
            Target {
                host: "synthetic.invalid".into(),
                file: "Refresh".into(),
                local: None,
            },
            25,
        );
        let key = session
            .set_layouts(vec![LayoutInfo {
                id: 7,
                name: "Refresh".into(),
            }])
            .unwrap();
        session.accept_page(key, Ok(page(5))).unwrap();
        session.geometry = Geometry::Unavailable;
        session.render = RenderState::Unavailable;
        let ctx = egui::Context::default();
        let mut doc = crate::document::DocumentState::default();
        let state = WindowState {
            layout: Some(7),
            ..Default::default()
        };
        doc.scroll.context((&state).into(), 100);
        actual_form(&ctx, &mut doc, &session);
        let key = session.refresh().unwrap();
        queue(&mut doc.scroll, &state, &[2, 3], None);
        for _ in 0..4 {
            paint_form(&ctx, &mut doc, &session);
            assert_eq!(
                doc.scroll.pending.len(),
                2,
                "old retained page consumed the queued movements"
            );
        }
        session.accept_page(key, Ok(page(25))).unwrap();
        actual_form(&ctx, &mut doc, &session);
        actual_form(&ctx, &mut doc, &session);
        let (y, height, viewport) = actual_form(&ctx, &mut doc, &session);
        assert!(
            (y - (height - 2.0 * viewport).max(0.0)).abs() < 1.0,
            "delayed refresh used old dimensions: {y}, {height}, {viewport}"
        );
    }

    #[test]
    fn scroll_window_selection_keeps_queued_object_identity_after_remeasurement() {
        let ctx = egui::Context::default();
        let state = WindowState::default();
        let mut scroll = Scroll::default();
        scroll.context((&state).into(), 100);
        let first = Id::unique("first object");
        let second = Id::unique("second object");
        let draw = |scroll: &mut Scroll, objects: &[(Id, f32)]| {
            scroll.start_fields();
            let mut measured = None;
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 360.0))),
                    ..Default::default()
                },
                |ui| {
                    CentralPanel::default().show(ui, |ui| {
                        let frame = scroll.begin(ui, "objects", Some(0.0..=40.0));
                        let mut area = egui::ScrollArea::vertical()
                            .id_salt("objects")
                            .auto_shrink([false, false])
                            .stick_to_bottom(frame.at_end());
                        if let Some(offset) = frame.offset() {
                            area = area.scroll_offset(offset);
                        }
                        let output = area.show(ui, |ui| {
                            let origin = ui.max_rect().min;
                            scroll.origin(ui, origin);
                            for &(id, top) in objects {
                                scroll.field_with_id(
                                    ui,
                                    "Code",
                                    Rect::from_min_size(origin + vec2(0.0, top), vec2(100.0, 30.0)),
                                    id,
                                );
                            }
                            ui.allocate_exact_size(vec2(400.0, 2000.0), Sense::hover());
                            frame.hold(ui);
                        });
                        measured = Some(output.state.clamped_offset().y);
                        scroll.finish(frame, &output);
                    });
                },
            )
            .drop_without_applying_deltas();
            measured.unwrap()
        };
        draw(&mut scroll, &[(first, 50.0), (second, 1600.0)]);
        scroll.focused("Code", second);
        queue(&mut scroll, &state, &[5], Some("Code"));
        // A repaint discovers another focused widget while measuring new
        // geometry. It must not retarget the already queued selection.
        scroll.focused("Code", first);
        scroll.context((&state).into(), 125);
        draw(&mut scroll, &[(second, 1600.0), (first, 50.0)]);
        let y = draw(&mut scroll, &[(first, 50.0), (second, 1600.0)]);
        assert!(
            y > 1200.0,
            "selection followed field order/fresh focus instead of the queued object: {y}"
        );
        scroll.focused("Code", second);
        queue(&mut scroll, &state, &[5], Some("Code"));
        scroll.context((&state).into(), 150);
        draw(&mut scroll, &[(first, 50.0)]);
        let after = draw(&mut scroll, &[(first, 50.0)]);
        assert!(
            (after - y).abs() < 1.0,
            "a disappeared object redirected selection to another copy/body row: {y} -> {after}"
        );
        // A programmatic request starts without a widget. Once its field
        // receives focus in the measuring paint, later visibility changes
        // must not replace that resolved instance with a newly earlier one.
        scroll.context((&state).into(), 100);
        draw(&mut scroll, &[(second, 1600.0)]);
        scroll.queue(
            vec![ScrollRequest {
                choice: 5,
                context: (&state).into(),
                field: Some("Code".into()),
                field_focus: true,
            }],
            &state,
        );
        draw(&mut scroll, &[(second, 1600.0)]);
        scroll.resolve_focus("Code", second);
        scroll.calculations(Some(99));
        draw(&mut scroll, &[(first, 50.0), (second, 1600.0)]);
        let resolved = draw(&mut scroll, &[(first, 50.0), (second, 1600.0)]);
        assert!(
            resolved > 1200.0,
            "a newly visible duplicate replaced the resolved programmatic object: {resolved}"
        );
    }
    #[test]
    fn scroll_window_new_commands_measure_content_that_changed_without_a_page_reload() {
        let ctx = egui::Context::default();
        let state = WindowState::default();
        let mut scroll = Scroll::default();
        scroll.context((&state).into(), 100);
        draw(&ctx, &mut scroll, 360.0, 2000.0, false, false);
        queue(&mut scroll, &state, &[2, 3], None);
        // Calculated text and other dynamic layout content can grow while
        // the Session's loaded record page and the viewport stay the same.
        draw(&ctx, &mut scroll, 360.0, 3000.0, false, false);
        // Ordinary worker updates carry empty batches; they must not keep
        // postponing an already measured command.
        queue(&mut scroll, &state, &[], None);
        let (y, height, viewport) = draw(&ctx, &mut scroll, 360.0, 3000.0, false, false);
        assert!(
            (y - (height - 2.0 * viewport).max(0.0)).abs() < 1.0,
            "new commands used old geometry without a page reload: {y}, {height}, {viewport}"
        );
    }
    #[test]
    fn scroll_window_a_new_page_invalidates_already_measured_commands() {
        use crate::session::{Geometry, RenderState, Session, Target};
        use om_data::LayoutInfo;
        let mut session = Session::new(
            Target {
                host: "synthetic.invalid".into(),
                file: "Replaced".into(),
                local: None,
            },
            25,
        );
        let key = session
            .set_layouts(vec![LayoutInfo {
                id: 7,
                name: "Replaced".into(),
            }])
            .unwrap();
        session.accept_page(key, Ok(page(5))).unwrap();
        session.geometry = Geometry::Unavailable;
        session.render = RenderState::Unavailable;
        let ctx = egui::Context::default();
        let mut doc = crate::document::DocumentState::default();
        let state = WindowState {
            layout: Some(7),
            ..Default::default()
        };
        doc.scroll.context((&state).into(), 100);
        actual_form(&ctx, &mut doc, &session);
        queue(&mut doc.scroll, &state, &[2, 3], None);
        actual_form(&ctx, &mut doc, &session);
        assert_eq!(
            doc.scroll.pending.len(),
            2,
            "new commands must first measure their content"
        );
        // A reload can complete between paints, with no loading frame.
        let key = session.refresh().unwrap();
        session.accept_page(key, Ok(page(25))).unwrap();
        actual_form(&ctx, &mut doc, &session);
        actual_form(&ctx, &mut doc, &session);
        let (y, height, viewport) = actual_form(&ctx, &mut doc, &session);
        assert!((y-(height-2.0*viewport).max(0.0)).abs()<1.0,
            "accepted data did not replace the pending commands' old measurement: {y}, {height}, {viewport}");
    }
    #[test]
    fn scroll_window_pending_field_focus_waits_for_the_current_record() {
        use crate::session::{Geometry, RenderState, Session, Target};
        use om_data::LayoutInfo;
        let mut session = Session::new(
            Target {
                host: "synthetic.invalid".into(),
                file: "Focus".into(),
                local: None,
            },
            25,
        );
        let key = session
            .set_layouts(vec![LayoutInfo {
                id: 7,
                name: "Focus".into(),
            }])
            .unwrap();
        session.accept_page(key, Ok(page(5))).unwrap();
        session.geometry = Geometry::Unavailable;
        session.render = RenderState::Unavailable;
        let ctx = egui::Context::default();
        let mut doc = crate::document::DocumentState::default();
        doc.capabilities.update = true;
        let state = WindowState {
            layout: Some(7),
            focus: Some("Field 0".into()),
            field_focus: true,
            ..Default::default()
        };
        doc.scroll.context((&state).into(), 100);
        paint_form(&ctx, &mut doc, &session);
        let key = session.refresh().unwrap();
        doc.focus = state.focus.clone();
        doc.scroll.queue(
            vec![ScrollRequest {
                choice: 5,
                context: (&state).into(),
                field: state.focus.clone(),
                field_focus: true,
            }],
            &state,
        );
        for _ in 0..3 {
            paint_form(&ctx, &mut doc, &session);
            assert_eq!(
                doc.focus.as_deref(),
                Some("Field 0"),
                "old retained record consumed the new focus request"
            );
        }
        let mut replacement = page(5);
        replacement.records[0].id = 2;
        session.accept_page(key, Ok(replacement)).unwrap();
        paint_form(&ctx, &mut doc, &session);
        paint_form(&ctx, &mut doc, &session);
        assert!(doc.focus.is_none());
        assert!(doc.scroll.pending.is_empty());
        let editor = doc
            .editor_scope
            .get()
            .unwrap()
            .with((7u64, 2i64, "Field 0"));
        assert!(
            ctx.memory(|memory| memory.has_focus(editor)),
            "focus belongs to the accepted record, not the retained old one"
        );
    }
}

#[cfg(test)]
mod calculation_geometry_tests {
    use super::*;
    use crate::{
        app::ViewMode,
        calc::{Calculations, Evaluator},
        document::DocumentState,
        session::{Geometry, RenderState, Session, Target},
    };
    use egui::{pos2, vec2, CentralPanel, RawInput, Rect};
    use om_data::calc::eval::Variables;
    use om_data::calc::{Locale, Value, ValueType};
    use om_data::host::{CalcData, CalcNeed, CalcSchema, FieldDef, RecordValues, SessionInfo};
    use om_data::{Field, FieldKind, LayoutInfo, Record, RecordPage};
    use std::sync::Arc;
    fn global(calcs: &mut Calculations, repeats: usize) {
        let mut values = RecordValues::default();
        values.set(
            3,
            1,
            Value::text("late asynchronous global value ".repeat(repeats)),
        );
        let mut data = CalcData::default();
        data.globals.insert(10, Arc::new(values));
        assert!(calcs
            .accept_data(&[CalcNeed::Globals { table: 10 }], Ok(data))
            .is_none());
    }
    fn record(calcs: &mut Calculations) {
        let mut values = RecordValues::default();
        values.set(1, 1, Value::text("First"));
        values.set(2, 1, Value::text("Second"));
        let mut data = CalcData::default();
        data.records.insert((10, 1), Some(Arc::new(values)));
        assert!(calcs
            .accept_data(
                &[CalcNeed::Record {
                    table: 10,
                    record: 1
                }],
                Ok(data)
            )
            .is_none());
    }
    #[test]
    fn scroll_window_late_calculation_data_remeasures_pending_commands() {
        for draft in [false, true] {
            let mut schema = CalcSchema::new("Synthetic", Locale::en_us());
            schema.add_table(10, "Data");
            schema.add_occurrence(20, "Data", 10);
            schema.add_layout(7, "Form", Some(20));
            for i in 1..=3 {
                schema.add_field(
                    10,
                    FieldDef::data(i, format!("Field {i}"), ValueType::Text).with_global(i == 3),
                );
            }
            let mut calcs = Calculations::default();
            calcs.set_schema(Some(Arc::new(schema)));
            global(&mut calcs, 1);
            record(&mut calcs);
            let mut session = Session::new(
                Target {
                    host: "late.invalid".into(),
                    file: "Synthetic".into(),
                    local: None,
                },
                25,
            );
            let k = session
                .set_layouts(vec![LayoutInfo {
                    id: 7,
                    name: "Form".into(),
                }])
                .unwrap();
            session
                .accept_page(
                    k,
                    Ok(RecordPage {
                        versions: Default::default(),
                        fields: (1..=3)
                            .map(|i| Field {
                                name: format!("Field {i}"),
                                kind: FieldKind::Text,
                            })
                            .collect(),
                        records: vec![Record {
                            id: 1,
                            values: vec![Some("First".into()), Some("Second".into()), None],
                        }],
                        total: 1,
                    }),
                )
                .unwrap();
            session.geometry = Geometry::Unavailable;
            session.render = RenderState::Unavailable;
            let w = WindowState {
                layout: Some(7),
                ..Default::default()
            };
            let mut doc = DocumentState::default();
            if draft {
                doc.put_draft(7, 1, "Field 1", "First".into(), "Draft".into());
            }
            doc.scroll.context((&w).into(), 100);
            let ctx = egui::Context::default();
            let paint = |doc: &mut DocumentState, calcs: &Calculations| {
                assert_eq!(
                    crate::calc::draft_overlay(
                        calcs.schema().unwrap(),
                        &calcs.cache.borrow(),
                        &session,
                        &doc.changes(&session),
                        20,
                        om_data::host::session::today(0),
                    )
                    .is_some(),
                    draft,
                    "the draft case must exercise a real cached-record overlay"
                );
                let eval = Evaluator::new(
                    calcs,
                    &session,
                    doc,
                    SessionInfo::default(),
                    &Variables::new(),
                )
                .unwrap();
                ctx.run_ui(
                    RawInput {
                        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(650.0, 360.0))),
                        ..Default::default()
                    },
                    |ui| {
                        CentralPanel::default().show(ui, |ui| {
                            crate::content::browse(
                                ui,
                                &session,
                                ViewMode::Form,
                                doc,
                                Some(&eval),
                                &mut Vec::new(),
                            )
                        });
                    },
                )
                .drop_without_applying_deltas();
                let m = doc.scroll.metrics.as_ref().unwrap();
                (
                    egui::scroll_area::State::load(&ctx, m.id)
                        .unwrap()
                        .clamped_offset()
                        .y,
                    m.height,
                    m.viewport,
                )
            };
            paint(&mut doc, &calcs);
            let short = paint(&mut doc, &calcs);
            let mut selected = None;
            // Select a real wrapped height whose eventual Page Up destination still
            // leaves the calculated field's top on screen, avoiding its inherited
            // offscreen calculation omission from confounding this lifecycle probe.
            for repeats in 20..150 {
                global(&mut calcs, repeats);
                let x = paint(&mut doc, &calcs);
                if x.1 > 2.0 * x.2 + 45.0 && x.1 < 2.0 * x.2 + 75.0 {
                    selected = Some((repeats, x));
                    break;
                }
            }
            let (repeats, tall) = selected.expect("fixture can measure a suitable dynamic height");
            calcs.data_changed();
            record(&mut calcs);
            paint(&mut doc, &calcs);
            doc.scroll.queue(
                [2, 3]
                    .map(|choice| ScrollRequest {
                        choice,
                        context: (&w).into(),
                        field: None,
                        field_focus: false,
                    })
                    .into(),
                &w,
            );
            paint(&mut doc, &calcs);
            assert_eq!(doc.scroll.pending.len(), 2);
            assert!(
                calcs
                    .take_wanted()
                    .contains(&CalcNeed::Globals { table: 10 }),
                "measuring paint must issue a real asynchronous globals need"
            );
            global(&mut calcs, repeats);
            let after = paint(&mut doc, &calcs);
            let settled = paint(&mut doc, &calcs);
            let expected = (after.1 - 2.0 * after.2).max(0.0);
            println!("late CalcData repeats={repeats} short={short:?} tall={tall:?} after={after:?} settled={settled:?} expected={expected}");
            assert_eq!(session.page.as_ref().unwrap().key, k);
            assert!((after.1 - tall.1).abs() < 1.0);
            assert!(expected > 15.0);
            assert!((settled.0-expected).abs()<1.0,"late CalcData changed current Form content after measurement, but the batch consumed the earlier height");
        }
    }
}

#[cfg(test)]
mod keyboard_draft_scroll {
    use super::*;
    use crate::{
        app::ViewMode,
        calc::{Calculations, Evaluator},
        document::DocumentState,
        session::{Geometry, RenderState, Session, Target},
    };
    use egui::{pos2, vec2, CentralPanel, RawInput, Rect};
    use om_data::calc::eval::Variables;
    use om_data::calc::{Locale, Value, ValueType};
    use om_data::formula::{self, Formula};
    use om_data::host::{CalcData, CalcNeed, CalcSchema, FieldDef, RecordValues, SessionInfo};
    use om_data::{Field, FieldKind, LayoutInfo, Record, RecordPage};
    use std::sync::Arc;
    #[test]
    fn scroll_window_keyboard_draft_change_uses_new_height() {
        let mut schema = CalcSchema::new("Synthetic", Locale::en_us());
        schema.add_table(10, "Data");
        schema.add_occurrence(20, "Data", 10);
        schema.add_layout(7, "Form", Some(20));
        for i in 1..=2 {
            schema.add_field(10, FieldDef::data(i, format!("Field {i}"), ValueType::Text));
        }
        let cx = formula::Context::new(schema.names());
        let code = Formula {
            context: None,
            formatted: None,
            polish: formula::compile("Data::Field 1", &cx).unwrap().expr,
        };
        schema.add_field(
            10,
            FieldDef::calculation(3, "Field 3", ValueType::Text, code, false),
        );
        let mut calcs = Calculations::default();
        calcs.set_schema(Some(Arc::new(schema)));
        let mut values = RecordValues::default();
        values.set(1, 1, Value::text("First"));
        values.set(2, 1, Value::text("Second"));
        let mut data = CalcData::default();
        data.records.insert((10, 1), Some(Arc::new(values)));
        assert!(calcs
            .accept_data(
                &[CalcNeed::Record {
                    table: 10,
                    record: 1
                }],
                Ok(data)
            )
            .is_none());
        let mut session = Session::new(
            Target {
                host: "draft.invalid".into(),
                file: "Synthetic".into(),
                local: None,
            },
            25,
        );
        let k = session
            .set_layouts(vec![LayoutInfo {
                id: 7,
                name: "Form".into(),
            }])
            .unwrap();
        session
            .accept_page(
                k,
                Ok(RecordPage {
                    versions: Default::default(),
                    fields: (1..=3)
                        .map(|i| Field {
                            name: format!("Field {i}"),
                            kind: FieldKind::Text,
                        })
                        .collect(),
                    records: vec![Record {
                        id: 1,
                        values: vec![Some("First".into()), Some("Second".into()), None],
                    }],
                    total: 1,
                }),
            )
            .unwrap();
        session.geometry = Geometry::Unavailable;
        session.render = RenderState::Unavailable;
        let w = WindowState {
            layout: Some(7),
            ..Default::default()
        };
        let mut doc = DocumentState::default();
        doc.capabilities.update = true;
        doc.put_draft(7, 1, "Field 1", "First".into(), "short".into());
        doc.scroll.context((&w).into(), 100);
        let ctx = egui::Context::default();
        let paint = |doc: &mut DocumentState, calcs: &Calculations, text: Option<String>| {
            assert!(crate::calc::draft_overlay(
                calcs.schema().unwrap(),
                &calcs.cache.borrow(),
                &session,
                &doc.changes(&session),
                20,
                om_data::host::session::today(0)
            )
            .is_some());
            let eval = Evaluator::new(
                calcs,
                &session,
                doc,
                SessionInfo::default(),
                &Variables::new(),
            )
            .unwrap();
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(650.0, 360.0))),
                    events: text.map(|t| vec![egui::Event::Text(t)]).unwrap_or_default(),
                    ..Default::default()
                },
                |ui| {
                    CentralPanel::default().show(ui, |ui| {
                        crate::content::browse(
                            ui,
                            &session,
                            ViewMode::Form,
                            doc,
                            Some(&eval),
                            &mut Vec::new(),
                        )
                    });
                },
            )
            .drop_without_applying_deltas();
            let m = doc.scroll.metrics.as_ref().unwrap();
            (
                egui::scroll_area::State::load(&ctx, m.id)
                    .unwrap()
                    .clamped_offset()
                    .y,
                m.height,
                m.viewport,
            )
        };
        paint(&mut doc, &calcs, None);
        let short = paint(&mut doc, &calcs, None);
        let mut selected = None;
        for repeats in 20..150 {
            let long = "late keyboard draft value ".repeat(repeats);
            doc.put_draft(7, 1, "Field 1", "First".into(), long.clone());
            let x = paint(&mut doc, &calcs, None);
            if x.1 > 2.0 * x.2 + 45.0 && x.1 < 2.0 * x.2 + 75.0 {
                selected = Some((long, x));
                break;
            }
        }
        let (long, tall) =
            selected.expect("fixture must grow a visible calculated field to suitable height");
        doc.put_draft(7, 1, "Field 1", "First".into(), "short".into());
        doc.focus = Some("Field 1".into());
        doc.select_all = true;
        paint(&mut doc, &calcs, None);
        paint(&mut doc, &calcs, None);
        let editor = doc
            .editor_scope
            .get()
            .unwrap()
            .with((7u64, 1i64, "Field 1"));
        assert!(ctx.memory(|m| m.has_focus(editor)));
        doc.scroll.queue(
            [2, 3]
                .map(|choice| ScrollRequest {
                    choice,
                    context: (&w).into(),
                    field: None,
                    field_focus: false,
                })
                .into(),
            &w,
        );
        let measuring = paint(&mut doc, &calcs, Some(long.clone()));
        assert_eq!(doc.scroll.pending.len(), 2);
        assert_eq!(doc.changes(&session).get("Field 1"), Some(&Some(long)));
        let after = paint(&mut doc, &calcs, None);
        let settled = paint(&mut doc, &calcs, None);
        let expected = (after.1 - 2.0 * after.2).max(0.0);
        println!("keyboard draft short={short:?} tall={tall:?} measuring={measuring:?} after={after:?} settled={settled:?} expected={expected}");
        assert_eq!(session.page.as_ref().unwrap().key, k);
        assert!((after.1 - tall.1).abs() < 1.0);
        assert!(expected > 15.0);
        assert!(
            (settled.0 - expected).abs() < 1.0,
            "changed real keyboard draft consumed stale calculation geometry"
        );
    }
}
