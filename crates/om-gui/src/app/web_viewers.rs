//! The windows' web viewers (`crate::web_viewer`): after a window is drawn
//! its painted web viewers are reconciled with their native views, the
//! worker's Set Web Viewer and Perform JavaScript requests go to the view of
//! the window they name, and a page's `FileMaker.PerformScript` starts a
//! script in the window that owns the view, only while that window still
//! shows the view's layout in Browse.

use super::{Mode, OpenMakerApp};
use crate::scripting::{Answer, Start};
use crate::web_viewer::{self, Command, NativeParent, Outcome, Unavailable, ViewKey, WindowFrame};

impl OpenMakerApp {
    /// Why web viewers show no page in the window about to be drawn.
    pub(super) fn web_viewer_note(&mut self, parent: Result<NativeParent, Unavailable>) {
        let reason = self.web.unavailable(parent);
        self.w.document.web_note = reason.map(web_viewer::unavailable_note);
        // Interactive containers' audio and video play in the same engine.
        self.w.document.media_note = reason.map(web_viewer::media_note);
        // The pages Browse paints itself, with their newest pictures.
        let layout = self.w.session.as_ref().and_then(|s| s.layout);
        self.w.document.web_pages =
            self.web
                .pages(self.w.id, &self.w.uuid, self.w.file_id, self.epoch, layout);
    }

    /// Reconcile the web viewers the window painted this frame with their
    /// views, after everything egui draws over the document was drawn. In
    /// Browse's Table view only the parts its Table View Properties
    /// include paint any (`document::show_table`).
    pub(super) fn reconcile_web_viewers(
        &mut self,
        ctx: &egui::Context,
        parent: Result<NativeParent, Unavailable>,
    ) {
        let browse =
            self.w.mode == Mode::Browse && !self.w.hidden && self.w.connection_lost.is_none();
        let layout = self
            .w
            .session
            .as_ref()
            .and_then(|s| s.layout)
            .filter(|_| browse);
        let placements = std::mem::take(&mut self.w.document.web_viewers);
        // The pages' pop-ups over everything the window drew.
        self.web.paint_popups(ctx, self.w.id);
        let covered = covered_areas(ctx, self.viewport_layer, &self.w.document.overlays);
        self.web.reconcile(
            ctx,
            &WindowFrame {
                window: self.w.id,
                uuid: &self.w.uuid,
                file: self.w.file_id,
                epoch: self.epoch,
                layout,
                parent: parent.ok(),
                pixels_per_point: ctx.pixels_per_point(),
                covered: &covered,
                placements: &placements,
                pressed: ctx.input(|i| i.pointer.any_pressed()),
            },
        );
    }

    /// Views of windows that closed (or of an earlier session) go.
    pub(super) fn retain_web_viewers(&mut self) {
        let live: Vec<(u32, String)> = std::iter::once(&self.w)
            .chain(&self.windows)
            .filter(|d| !self.closed_windows.contains(&d.id) && d.connection_lost.is_none())
            .map(|d| (d.id, d.uuid.clone()))
            .collect();
        self.web.retain_windows(self.epoch, &live);
    }

    /// The worker's request for a web viewer of the window it works in.
    pub(super) fn web_viewer_command(&mut self, ctx: &egui::Context, id: u64, command: Command) {
        let key = self.window_key();
        self.web.command(id, &key, command);
        self.web_viewer_outcomes(ctx);
        ctx.request_repaint();
    }

    fn window_key(&self) -> ViewKey {
        ViewKey {
            window: self.w.id,
            uuid: self.w.uuid.clone(),
            file: self.w.file_id,
            epoch: self.epoch,
            layout: self.w.session.as_ref().and_then(|s| s.layout).unwrap_or(0),
            object: 0,
            record: None,
        }
    }

    /// Answer the worker and start the scripts pages asked for.
    pub(super) fn web_viewer_outcomes(&mut self, ctx: &egui::Context) {
        self.web.frames(ctx);
        for outcome in self.web.poll() {
            match outcome {
                Outcome::Answer { prompt, result } => {
                    self.answer(ctx, prompt, Answer::WebViewer(result));
                }
                Outcome::PerformScript { key, call } => {
                    if key.epoch != self.epoch || self.closed_windows.contains(&key.window) {
                        continue;
                    }
                    self.with_window(key.window, |app| {
                        let current = app.w.uuid == key.uuid
                            && app.w.file_id == key.file
                            && app.w.mode == Mode::Browse
                            && app.w.connection_lost.is_none()
                            && app.w.session.as_ref().and_then(|s| s.layout) == Some(key.layout);
                        if current {
                            app.start_script_now(
                                ctx,
                                Start::WebViewer {
                                    window: key.window,
                                    call,
                                },
                            );
                        }
                    });
                }
            }
        }
        if self.web.is_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

/// Whatever egui draws above the document in the viewport being drawn: its
/// windows, menus, pop-ups and tooltips, a modal's backdrop, and the
/// layout's open popovers (`overlays`), in window points. In the root
/// viewport that is every layer above the background order the panels and
/// the document paint in. An embedded viewport's panels and document paint
/// in its egui window's layer (`host`) and its sublayers: there it is the
/// layers egui paints after those, as of the last frame, such as another
/// embedded window raised above it.
fn covered_areas(
    ctx: &egui::Context,
    host: egui::LayerId,
    overlays: &[egui::Rect],
) -> Vec<egui::Rect> {
    let (modal, layers) = ctx.memory(|memory| {
        let areas = memory.areas();
        let visible = areas.visible_layer_ids();
        let above: Vec<egui::LayerId> = if host.order == egui::Order::Background {
            visible
                .into_iter()
                .filter(|layer| layer.order != egui::Order::Background)
                .collect()
        } else {
            memory
                .layer_ids()
                .skip_while(|layer| *layer != host)
                .skip(1)
                .filter(|layer| visible.contains(layer) && areas.parent_layer(*layer) != Some(host))
                .collect()
        };
        let layers: Vec<(egui::LayerId, egui::Rect)> = above
            .into_iter()
            .filter_map(|layer| Some((layer, memory.area_rect(layer.id)?)))
            .collect();
        (memory.top_modal_layer().is_some(), layers)
    });
    let mut covered = overlays.to_vec();
    if modal {
        covered.push(egui::Rect::EVERYTHING);
    }
    for (layer, rect) in layers {
        covered.push(
            ctx.layer_transform_to_global(layer)
                .map_or(rect, |transform| transform * rect),
        );
    }
    covered
}
