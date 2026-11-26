//! Document zoom uses its own transformed layer. The application menus and
//! toolbars retain their size, while layout painting, hit testing and clips
//! share the same logical coordinates.

use egui::{
    emath::TSTransform, AreaState, Context, InnerResponse, LayerId, Pos2, Rect, Sense, Ui,
    UiBuilder, Vec2,
};

const STEPS: [u16; 8] = [25, 50, 75, 100, 150, 200, 300, 400];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Zoom {
    step: usize,
}

impl Default for Zoom {
    fn default() -> Self {
        Self { step: 3 }
    }
}

impl Zoom {
    pub fn factor(self) -> f32 {
        f32::from(self.percent()) / 100.0
    }

    pub fn set_percent(&mut self, percent: u16) {
        if let Some(step) = STEPS.iter().position(|value| *value == percent) {
            self.step = step;
        }
    }

    pub fn percent(self) -> u16 {
        STEPS[self.step]
    }

    pub fn zoom_in(&mut self) {
        self.step = (self.step + 1).min(STEPS.len() - 1);
    }

    pub fn zoom_out(&mut self) {
        self.step = self.step.saturating_sub(1);
    }

    pub fn actual_size(&mut self) {
        *self = Self::default();
    }
}

/// Place this inside the document's outer scroll area. Its allocation is
/// scaled; the closure sees a zero-origin, unscaled layout. egui transforms
/// input together with the layer, as it does for its Scene container.
pub(crate) fn show<R>(
    ui: &mut Ui,
    logical_size: Vec2,
    factor: f32,
    paint: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    // Preview multiplies the zoom (25% to 400%) by Page Setup's scale above
    // 100% (up to 1000%, `om_layout::paper::SCALES`).
    let factor = if factor.is_finite() && factor > 0.0 {
        factor.clamp(0.25, 40.0)
    } else {
        1.0
    };
    let finite = |value: f32| {
        if value.is_finite() {
            value.max(0.0)
        } else {
            0.0
        }
    };
    let logical_size = Vec2::new(finite(logical_size.x), finite(logical_size.y));
    let (outer, response) = ui.allocate_exact_size(logical_size * factor, Sense::hover());
    let to_parent =
        TSTransform::from_translation(outer.min.to_vec2()) * TSTransform::from_scaling(factor);
    let parent_to_global = ui
        .ctx()
        .layer_transform_to_global(ui.layer_id())
        .unwrap_or(TSTransform::IDENTITY);
    let layer = LayerId::new(ui.layer_id().order, ui.scope_id().with("document_zoom"));
    ui.ctx().set_sublayer(ui.layer_id(), layer);
    ui.ctx()
        .set_transform_layer(layer, parent_to_global * to_parent);
    let mut local = ui.new_child(
        UiBuilder::new()
            .layer_id(layer)
            .max_rect(Rect::from_min_size(Pos2::ZERO, logical_size)),
    );
    let visible = to_parent.inverse() * outer.intersect(ui.clip_rect());
    local.set_clip_rect(visible);
    local.set_min_size(logical_size);
    // Layer transforms cover paint and pointer input, but egui 0.37 leaves
    // accessibility bounds in widget coordinates. Put the same transform
    // on this container so assistive technology targets painted positions.
    local
        .ctx()
        .accesskit_node_builder(local.unique_id(), |node| {
            node.set_transform(egui::accesskit::Affine::new([
                f64::from(factor),
                0.0,
                0.0,
                f64::from(factor),
                f64::from(outer.min.x),
                f64::from(outer.min.y),
            ]));
        });
    // egui gives the wheel only to scroll areas in the layer it finds under
    // the pointer, and it finds a layer by its area. While the layout paints,
    // the document's visible part is this layer's area, so its scroll areas,
    // such as portals, take the wheel under the pointer. Only where the
    // document's own layer is on top, though (this layer's area contains no
    // point yet, so it is not in the way): a menu, popup or window over the
    // document keeps the wheel, also in this layer's first pass, while egui
    // still orders the new layer above every other one.
    let on_document = ui
        .ctx()
        .input(|input| input.pointer.interact_pos())
        .is_some_and(|pointer| ui.ctx().layer_id_at(pointer) == Some(ui.layer_id()));
    set_pointer_area(ui.ctx(), layer, on_document.then_some(visible));
    let inner = paint(&mut local);
    // The document's scroll area takes the wheel after its content, in the
    // parent layer. Giving the pointer back to that layer lets it scroll by
    // what the layout's scroll areas leave, as nested scroll areas in one
    // layer chain.
    set_pointer_area(ui.ctx(), layer, None);
    InnerResponse { inner, response }
}

/// Where egui finds `layer` under the pointer, in the layer's coordinates, or
/// nowhere. Clicks and hover are hit tested by widget instead, so the layer
/// stays interactable either way.
fn set_pointer_area(ctx: &Context, layer: LayerId, rect: Option<Rect>) {
    // A negative size contains no point.
    let (min, size) = rect.map_or((Pos2::ZERO, Vec2::splat(-1.0)), |rect| {
        (rect.min, rect.size())
    });
    ctx.memory_mut(|memory| {
        memory.areas_mut().set_state(
            layer,
            AreaState {
                pivot_pos: Some(min),
                size: Some(size),
                ..AreaState::default()
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2, Color32, PointerButton};

    #[test]
    fn native_zoom_steps_clamp_and_reset_to_actual_size() {
        let mut zoom = Zoom::default();
        assert_eq!((zoom.percent(), zoom.factor()), (100, 1.0));
        for expected in [150, 200, 300, 400, 400] {
            zoom.zoom_in();
            assert_eq!(zoom.percent(), expected);
        }
        for expected in [300, 200, 150, 100, 75, 50, 25, 25] {
            zoom.zoom_out();
            assert_eq!(zoom.percent(), expected);
        }
        zoom.actual_size();
        assert_eq!(zoom.percent(), 100);
    }

    #[test]
    fn zoom_scales_extent_and_inverse_transforms_the_parent_clip() {
        let context = egui::Context::default();
        let mut observed = None;
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(200.0, 150.0));
                let mut parent = ui.new_child(UiBuilder::new().max_rect(rect));
                parent.set_clip_rect(Rect::from_min_max(pos2(35.0, 55.0), pos2(170.0, 145.0)));
                let result = show(&mut parent, vec2(200.0, 100.0), 1.5, |ui| {
                    assert_eq!(
                        ui.max_rect(),
                        Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0))
                    );
                    ui.painter().rect_filled(ui.max_rect(), 0, Color32::BLUE);
                    ui.clip_rect()
                });
                observed = Some((result.response.rect, result.inner));
            })
            .drop_without_applying_deltas();
        let (extent, clip) = observed.unwrap();
        assert_eq!(
            extent,
            Rect::from_min_size(pos2(20.0, 40.0), vec2(300.0, 150.0))
        );
        assert!((clip.min - pos2(10.0, 10.0)).length() < 0.001);
        assert!((clip.max - pos2(100.0, 70.0)).length() < 0.001);
        assert_eq!(context.zoom_factor(), 1.0);
    }

    #[test]
    fn zoomed_controls_hit_at_their_painted_position() {
        let context = egui::Context::default();
        let logical_button = Rect::from_min_size(pos2(20.0, 10.0), vec2(30.0, 20.0));
        let mut clicked = false;
        for frame in 0..3 {
            let mut input = egui::RawInput::default();
            let point = pos2(30.0, 40.0) + logical_button.center().to_vec2() * 1.5;
            input.events.push(egui::Event::PointerMoved(point));
            if frame > 0 {
                input.events.push(egui::Event::PointerButton {
                    pos: point,
                    button: PointerButton::Primary,
                    pressed: frame == 1,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            context
                .run_ui(input, |ui| {
                    let rect = Rect::from_min_size(pos2(30.0, 40.0), vec2(300.0, 200.0));
                    let mut parent = ui.new_child(UiBuilder::new().max_rect(rect));
                    show(&mut parent, vec2(200.0, 100.0), 1.5, |ui| {
                        let response = ui.interact(
                            logical_button,
                            ui.scope_id().with("button"),
                            Sense::click(),
                        );
                        clicked |= response.clicked();
                    });
                })
                .drop_without_applying_deltas();
        }
        assert!(clicked);
        assert_eq!(
            context.zoom_factor(),
            1.0,
            "toolbar/global scale must stay unchanged"
        );
    }

    #[test]
    fn outer_scroll_area_uses_the_scaled_document_extent() {
        let context = egui::Context::default();
        let mut content_size = Vec2::ZERO;
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let mut parent = ui.new_child(
                    UiBuilder::new().max_rect(Rect::from_min_size(Pos2::ZERO, vec2(200.0, 150.0))),
                );
                let scroll = egui::ScrollArea::both().show(&mut parent, |ui| {
                    show(ui, vec2(300.0, 200.0), 2.0, |_| {});
                });
                content_size = scroll.content_size;
            })
            .drop_without_applying_deltas();
        assert_eq!(content_size, vec2(600.0, 400.0));
    }

    #[test]
    fn accessibility_clicks_use_the_transformed_document_bounds() {
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{cell::Cell, rc::Rc};

        let clicked = Rc::new(Cell::new(false));
        let observed = Rc::clone(&clicked);
        let logical_button = Rect::from_min_size(pos2(20.0, 10.0), vec2(100.0, 30.0));
        let mut app = Harness::builder()
            .with_size(vec2(500.0, 400.0))
            .build_ui(move |ui| {
                let parent_rect = Rect::from_min_size(pos2(30.0, 40.0), vec2(400.0, 300.0));
                let mut parent = ui.new_child(UiBuilder::new().max_rect(parent_rect));
                show(&mut parent, vec2(200.0, 150.0), 1.5, |ui| {
                    if ui
                        .put(logical_button, egui::Button::new("Zoom target"))
                        .clicked()
                    {
                        observed.set(true);
                    }
                });
            });
        let rect = app.get_by_label("Zoom target").rect();
        let expected = pos2(30.0, 40.0) + logical_button.center().to_vec2() * 1.5;
        assert!((rect.center() - expected).length() < 0.01);
        app.get_by_label("Zoom target").click();
        app.run_steps(3);
        assert!(clicked.get());
    }

    #[test]
    fn zoomed_scroll_areas_take_the_wheel_under_the_pointer_and_the_document_the_rest() {
        let context = egui::Context::default();
        let mut time = 0.0;
        // Returns the inner (portal) and outer (document) scroll offsets
        // after wheeling at `pointer`, in screen points.
        let mut wheel = |pointer: Pos2, delta: f32| {
            let mut offsets = (0.0, 0.0);
            for frame in 0..20 {
                time += 0.1;
                let mut input = egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                };
                input.events.push(egui::Event::PointerMoved(pointer));
                if frame == 1 {
                    input.events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, delta),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                context
                    .run_ui(input, |ui| {
                        let viewport = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 200.0));
                        let mut parent = ui.new_child(UiBuilder::new().max_rect(viewport));
                        let document = egui::ScrollArea::vertical()
                            .id_salt("document")
                            .auto_shrink(false)
                            .show(&mut parent, |ui| {
                                show(ui, vec2(150.0, 600.0), 2.0, |ui| {
                                    let portal =
                                        Rect::from_min_size(pos2(20.0, 20.0), vec2(100.0, 60.0));
                                    let mut child = ui.new_child(UiBuilder::new().max_rect(portal));
                                    egui::ScrollArea::vertical()
                                        .id_salt("portal")
                                        .auto_shrink(false)
                                        .show(&mut child, |ui| {
                                            ui.allocate_space(vec2(100.0, 1000.0));
                                        })
                                        .state
                                        .clamped_offset()
                                        .y
                                })
                                .inner
                            });
                        offsets = (document.inner, document.state.clamped_offset().y);
                    })
                    .drop_without_applying_deltas();
            }
            offsets
        };
        // The portal is at (40, 40)-(240, 160) on screen until the document
        // scrolls.
        let (portal, document) = wheel(pos2(100.0, 100.0), -100.0);
        assert!(portal > 40.0, "the portal under the pointer scrolls");
        assert_eq!(document, 0.0, "the document stays");
        let (portal_after, document) = wheel(pos2(270.0, 100.0), -50.0);
        assert_eq!(portal_after, portal, "the portal stays");
        assert!(
            document > 40.0,
            "the wheel beside the portal scrolls the document"
        );
        // The portal is now `document` points higher. Up past its first row,
        // the rest scrolls the document back.
        let (portal, document_after) = wheel(pos2(100.0, 140.0 - document), 500.0);
        assert_eq!(portal, 0.0);
        assert!(
            document_after < document,
            "the portal's end chains to the document"
        );
    }

    #[test]
    fn a_menu_open_before_the_document_first_paints_keeps_the_wheel() {
        let context = egui::Context::default();
        let menu_id = egui::Id::unique("menu over the document");
        // Over the zoomed portal, (40, 40)-(240, 160) on screen, and the menu.
        let pointer = pos2(100.0, 100.0);
        let (mut portal, mut menu) = (0.0, 0.0);
        for frame in 0..6 {
            let mut input = egui::RawInput {
                time: Some(f64::from(frame) * 0.1),
                ..Default::default()
            };
            input.events.push(egui::Event::PointerMoved(pointer));
            // The document's first frame. A wheel step this small all
            // arrives in it, before egui orders the new zoom layer.
            if frame == 3 {
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, -5.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            context
                .run_ui(input, |ui| {
                    if frame >= 3 {
                        let viewport = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 200.0));
                        let mut parent = ui.new_child(UiBuilder::new().max_rect(viewport));
                        egui::ScrollArea::vertical()
                            .id_salt("document")
                            .auto_shrink(false)
                            .show(&mut parent, |ui| {
                                show(ui, vec2(150.0, 600.0), 2.0, |ui| {
                                    let rect =
                                        Rect::from_min_size(pos2(20.0, 20.0), vec2(100.0, 60.0));
                                    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
                                    portal = egui::ScrollArea::vertical()
                                        .id_salt("portal")
                                        .auto_shrink(false)
                                        .show(&mut child, |ui| {
                                            ui.allocate_space(vec2(100.0, 1000.0));
                                        })
                                        .state
                                        .clamped_offset()
                                        .y;
                                });
                            });
                    }
                    // Open over the document since before it first painted.
                    egui::Area::new(menu_id)
                        .order(egui::Order::Foreground)
                        .fixed_pos(pos2(60.0, 60.0))
                        .show(ui.ctx(), |ui| {
                            menu = egui::ScrollArea::vertical()
                                .id_salt("menu")
                                .max_height(80.0)
                                .show(ui, |ui| {
                                    ui.allocate_space(vec2(120.0, 400.0));
                                })
                                .state
                                .clamped_offset()
                                .y;
                        });
                })
                .drop_without_applying_deltas();
        }
        assert_eq!(portal, 0.0, "the portal under the menu must not scroll");
        assert!(menu > 0.0, "the menu keeps the wheel");
    }
}
