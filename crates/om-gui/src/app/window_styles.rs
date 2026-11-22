//! New Window's Card and Dialog styles in the desktop client (Claris,
//! "About window styles"; the worker's rules are in
//! `scripting/host/windows.rs`).
//!
//! - A card has no viewport of its own: it is drawn over its parent's
//!   layout area, in the parent's viewport and on a layer of its own above
//!   the parent's layout, without a title bar, frame or status toolbar,
//!   with a close box when its options have one. The
//!   parent takes no input while the card is open (its layout area dimmed
//!   with Dim parent window); the viewport's menu bar and keyboard work on
//!   the card. Other windows stay usable. Closing the card makes its
//!   parent the front window; closing the parent closes the card first.
//! - A dialog has a viewport of its own, kept in front. While one is open
//!   every other window takes no input, and a click there brings the
//!   dialog forward; closing it returns to the window it was opened from.
//!
//! Each window keeps its own [`Doc`](super::windows::Doc): what a card's
//! controls do applies to the card's window and record, never its
//! parent's, and a reply for a card that has closed goes nowhere.

use egui::{Color32, CornerRadius, Rect, Stroke, Ui};

use super::{Action, OpenMakerApp};
use crate::desktop::{card_frame, Placement};
use crate::theme;
use om_data::host::Frame;

/// How a window that shares its viewport or waits behind another is drawn
/// ([`OpenMakerApp::draw_window_with`]).
pub(super) struct Sharing {
    /// Its menu bar shows (a card's parent's viewport shows the card's).
    pub menu: bool,
    /// A card over it or a dialog in front: it takes no input, and its web
    /// viewers' native views hide.
    pub covered: bool,
    /// What the user asked of it in the viewport's menu bar.
    pub actions: Vec<Action>,
}

impl Default for Sharing {
    fn default() -> Self {
        Sharing {
            menu: true,
            covered: false,
            actions: Vec::new(),
        }
    }
}

type NativeParent = Result<crate::web_viewer::NativeParent, crate::web_viewer::Unavailable>;

/// The darkening of a card's parent's layout area (Dim parent window).
const DIM: Color32 = Color32::from_black_alpha(96);

/// The card's close box.
const CLOSE_BOX: f32 = 18.0;

/// An open card as the window last drew it ([`OpenMakerApp::cards`]), in
/// points of the viewport it is drawn in.
#[derive(Clone, Debug, PartialEq)]
pub struct CardFrame {
    /// The card window's name.
    pub name: String,
    /// The card, without frame or title bar.
    pub frame: Rect,
    /// Its parent's layout area, which it dims when `dims`.
    pub parent_area: Rect,
    pub dims: bool,
}

impl OpenMakerApp {
    pub(super) fn docs(&self) -> impl Iterator<Item = &super::windows::Doc> {
        std::iter::once(&self.w).chain(&self.windows)
    }

    /// The cards open on the app's windows, where they were last drawn;
    /// for tests and measures that read what the window paints there.
    pub fn cards(&self) -> Vec<CardFrame> {
        self.docs()
            .filter_map(|doc| {
                let card = doc.style.card()?;
                let parent = self.docs().find(|p| p.id == card.parent)?;
                Some(CardFrame {
                    name: doc.title.clone().unwrap_or_default(),
                    frame: doc.content_rect?,
                    parent_area: parent.content_rect?,
                    dims: card.dim,
                })
            })
            .collect()
    }

    pub(super) fn is_card(&self, id: u32) -> bool {
        self.docs().any(|d| d.id == id && d.style.card().is_some())
    }

    /// The card open on window `id`.
    pub(super) fn card_of(&self, id: u32) -> Option<u32> {
        self.docs()
            .find(|d| d.style.card().is_some_and(|c| c.parent == id))
            .map(|d| d.id)
    }

    /// The window a card is drawn on, through cards on cards: the one
    /// with a viewport.
    pub(super) fn base_window(&self, mut id: u32) -> u32 {
        for _ in 0..64 {
            let parent = self
                .docs()
                .find(|d| d.id == id)
                .and_then(|d| d.style.card())
                .map(|c| c.parent);
            match parent {
                Some(parent) if self.docs().any(|d| d.id == parent) => id = parent,
                _ => break,
            }
        }
        id
    }

    /// Window `id` and the cards over it, the top one last.
    pub(super) fn card_chain(&self, id: u32) -> Vec<u32> {
        let mut chain = vec![id];
        while let Some(card) = self.card_of(*chain.last().expect("one window")) {
            if chain.contains(&card) || chain.len() > 64 {
                break;
            }
            chain.push(card);
        }
        chain
    }

    /// The dialog that keeps window `id` from taking input: the dialog in
    /// front, when `id` is another window and not one of its cards.
    pub(super) fn blocking_dialog(&self, id: u32) -> Option<u32> {
        let dialog = self
            .window_ids()
            .into_iter()
            .find(|w| self.docs().any(|d| d.id == *w && d.style.is_dialog()))?;
        (self.base_window(id) != dialog).then_some(dialog)
    }

    /// Where the window the app works on is, as its viewport reports it:
    /// a card's frame is its own area on its parent.
    pub(super) fn window_placement(&self, ctx: &egui::Context) -> Placement {
        let mut placement = crate::desktop::placement(
            ctx,
            self.w.content_rect,
            self.w.show_toolbar,
            self.w.bars,
            self.w.menu_drawn,
            self.w.menu_height,
        );
        if self.w.style.card().is_some() {
            placement.frame = placement.content;
            placement.inner = placement.content;
        }
        placement
    }

    /// Draw window `id` and its cards in `ui`, its viewport, whose native
    /// window is `parent`.
    pub(super) fn draw_viewport(
        &mut self,
        ui: &mut Ui,
        id: u32,
        embedded: bool,
        parent: NativeParent,
    ) {
        let chain = self.card_chain(id);
        let top = *chain.last().expect("one window");
        let dialog = self.blocking_dialog(id);
        if !embedded
            && dialog.is_none()
            && matches!(
                self.presentation,
                Some(super::windows::Presentation::Window { pending: false, .. })
            )
        {
            let focus_elsewhere = matches!(self.presentation,
                Some(super::windows::Presentation::Window { id: target, pending: false, .. })
                    if self.base_window(target) != id);
            let user_input = ui.input(|i| {
                i.pointer.any_pressed()
                    || i.events.iter().any(|e| {
                        matches!(e, egui::Event::Key { pressed: true, .. })
                            || focus_elsewhere && matches!(e, egui::Event::WindowFocused(true))
                    })
            });
            if user_input {
                self.presentation = Some(super::windows::Presentation::User);
                if !self.bring_to_front(top) {
                    let viewport = self.viewport_of(self.w.id);
                    ui.ctx()
                        .send_viewport_cmd_to(viewport, egui::ViewportCommand::Focus);
                }
            }
        }
        if let Some(dialog) = dialog {
            // A click on a window behind an open dialog brings the dialog
            // forward.
            if !embedded && ui.input(|i| i.pointer.any_pressed()) {
                let viewport = self.viewport_of(dialog);
                ui.ctx()
                    .send_viewport_cmd_to(viewport, egui::ViewportCommand::Focus);
            }
        }
        // A previous viewport, or script presentation handling above, can
        // have changed which file's dialogs belong in this frame.
        let front = self.front_doc().id;
        let front_here = chain.contains(&front);
        let input = dialog.is_none() && (!embedded || front_here);
        if chain.len() == 1 {
            let draw = |app: &mut Self, ui: &mut Ui| {
                let sharing = Sharing {
                    covered: dialog.is_some(),
                    ..Sharing::default()
                };
                app.with_window(id, |app| {
                    app.draw_window_with(ui, input, front_here, parent, sharing)
                });
            };
            if dialog.is_some() {
                ui.scope(|ui| {
                    ui.disable();
                    draw(self, ui);
                });
            } else {
                draw(self, ui);
            }
            return;
        }
        // The viewport's menu bar works on the card in front. A choice made
        // in the system menu bar while a window under it was in front cannot
        // reach that window now: it goes, rather than run once the card
        // closes.
        #[cfg(target_os = "macos")]
        if let Some(bar) = &mut self.system_menu {
            for &covered in chain.iter().filter(|&&window| window != top) {
                bar.drop_choices_for((self.epoch, covered));
            }
        }
        let mut menu = Vec::new();
        let system = self.system_menu_bar();
        if system || self.with_window(id, |app| app.w.menubar).unwrap_or(false) {
            self.with_window(top, |app| {
                if dialog.is_some() {
                    ui.disable();
                }
                app.menu_bar(ui, input, &mut menu);
            });
        }
        // The parent and any card under the top one take no input; they
        // look as they are (a dialog in front fades them).
        ui.scope(|ui| {
            if dialog.is_none() {
                ui.style_mut().visuals.disabled_alpha = 1.0;
            }
            ui.disable();
            self.with_window(id, |app| {
                let sharing = Sharing {
                    menu: false,
                    covered: true,
                    actions: Vec::new(),
                };
                app.draw_window_with(ui, false, false, parent, sharing);
            });
        });
        let native = parent;
        for pair in chain.windows(2) {
            let (parent_window, card) = (pair[0], pair[1]);
            let actions = if card == top {
                std::mem::take(&mut menu)
            } else {
                Vec::new()
            };
            let card_input = input && card == top;
            let card_front = front_here && card == top;
            let blocked = dialog.is_some() || card != top;
            let sharing = Sharing {
                menu: false,
                covered: blocked,
                actions,
            };
            self.draw_card(
                ui,
                (parent_window, card),
                (card_input, card_front),
                native,
                sharing,
            );
        }
    }

    /// Card `card` over window `parent`'s layout area: the area dimmed when
    /// the card asks, then the card with its close box.
    fn draw_card(
        &mut self,
        ui: &mut Ui,
        (parent, card): (u32, u32),
        (input, front): (bool, bool),
        native: NativeParent,
        sharing: Sharing,
    ) {
        let blocked = sharing.covered;
        let ctx = ui.ctx().clone();
        let area = self
            .with_window(parent, |app| app.w.content_rect)
            .flatten()
            .unwrap_or_else(|| ui.max_rect());
        let Some(Some(options)) = self.with_window(card, |app| app.w.style.card().copied()) else {
            return;
        };
        // The dimming and the card paint on a layer of their own, above the
        // viewport's. Each document paints on a sublayer of the layer it is
        // shown in (`document::zoom`), and egui paints sublayers above that
        // layer, among themselves in no set order: on the viewport's layer
        // the parent's layout covered the dimming and the card, and the
        // card's layout could go under its parent's (#865). Over the
        // parent's whole layout area, the layer also takes the pointer
        // there, which the parent does not while the card is open.
        let layer = egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::unique(("openmaker-card-layer", card)),
        );
        if let Some(transform) = ctx.layer_transform_to_global(ui.layer_id()) {
            ctx.set_transform_layer(layer, transform);
        }
        ctx.memory_mut(|memory| {
            memory.areas_mut().set_state(
                layer,
                egui::AreaState {
                    pivot_pos: Some(area.min),
                    pivot: egui::Align2::LEFT_TOP,
                    size: Some(area.size()),
                    interactable: true,
                    last_became_visible_at: None,
                },
            );
        });
        // A parent drawn in an embedded window (an egui window of the same
        // order) goes above the card when the user raises it: the card goes
        // back over it.
        let host = ui.layer_id();
        if host.order == layer.order {
            let behind = ctx.memory(|memory| {
                let order: Vec<_> = memory.layer_ids().collect();
                let at = |id| order.iter().position(|l| *l == id);
                at(layer) < at(host)
            });
            if behind {
                ctx.move_to_top(layer);
            }
        }
        // Within its parent's layout area: a card larger than that is cut
        // to it.
        let ui = &mut ui.new_child(egui::UiBuilder::new().layer_id(layer).max_rect(area));
        if options.dim {
            ui.painter().rect_filled(area, CornerRadius::ZERO, DIM);
        }
        let frame = card_frame(
            Frame::new(
                f64::from(area.left()),
                f64::from(area.top()),
                f64::from(area.width()),
                f64::from(area.height()),
            ),
            &options,
        );
        let rect = Rect::from_min_size(
            egui::pos2(frame.left as f32, frame.top as f32),
            egui::vec2(frame.width as f32, frame.height as f32),
        )
        .intersect(ui.max_rect());
        let shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(70),
        };
        ui.painter()
            .add(shadow.as_shape(rect, CornerRadius::same(4)));
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), theme::CONTENT_BG);
        let mut close = false;
        // Ids of its own: the card shows the same widgets (scroll areas)
        // as its parent, in the same viewport.
        let builder = egui::UiBuilder::new()
            .max_rect(rect)
            .id_salt(("openmaker-card", card));
        ui.scope_builder(builder, |ui| {
            ui.set_clip_rect(rect);
            if blocked {
                ui.style_mut().visuals.disabled_alpha = 1.0;
                ui.disable();
            }
            self.with_window(card, |app| {
                app.draw_window_with(ui, input, front, native, sharing);
            });
        });
        if options.close {
            // On a layer of its own, above the card's: the card's layout
            // paints on a sublayer of the card's layer (`document::zoom`),
            // which egui paints above it, so a Table view's header or a
            // layout's canvas there would cover the close box and take
            // its click.
            let top = egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::unique(("openmaker-card-close-layer", card)),
            );
            if let Some(transform) = ctx.layer_transform_to_global(layer) {
                ctx.set_transform_layer(top, transform);
            }
            let button = close_box_rect(rect);
            ctx.memory_mut(|memory| {
                memory.areas_mut().set_state(
                    top,
                    egui::AreaState {
                        pivot_pos: Some(button.min),
                        pivot: egui::Align2::LEFT_TOP,
                        size: Some(button.size()),
                        interactable: true,
                        last_became_visible_at: None,
                    },
                );
            });
            // Keep the close box above this card's layout and below any
            // card covering it. Both are sublayers of the same card.
            ctx.set_sublayer(layer, top);
            ctx.move_to_top(top);
            let ui = &mut ui.new_child(egui::UiBuilder::new().layer_id(top).max_rect(rect));
            ui.set_clip_rect(rect);
            if blocked {
                ui.style_mut().visuals.disabled_alpha = 1.0;
                ui.disable();
            }
            close = close_box(ui, card, rect);
        }
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::from_black_alpha(60)),
            egui::StrokeKind::Outside,
        );
        if close {
            // As a window's close box: OnWindowClose, then its record's
            // commit; the card stays if either refuses.
            self.with_window(card, |app| {
                if !app.script.busy() {
                    app.apply(&ctx, Action::CloseWindow);
                }
            });
        }
    }
}

/// Where the close box of a card at `rect` is: its top right corner.
fn close_box_rect(rect: Rect) -> Rect {
    Rect::from_min_size(
        egui::pos2(rect.right() - CLOSE_BOX - 6.0, rect.top() + 6.0),
        egui::vec2(CLOSE_BOX, CLOSE_BOX),
    )
}

/// The card's close box at its top right corner, drawn (no font has a
/// glyph every platform shares); `true` when clicked.
fn close_box(ui: &mut Ui, card: u32, rect: Rect) -> bool {
    let button = close_box_rect(rect);
    let response = ui
        .interact(
            button,
            ui.scope_id().with(("openmaker-card-close", card)),
            egui::Sense::click(),
        )
        .on_hover_text("Close");
    let enabled = ui.is_enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, "Close card"));
    let visuals = ui.style().interact(&response);
    let painter = ui.painter();
    painter.circle(
        button.center(),
        CLOSE_BOX / 2.0,
        visuals.bg_fill,
        Stroke::new(1.0, theme::CAPTION),
    );
    let (c, d) = (button.center(), CLOSE_BOX / 4.5);
    let stroke = Stroke::new(1.5, visuals.fg_stroke.color);
    painter.line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
    painter.line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], stroke);
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::super::tests::two_record_windows;
    use super::super::windows::Doc;
    use super::Action;
    use crate::scripting::{Card, ScriptEvent, WindowStyle, WindowUpdate};
    use crate::worker::{Reply, Response};

    /// Window 3 is a card on window 2, behind window 1. The Window menu
    /// brings the card forward over its parent; when the parent closes
    /// the card goes with it, and a late update for the card reaches no
    /// other window.
    #[test]
    fn a_card_goes_with_its_parent_and_its_replies_reach_no_other_window() {
        let ctx = egui::Context::default();
        let mut app = two_record_windows();
        let mut card = Doc::new(3);
        card.file_id = app.windows[0].file_id;
        card.style = WindowStyle::Card(Card {
            parent: 2,
            dim: true,
            close: true,
            left: None,
            top: None,
            width: None,
            height: None,
        });
        app.windows.push(card);
        assert_eq!(app.viewport_of(3), app.viewport_of(2));
        assert_eq!(app.card_chain(2), [2, 3]);
        assert_eq!(app.root_window(), 1);
        app.apply(&ctx, Action::SelectWindow(2));
        assert_eq!(app.w.id, 3);
        let mut stale = app.window_state();
        stale.title = Some("Stale".into());
        let epoch = app.epoch;
        app.on_response(
            &ctx,
            Response {
                epoch,
                window: 2,
                reply: Reply::Script(ScriptEvent::CloseWindow(2)),
            },
        );
        assert_eq!(app.window_ids(), [1]);
        app.on_response(
            &ctx,
            Response {
                epoch,
                window: 3,
                reply: Reply::Script(ScriptEvent::Window(Box::new(WindowUpdate {
                    file: app.w.file_id,
                    state: stale,
                    data_changed: false,
                    refreshed: false,
                    objects: Vec::new(),
                    scroll: Vec::new(),
                    fronted: Vec::new(),
                    originals: Default::default(),
                    found_set: None,
                    background: false,
                }))),
            },
        );
        assert_eq!(app.window_ids(), [1]);
        assert_eq!(app.w.title, None);
    }
}
