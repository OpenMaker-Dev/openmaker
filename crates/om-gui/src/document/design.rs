//! Static layout artwork. The design canvas supplies draft frames and owns
//! all interaction. No record values, calculations or actions are read here.

use std::borrow::Cow;

use egui::{Color32, Rect, Ui};
use om_layout::{Layout, ObjectKind, TextRun};

use super::{
    color, layout_cache::LayoutCache, line, paint_box, paint_label_icon, paint_library_image,
    paint_rich_text, Session,
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

    pub(crate) fn paint_part(&self, ui: &Ui, id: u64, rect: Rect) {
        if let Some(part) = self.layout.parts.iter().find(|part| part.id == id) {
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
        if kind == ObjectKind::Graphic {
            let Some(image) = object.graphic.as_ref().filter(|g| g.library_key != 0) else {
                return false;
            };
            return paint_library_image(ui, self.cache, self.layout, image.library_key, rect, None);
        }
        if matches!(
            kind,
            ObjectKind::Text | ObjectKind::Field | ObjectKind::Button | ObjectKind::PopoverButton
        ) {
            let text = if kind == ObjectKind::Field {
                Cow::Borrowed(field_caption)
            } else if !object.shows_label() {
                Cow::Borrowed("")
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
            let rect = paint_label_icon(
                ui,
                self.cache,
                (self.layout, object),
                rect,
                text.is_empty(),
                None,
                (1, style.border_color[0]),
            );
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
