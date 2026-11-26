//! Static layout artwork. The design canvas supplies draft frames and owns
//! all interaction. No record values, calculations or actions are read here.

use std::borrow::Cow;

use egui::{pos2, vec2, Color32, CornerRadius, Rect, Stroke, StrokeKind, Ui};
use om_layout::{Layout, ObjectKind, TextRun};

use super::{
    color, layout_cache::LayoutCache, line, paint_box, paint_library_image, paint_rich_text,
    Session,
};

pub(crate) struct Surface<'a> {
    layout: &'a Layout,
    cache: &'a LayoutCache,
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

    /// Paint the object in its current draft frame. Unsupported objects
    /// keep the canvas's outline and caption.
    pub(crate) fn paint_object(
        &self,
        ui: &mut Ui,
        key: u64,
        rect: Rect,
        field_caption: &str,
        draft_words: Option<&str>,
    ) -> bool {
        let Some(object) = self.cache.object(self.layout, key) else {
            return false;
        };
        let kind = object.kind();
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
            let text = if kind == ObjectKind::Field {
                Cow::Borrowed(field_caption)
            } else if !object.shows_label() {
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
            let style = self.cache.text_style(self.layout, object, &[], 1);
            let rich = draft_words
                .is_none()
                .then_some(object.rich_text.as_ref())
                .flatten();
            let _ = paint_rich_text(ui, rect, &text, &style, rich, None);
        }
        true
    }
}
