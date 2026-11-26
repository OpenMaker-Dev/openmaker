//! Static layout artwork. The design canvas supplies draft frames and owns
//! all interaction. No record values, calculations or actions are read here.

use std::borrow::Cow;
use std::collections::BTreeSet;

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Stroke, StrokeKind, Ui};
use om_layout::{Layout, ObjectKind, TextRun};

use super::{
    color, layout_cache::LayoutCache, line, paint_box, paint_label_icon, paint_library_image,
    paint_rich_text, rich_text, scaled_text, ButtonIconMode, Session,
};

pub(crate) struct Surface<'a> {
    layout: &'a Layout,
    cache: &'a LayoutCache,
}

#[derive(Default)]
pub(crate) struct TabArtwork {
    framed_panels: BTreeSet<u64>,
    pub(crate) selected_labels: Vec<Rect>,
}

impl<'a> Surface<'a> {
    pub(super) fn new(session: &'a Session, cache: &'a LayoutCache) -> Option<Self> {
        let crate::session::RenderState::Loaded(render) = &session.render else {
            return None;
        };
        let layout = &render.layout;
        (session.layout.map(u64::from) == Some(layout.id)).then_some(Self { layout, cache })
    }

    pub(crate) fn background(&self) -> Option<Color32> {
        self.cache
            .background_style(self.layout, 1, 1)
            .background
            .map(color)
    }

    pub(crate) fn paint_parts(&self, ui: &Ui, surface: Rect) {
        for part in &self.layout.parts {
            let rect = Rect::from_min_size(
                pos2(surface.left(), surface.top() + part.top as f32),
                vec2(surface.width(), part.height as f32),
            );
            if !rect.is_finite() || !rect.is_positive() || !ui.clip_rect().intersects(rect) {
                continue;
            }
            let style = self.cache.part_style(self.layout, part, 1, 1);
            paint_box(ui, rect, &style, ObjectKind::Rectangle);
        }
    }

    /// Draw the saved headers and front-panel background before the canvas
    /// paints the panel's children. Collect selected labels' ink bounds so
    /// their editing frames can be painted above the artwork.
    fn paint_tabs(
        &self,
        ui: &mut Ui,
        control: &om_layout::LayoutObject,
        rect: Rect,
        (visible, artwork, selected): (&dyn Fn(u64) -> bool, &mut TabArtwork, bool),
    ) -> bool {
        let Some(tab) = control
            .tab
            .as_ref()
            .filter(|tab| tab.tab_height > 0 && rect.is_positive())
        else {
            return false;
        };
        let height = (tab.tab_height as f32).min(rect.height());
        let panels = control
            .children
            .iter()
            .filter_map(|&id| self.cache.object(self.layout, id))
            .filter(|panel| {
                panel.parent == Some(control.id) && panel.kind() == ObjectKind::TabPanel
            });

        let clip = ui.clip_rect();
        ui.set_clip_rect(clip.intersect(rect.expand(1.0)));
        let body = Rect::from_min_max(rect.left_top() + vec2(0.0, height), rect.max);
        let front = panels.clone().find(|panel| visible(panel.id));
        let mut front_painted = false;
        if let Some(front) = front.filter(|_| body.is_positive()) {
            let style = self.cache.formatted_style(self.layout, front, &[], 1, 7);
            paint_box(ui, body, &style, ObjectKind::Rectangle);
            front_painted = true;
        }
        let scale = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .map_or(1.0, |transform| transform.scaling);
        let mut painted = front_painted;
        for panel in panels {
            let Some(position) = panel.tab_panel.as_ref().filter(|info| info.tab_width > 0) else {
                continue;
            };
            let header = Rect::from_min_size(
                pos2(rect.left() + position.tab_left_edge as f32, rect.top()),
                vec2(position.tab_width as f32, height),
            );
            if !header.intersect(rect).is_positive() {
                continue;
            }
            let active = front.is_some_and(|front| front.id == panel.id);
            front_painted |= active;
            painted = true;
            if !ui.clip_rect().intersects(header) {
                continue;
            }
            let state = if active { 7 } else { 1 };
            let mut style = self
                .cache
                .formatted_style(self.layout, panel, &[], 1, state);
            let mut box_rect = header;
            if active
                && body.is_positive()
                && style.background.is_some_and(|fill| color(fill).is_opaque())
            {
                // Join the active tab to its panel by covering the panel's
                // top border under the tab and omitting the tab's bottom rule.
                box_rect.max.y += super::border_width(&style, 0).unwrap_or_default() * 0.5;
                style.border_width[2] = None;
            }
            paint_box(ui, box_rect, &style, ObjectKind::Rectangle);
            let Some(label) = panel.text.as_deref().filter(|text| !text.is_empty()) else {
                continue;
            };
            let mut text = self.cache.text_style(self.layout, panel, &[], state);
            let defaults = self.cache.text_style(self.layout, control, &[], state);
            text.font_size = text.font_size.or(defaults.font_size);
            text.color = text.color.or(defaults.color);
            text.text_align = Some(2);
            let [top, right, bottom, left] =
                text.padding.map(|value| value.unwrap_or_default() as f32);
            let label_rect = Rect::from_min_max(
                header.min + vec2(left, top),
                header.max - vec2(right, bottom),
            );
            if !label_rect.is_positive() {
                continue;
            }
            let galley = ui.fonts_mut(|fonts| {
                let job = rich_text::layout_job(fonts, label, &text, None, label_rect.width());
                scaled_text::layout_galley(fonts, job, scale)
            });
            let space = (label_rect.height() - galley.size().y).max(0.0);
            let origin = pos2(label_rect.center().x, label_rect.top() + space * 0.5);
            let painter = ui
                .painter()
                .with_clip_rect(ui.clip_rect().intersect(label_rect));
            if selected {
                artwork.selected_labels.push(
                    galley
                        .mesh_bounds
                        .translate(origin.to_vec2())
                        .expand(1.0)
                        .intersect(painter.clip_rect()),
                );
            }
            painter.galley(
                origin,
                galley,
                text.color.map(color).unwrap_or(Color32::BLACK),
            );
        }
        ui.set_clip_rect(clip);
        if let Some(front) = front.filter(|_| front_painted) {
            artwork.framed_panels.insert(front.id);
        }
        painted
    }

    /// Paint the object in its current draft frame. Unsupported objects
    /// keep the canvas's outline and caption.
    pub(crate) fn paint_object(
        &self,
        ui: &mut Ui,
        key: u64,
        rect: Rect,
        field_caption: &str,
        draft_words: Option<&str>,
        tabs: (&dyn Fn(u64) -> bool, &mut TabArtwork, bool),
    ) -> bool {
        let Some(object) = self.cache.object(self.layout, key) else {
            return false;
        };
        let kind = object.kind();
        if kind == ObjectKind::TabControl {
            return self.paint_tabs(ui, object, rect, tabs);
        }
        if kind == ObjectKind::TabPanel {
            // The parent paints the panel frame and headers. Canvas traversal
            // still owns visibility, selection and drawing the children.
            return tabs.1.framed_panels.contains(&key);
        }
        if !matches!(
            kind,
            ObjectKind::Field
                | ObjectKind::Text
                | ObjectKind::Rectangle
                | ObjectKind::RoundedRectangle
                | ObjectKind::Oval
                | ObjectKind::Line
                | ObjectKind::Button
                | ObjectKind::PopoverButton
                | ObjectKind::Graphic
        ) {
            return false;
        }
        if kind != ObjectKind::Line && !rect.is_positive() {
            return false;
        }
        if !ui.clip_rect().intersects(rect.expand(1.0)) {
            return true;
        }
        let style = self.cache.formatted_style(self.layout, object, &[], 1, 1);
        if kind == ObjectKind::Line {
            line::paint(ui.painter(), rect, object.flags, &style);
            return true;
        }
        paint_box(ui, rect, &style, kind);
        if matches!(kind, ObjectKind::Field | ObjectKind::Text)
            && !(0..4).any(|side| {
                super::border_width(&style, side).is_some()
                    && style.border_color[side].is_none_or(|c| color(c).a() > 0)
            })
        {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, crate::theme::HAIRLINE),
                StrokeKind::Inside,
            );
        }
        if kind == ObjectKind::Graphic {
            let Some(image) = object.graphic.as_ref().filter(|g| g.library_key != 0) else {
                return false;
            };
            if self
                .cache
                .library_image(self.layout, image.library_key, None)
                .is_none()
            {
                return false;
            }
            paint_library_image(ui, self.cache, self.layout, image.library_key, rect, None);
            return true;
        }
        if matches!(
            kind,
            ObjectKind::Text | ObjectKind::Field | ObjectKind::Button | ObjectKind::PopoverButton
        ) {
            let button = matches!(kind, ObjectKind::Button | ObjectKind::PopoverButton);
            let text = if kind == ObjectKind::Field {
                Cow::Borrowed(field_caption)
            } else if !button && !object.shows_label() {
                return false;
            } else if let Some(words) = draft_words {
                Cow::Borrowed(words)
            } else if object.text_runs.is_empty() {
                Cow::Borrowed(object.text.as_deref().unwrap_or_default())
            } else {
                Cow::Owned(
                    object
                        .text_runs
                        .iter()
                        .map(|run| match run {
                            TextRun::Literal(text) => Cow::Borrowed(text.as_str()),
                            TextRun::MergeField(name) => Cow::Owned(format!("<<{name}>>")),
                        })
                        .collect(),
                )
            };
            let text_style = self.cache.text_style(self.layout, object, &[], 1);
            let rich = draft_words
                .is_none()
                .then_some(object.rich_text.as_ref())
                .flatten();
            let label = if button {
                let (label, has_icon) = paint_label_icon(
                    ui,
                    self.cache,
                    (self.layout, object),
                    rect,
                    (&text, &text_style, rich),
                    ButtonIconMode::Layout,
                    (1, style.border_color[0]),
                );
                let Some(label) = label else {
                    return has_icon;
                };
                label
            } else {
                rect
            };
            let _ = paint_rich_text(ui, label, &text, &text_style, rich, None);
        }
        true
    }
}
