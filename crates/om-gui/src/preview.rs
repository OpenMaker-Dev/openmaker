//! Read-only physical report pages. Data is requested through Session's normal
//! epoch/revision-tagged ranges; no record is selected, committed or written.
use crate::{
    app::Action,
    calc::Evaluator,
    document::zoom,
    report,
    session::{RenderState, Session},
    theme,
};
use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui};
use om_layout::{Color, ObjectKind, ResolvedStyle};

/// Why Preview refuses a layout whose records the set gives no access.
const MASKED: &str = "your access privileges give this layout's records no access";

pub(crate) fn show(
    ui: &mut Ui,
    session: &Session,
    calc: Option<&Evaluator<'_>>,
    panels: &std::collections::BTreeMap<(u64, u64), u64>,
    zoom: zoom::Zoom,
    scroll_state: &mut crate::document::scroll::Scroll,
    actions: &mut Vec<Action>,
) {
    ui.painter()
        .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::PRINT_WORKSPACE);
    // Sub-summary groups are read once per found set, not per paint; data
    // still being fetched leaves the plan loading.
    // A layout whose records the set gives no access is not previewed,
    // its groups and pagination not worked out either: they follow its
    // records' values [I: unmeasured in FileMaker Pro, #707].
    let masked = calc.is_some_and(Evaluator::layout_masked) || session.layout_masked();
    if let (Some(total), RenderState::Loaded(document)) =
        (session.report_groups_wanted(), &session.render)
    {
        match calc {
            _ if masked => session.accept_report_groups(total, Err(MASKED.into())),
            None => session.accept_report_groups(
                total,
                Err("This provider has no report calculation context".into()),
            ),
            Some(calc) => {
                if let Some(groups) = calc.report_groups(&document.layout, total) {
                    session.accept_report_groups(total, groups.map_err(|e| e.message));
                }
            }
        }
    }
    // A report whose parts resize is measured once per found set and data
    // generation, over its records' values; pending values keep it loading.
    if let (Some((total, found, paper)), RenderState::Loaded(document)) =
        (session.report_measure_wanted(), &session.render)
    {
        match calc {
            _ if masked => session.accept_report_plan(total, Err(MASKED.into())),
            None => session.accept_report_plan(
                total,
                Err("This provider has no report calculation context".into()),
            ),
            Some(calc) => {
                let shown = (panels.iter())
                    .filter(|((layout, _), _)| *layout == document.layout.id)
                    .map(|((_, control), panel)| (*control, *panel))
                    .collect();
                if let Some(plan) = calc.report_plan(&document.layout, paper, &found, &shown) {
                    session.accept_report_plan(total, plan.map_err(|e| e.message));
                }
            }
        }
    }
    let plan = match session.report_plan() {
        Ok(Some(plan)) => plan,
        Ok(None) => {
            ui.label("Loading report…");
            return;
        }
        Err(message) => {
            ui.label(format!("Preview unavailable: {message}"));
            return;
        }
    };
    let page = session.preview_position().unwrap().0 as usize;
    let RenderState::Loaded(document) = &session.render else {
        return;
    };
    let mut missing = false;
    for placement in &plan.pages[page] {
        if let Some(error) = session.preview_record_error(placement.record as u64) {
            ui.label(format!(
                "Preview unavailable: report records could not be loaded: {error}"
            ));
            return;
        }
        match session.record_at(placement.record as u64) {
            None => {
                actions.push(Action::LoadRecordRange(placement.record as u64));
                missing = true;
            }
            // Groups were read for this found set's records, in order.
            Some((_, record))
                if session
                    .report_group_record(placement.record as u64)
                    .is_some_and(|id| i64::try_from(id).ok() != Some(record.id)) =>
            {
                ui.label("Preview unavailable: the found set changed while grouping the report");
                return;
            }
            Some(_) => {}
        }
    }
    if missing {
        ui.label("Loading report records…");
        return;
    }
    let pictures = session.report_pictures();
    // A picture drawn apart asks for the repaint that draws its page.
    pictures.on_drawn(|| {
        let ctx = ui.ctx().clone();
        std::sync::Arc::new(move || ctx.request_repaint())
    });
    let scene = report::prepare_page(
        &document.layout,
        &plan,
        page,
        pictures,
        &mut Content {
            session,
            calc,
            panels: (panels.iter())
                .filter(|((layout, _), _)| *layout == document.layout.id)
                .map(|((_, control), panel)| (*control, *panel))
                .collect(),
        },
    );
    let scene = match scene {
        Ok(scene) => scene,
        Err(error) => {
            // Pending: a picture being drawn apart, whose drawing asks for
            // the repaint (`on_drawn`), or data on its way.
            ui.label(if error.code == -1 {
                error.message
            } else {
                format!("Preview unavailable: {}", error.message)
            });
            return;
        }
    };
    // PDF and Print embed any decoded picture; a display texture is bounded.
    let max_side = ui.ctx().input(|i| i.max_texture_side);
    if scene
        .parts
        .iter()
        .flat_map(|part| &part.objects)
        .filter_map(|object| object.picture.as_ref())
        .any(|picture| picture.width.max(picture.height) as usize > max_side)
    {
        ui.label(format!(
            "Preview unavailable: a report picture is larger than this display's \
             {max_side}-pixel texture limit"
        ));
        return;
    }
    // A new physical page starts at its top. Wheel/scrollbar motion stays on
    // that page, and zoom scales the paper without scaling application chrome.
    scroll_state.begin_paint(session);
    scroll_state.calculations(calc.map(Evaluator::content_revision));
    let salt = ("physical_preview", session.preview_identity());
    let frame = scroll_state.begin(ui, salt, None);
    let mut scroll = egui::ScrollArea::both()
        .id_salt(salt)
        .auto_shrink([false, false]);
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let (draw, magnify) = drawn(plan.paper.scale);
    let gutter = 16.0 / magnify;
    let output = scroll.stick_to_bottom(frame.at_end()).show(ui, |ui| {
        zoom::show(
            ui,
            vec2(
                plan.paper.width as f32 * draw + 2.0 * gutter,
                plan.paper.height as f32 * draw + 2.0 * gutter,
            ),
            zoom.factor() * magnify,
            |ui| paint(ui, &scene, &plan.paper),
        );
        frame.hold(ui);
    });
    scroll_state.finish(frame, &output);
}

/// The dashed printable-area guide Preview draws on the page. It sits on
/// the white paper, not on a theme surface, so no line token fits.
const MARGIN_GUIDE: Color32 = Color32::GRAY;

fn color(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(
        (c.red * 255.0) as u8,
        (c.green * 255.0) as u8,
        (c.blue * 255.0) as u8,
        (c.alpha * 255.0) as u8,
    )
}
/// How Preview shows a page at Page Setup's `scale`: the scene's layout
/// points drawn times the first, and that drawing magnified by the second
/// with the zoom. Up to 100% the scene is drawn on the sheet, so text is
/// rasterized at the size it prints; above it, at its own size and
/// magnified, as zooming in does, since egui drops glyphs rasterized
/// past 1,024 pixels.
fn drawn(scale: f64) -> (f32, f32) {
    let scale = scale as f32;
    if scale > 1.0 {
        (1.0, scale)
    } else {
        (scale, 1.0)
    }
}

/// `r`, in the page's layout points, drawn `draw` times its size inside
/// the gutter (`gutter` drawn points).
fn sheet_rect(r: &om_layout::Rect, draw: f32, gutter: f32) -> Rect {
    Rect::from_min_max(
        pos2(r.left as f32 * draw + gutter, r.top as f32 * draw + gutter),
        pos2(
            r.right as f32 * draw + gutter,
            r.bottom as f32 * draw + gutter,
        ),
    )
}
fn black() -> Color {
    Color {
        alpha: 1.0,
        ..Default::default()
    }
}
/// An object's or part's fill and borders, as the PDF draws them. A rounded
/// frame (`radius`, fitted to the frame as the PDF fits it) fills its
/// rounded shape and draws its one border inside the bounds: the scene was
/// checked by the PDF writer first, so every side of a rounded frame has
/// the same border. egui rounds corner radii to whole points, as Browse's
/// frames do. Widths and `radius` are in layout points, drawn at `scale`.
fn box_style(painter: &egui::Painter, r: Rect, style: &ResolvedStyle, radius: f64, scale: f32) {
    let radius = (radius * f64::from(scale))
        .min(f64::from(r.width()) / 2.0)
        .min(f64::from(r.height()) / 2.0)
        .max(0.0);
    if radius > 0.0 {
        let width = style.border_width[0].unwrap_or(0.0) as f32 * scale;
        let stroke = if width > 0.0 && !matches!(style.border_style[0], Some(0 | 1)) {
            Stroke::new(width, color(style.border_color[0].unwrap_or_else(black)))
        } else {
            Stroke::NONE
        };
        painter.rect(
            r,
            CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8),
            style.background.map_or(Color32::TRANSPARENT, color),
            stroke,
            StrokeKind::Inside,
        );
        return;
    }
    if let Some(c) = style.background {
        painter.rect_filled(r, CornerRadius::ZERO, color(c));
    }
    for side in 0..4 {
        let width = style.border_width[side].unwrap_or(0.0) as f32 * scale;
        if width <= 0.0 || matches!(style.border_style[side], Some(0 | 1)) {
            continue;
        }
        let r = r.shrink(width / 2.0);
        let points = match side {
            0 => [r.left_top(), r.right_top()],
            1 => [r.right_top(), r.right_bottom()],
            2 => [r.right_bottom(), r.left_bottom()],
            _ => [r.left_bottom(), r.left_top()],
        };
        painter.line_segment(
            points,
            Stroke::new(width, color(style.border_color[side].unwrap_or_else(black))),
        );
    }
}

/// Preview's report content: the window's evaluator over the loaded
/// records, which never waits for the provider.
struct Content<'s, 'c, 'e> {
    session: &'s Session,
    calc: Option<&'c Evaluator<'e>>,
    /// The panel each tab control shows in the window, by control.
    panels: std::collections::BTreeMap<u64, u64>,
}

impl Content<'_, '_, '_> {
    /// The record's id, and the evaluator.
    fn record(
        &self,
        context: report::ContentContext,
    ) -> Result<(u64, &Evaluator<'_>), report::ReportError> {
        let (_, record) = self
            .session
            .record_at(context.record as u64)
            .ok_or_else(|| report::ReportError::unsupported("A report record is unavailable"))?;
        if record.id <= 0 {
            return Err(report::ReportError::unsupported(
                "The report record identity is unavailable",
            ));
        }
        let calc = self.calc.ok_or_else(|| {
            report::ReportError::unsupported("This provider has no report calculation context")
        })?;
        // A layout whose records the set gives no access is not previewed
        // [I: unmeasured in FileMaker Pro, #707].
        if calc.layout_masked() || self.session.layout_masked() {
            return Err(report::ReportError::unsupported(MASKED));
        }
        Ok((record.id as u64, calc))
    }
}

impl report::ReportContent for Content<'_, '_, '_> {
    fn text(
        &mut self,
        object: &om_layout::LayoutObject,
        context: report::ContentContext,
    ) -> Result<String, report::ReportError> {
        // A static label (a text object's or a button's) needs no record,
        // unless it holds merge variables or symbols.
        if matches!(object.kind(), ObjectKind::Text | ObjectKind::Button)
            && object.text_runs.is_empty()
            && !report::merges(object.text.as_deref().unwrap_or_default())
        {
            self.session
                .record_at(context.record as u64)
                .ok_or_else(|| {
                    report::ReportError::unsupported("A report record is unavailable")
                })?;
            return Ok(object.text.clone().unwrap_or_default());
        }
        let (record, calc) = self.record(context)?;
        calc.report_text_in(
            object,
            record,
            context.page as u64,
            context.group,
            context.portal,
        )
    }

    fn container(
        &mut self,
        object: &om_layout::LayoutObject,
        context: report::ContentContext,
    ) -> Result<Option<std::sync::Arc<om_data::calc::eval::StoredContainer>>, report::ReportError>
    {
        if object.kind() != ObjectKind::Field {
            return Ok(None);
        }
        let (record, calc) = self.record(context)?;
        calc.report_container(object, record, context.portal)
    }

    fn conditions(
        &mut self,
        object: &om_layout::LayoutObject,
        context: report::ContentContext,
    ) -> Result<Option<Vec<usize>>, report::ReportError> {
        let (record, calc) = self.record(context)?;
        calc.report_conditions(
            object,
            record,
            context.page as u64,
            context.group,
            context.portal,
        )
        .map(Some)
    }

    fn front_panel(&mut self, control: &om_layout::LayoutObject) -> Option<u64> {
        self.panels.get(&control.id).copied()
    }

    fn portal_records(
        &mut self,
        portal: &om_layout::LayoutObject,
        context: report::ContentContext,
    ) -> Result<Option<Vec<u64>>, report::ReportError> {
        let (record, calc) = self.record(context)?;
        // Unknown while its records are fetched, as a field's value is.
        let records = calc
            .portal_records(portal, record)
            .ok_or(report::ReportError {
                code: -1,
                message: "Loading report values…".into(),
            })?;
        Ok(Some(records.to_vec()))
    }
}

/// The scene's decoded samples, uploaded once. Like Browse's image loader,
/// egui premultiplies the straight alpha here, exactly once.
fn picture_texture<'a>(
    ctx: &egui::Context,
    picture: &'a report::Picture,
) -> &'a egui::TextureHandle {
    picture.texture.get_or_init(|| {
        ctx.load_texture(
            "report picture",
            egui::ColorImage::from_rgba_unmultiplied(
                [picture.width as usize, picture.height as usize],
                &picture.rgba,
            ),
            egui::TextureOptions::default(),
        )
    })
}

pub(crate) fn paint(
    ui: &mut Ui,
    scene: &report::ReportPage,
    paper: &om_layout::pagination::PageSize,
) {
    // The scene is in the page's layout points, drawn at Page Setup's
    // scale ([`drawn`]); the gutter and the guide look the same at every
    // scale.
    let (scale, magnify) = drawn(paper.scale);
    let gutter = 16.0 / magnify;
    let rect = |r: &om_layout::Rect| sheet_rect(r, scale, gutter);
    let painter = ui.painter().clone();
    painter.rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::PRINT_WORKSPACE);
    let page = Rect::from_min_size(
        pos2(gutter, gutter),
        vec2(paper.width as f32 * scale, paper.height as f32 * scale),
    );
    painter.rect_filled(page, CornerRadius::ZERO, Color32::WHITE);
    for part in &scene.parts {
        let painter = painter.with_clip_rect(rect(&part.bounds).intersect(ui.clip_rect()));
        box_style(&painter, rect(&part.bounds), &part.style, 0.0, scale);
        for object in &part.objects {
            let r = rect(&object.bounds);
            match object.kind {
                ObjectKind::Line => {
                    // The endpoints are corners of the bounds (`line_endpoints`).
                    let b = &object.bounds;
                    let corner = |(x, y): (f64, f64)| {
                        pos2(
                            if x == b.left { r.left() } else { r.right() },
                            if y == b.top { r.top() } else { r.bottom() },
                        )
                    };
                    let ends = object
                        .line
                        .map(|ends| ends.map(corner))
                        .unwrap_or([r.left_top(), r.right_bottom()]);
                    painter.line_segment(
                        ends,
                        Stroke::new(
                            object.style.border_width[0].unwrap_or(1.0) as f32 * scale,
                            color(
                                object.style.border_color[0]
                                    .or(object.style.color)
                                    .unwrap_or_else(black),
                            ),
                        ),
                    );
                }
                ObjectKind::Graphic => {
                    box_style(&painter, r, &object.style, object.radius, scale);
                    if let Some(picture) = &object.picture {
                        painter.image(
                            picture_texture(ui.ctx(), picture).id(),
                            r,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                ObjectKind::Oval => {
                    painter.add(egui::epaint::EllipseShape {
                        center: r.center(),
                        angle: 0.0,
                        radius: r.size() / 2.0,
                        fill: object.style.background.map_or(Color32::TRANSPARENT, color),
                        stroke: if object.style.background.is_some_and(|c| c.alpha != 0.0) {
                            Stroke::NONE
                        } else {
                            Stroke::new(scale, Color32::BLACK)
                        },
                    });
                }
                _ => {
                    box_style(&painter, r, &object.style, object.radius, scale);
                    if let (Some(picture), Some([clip, image])) = (&object.picture, &object.placed)
                    {
                        painter
                            .with_clip_rect(rect(clip).intersect(painter.clip_rect()))
                            .image(
                                picture_texture(ui.ctx(), picture).id(),
                                rect(image),
                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                    }
                    if let Some(icon) = &object.file_icon {
                        crate::icons::paint(
                            &painter,
                            rect(icon),
                            crate::icons::Icon::File,
                            color(object.style.color.unwrap_or_else(black)),
                            Color32::TRANSPARENT,
                        );
                    }
                    if let Some((icon, square)) = &object.icon {
                        painter.image(
                            picture_texture(ui.ctx(), icon).id(),
                            rect(square),
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    for line in &object.text {
                        // Overflowing field text is cut at its box.
                        let painter = match &line.clip {
                            Some(clip) => {
                                painter.with_clip_rect(rect(clip).intersect(painter.clip_rect()))
                            }
                            None => painter.clone(),
                        };
                        let font = egui::FontId::new(
                            line.size as f32 * scale,
                            theme::report_family(line.face),
                        );
                        // Use shared measured advances and baselines; egui only
                        // rasterizes the bundled font glyphs at those positions,
                        // each part of the line in its color.
                        for (glyphs, part_color, start) in line.parts() {
                            let mut x = start as f32 * scale + gutter;
                            for (_, advance, ch) in glyphs {
                                let galley = painter.layout_no_wrap(
                                    ch.to_string(),
                                    font.clone(),
                                    color(part_color),
                                );
                                let baseline = galley
                                    .rows
                                    .first()
                                    .and_then(|r| r.glyphs.first().map(|g| r.pos.y + g.pos.y))
                                    .unwrap_or(0.0);
                                painter.galley(
                                    pos2(x, line.baseline as f32 * scale + gutter - baseline),
                                    galley,
                                    color(part_color),
                                );
                                x += *advance as f32 * scale;
                            }
                        }
                        // Underlines and strikethroughs, in the line's clip.
                        for (decoration, decoration_color) in line.decorations() {
                            painter.rect_filled(
                                rect(&decoration),
                                CornerRadius::ZERO,
                                color(decoration_color),
                            );
                        }
                        // One accessible label per physical line, without any
                        // field editor or interaction in the read-only scene.
                        let label: String = line.glyphs.iter().map(|g| g.2).collect();
                        let response = ui.allocate_rect(r, Sense::hover());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Label, true, &label)
                        });
                    }
                }
            }
        }
    }
    let m = &paper.margins;
    let printable = Rect::from_min_max(
        page.min + vec2(m.left as f32, m.top as f32) * scale,
        page.max - vec2(m.right as f32, m.bottom as f32) * scale,
    );
    for [a, b] in [
        [printable.left_top(), printable.right_top()],
        [printable.right_top(), printable.right_bottom()],
        [printable.right_bottom(), printable.left_bottom()],
        [printable.left_bottom(), printable.left_top()],
    ] {
        painter.add(egui::Shape::dashed_line(
            &[a, b],
            Stroke::new(0.7 / magnify, MARGIN_GUIDE),
            2.0 / magnify,
            3.0 / magnify,
        ));
    }
}
