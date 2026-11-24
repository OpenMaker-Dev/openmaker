//! A frame for the native painter: egui's tessellated triangles, its
//! texture changes and clip rectangles, as egui's own painters take them
//! (egui-wgpu's shader: positions in points, colors and textures as
//! premultiplied sRGB, blended without converting to linear light).

use egui::epaint::{ImageDelta, Primitive, TextureId};
use egui::{ClippedPrimitive, TextureOptions, TexturesDelta};

use crate::{Element, Menu, Viewport};

/// What the native painter draws, and what the chrome shows with it.
#[derive(Debug, uniffi::Record)]
pub struct Frame {
    /// The canvas the frame was made for.
    pub viewport: Viewport,
    /// Texture changes to make before drawing, in order.
    pub textures: Vec<TextureUpdate>,
    /// Textures to free after drawing.
    pub freed: Vec<u64>,
    /// Vertices, 20 bytes each, little-endian: x and y in points (f32), u
    /// and v (f32), and the color as premultiplied sRGB bytes r, g, b, a.
    pub vertices: Vec<u8>,
    /// Triangle indices into `vertices`, u32 little-endian.
    pub indices: Vec<u8>,
    /// The draw calls, in order.
    pub batches: Vec<Batch>,
    pub state: crate::DocumentState,
    /// The menus, when they changed since the last frame taken.
    pub menus: Option<Vec<Menu>>,
    /// The visible fields, labels and buttons, when they changed since the
    /// last frame taken (only while a screen reader runs).
    pub elements: Option<Vec<Element>>,
    /// Text the file copied, for the clipboard.
    pub copied: Option<String>,
    /// URLs the file asked to open (Open URL), in order.
    pub open_urls: Vec<String>,
    pub timings: FrameTimings,
}

/// A run of [`Frame::indices`] drawn with one texture inside one clip
/// rectangle (in pixels from the canvas's top left).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Batch {
    pub texture: u64,
    pub clip_x: u32,
    pub clip_y: u32,
    pub clip_width: u32,
    pub clip_height: u32,
    pub first_index: u32,
    pub index_count: u32,
}

/// New pixels for a texture: the whole texture, made anew at `width` by
/// `height`, or with `partial` a region of it at `x`, `y`.
#[derive(Clone, Debug, uniffi::Record)]
pub struct TextureUpdate {
    pub id: u64,
    pub partial: bool,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Sampled smoothly when magnified and minified (else nearest).
    pub magnify_linear: bool,
    pub minify_linear: bool,
    pub wrap: TextureWrap,
    /// Premultiplied sRGB bytes r, g, b, a, row by row.
    pub pixels: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TextureWrap {
    Clamp,
    Repeat,
    Mirror,
}

/// How long the engine took to make the frame, in microseconds: egui's
/// pass (the window's own drawing), tessellation, and packing.
#[derive(Clone, Copy, Debug, Default, uniffi::Record)]
pub struct FrameTimings {
    pub pass: u32,
    pub tessellate: u32,
    pub pack: u32,
}

/// One id space for egui's managed and user textures.
pub(crate) fn texture_key(id: TextureId) -> u64 {
    match id {
        TextureId::Managed(n) => n << 1,
        TextureId::User(n) => (n << 1) | 1,
    }
}

/// egui's texture changes as updates and frees. A texture's own changes
/// keep their order.
pub(crate) fn texture_updates(mut delta: TexturesDelta) -> (Vec<TextureUpdate>, Vec<u64>) {
    let updates = std::mem::take(&mut delta.set)
        .into_iter()
        .flat_map(|(id, images)| {
            images
                .into_iter()
                .map(move |image| update(texture_key(id), image))
        })
        .collect();
    let freed = std::mem::take(&mut delta.free)
        .into_iter()
        .map(texture_key)
        .collect();
    (updates, freed)
}

fn update(id: u64, delta: ImageDelta) -> TextureUpdate {
    let egui::ImageData::Color(image) = &delta.image;
    let mut pixels = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        pixels.extend_from_slice(&pixel.to_array());
    }
    let TextureOptions {
        magnification,
        minification,
        wrap_mode,
        ..
    } = delta.options;
    let [x, y] = delta.pos.unwrap_or([0, 0]);
    TextureUpdate {
        id,
        partial: delta.pos.is_some(),
        x: x as u32,
        y: y as u32,
        width: image.size[0] as u32,
        height: image.size[1] as u32,
        magnify_linear: magnification == egui::TextureFilter::Linear,
        minify_linear: minification == egui::TextureFilter::Linear,
        wrap: match wrap_mode {
            egui::TextureWrapMode::ClampToEdge => TextureWrap::Clamp,
            egui::TextureWrapMode::Repeat => TextureWrap::Repeat,
            egui::TextureWrapMode::MirroredRepeat => TextureWrap::Mirror,
        },
        pixels,
    }
}

/// The frame's geometry: vertices, indices and batches.
#[derive(Default, PartialEq)]
pub(crate) struct Geometry {
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub batches: Vec<Batch>,
}

/// Pack egui's meshes into one vertex and one index buffer, with a batch
/// per run of one texture and clip rectangle. Meshes clipped away are left
/// out; paint callbacks, which egui emits only for custom painters, are
/// not drawn (OpenMaker has none).
pub(crate) fn pack(primitives: &[ClippedPrimitive], viewport: Viewport) -> Geometry {
    let mut geometry = Geometry::default();
    let (width, height) = (
        (viewport.width * viewport.scale).round(),
        (viewport.height * viewport.scale).round(),
    );
    let mut vertex_count = 0u32;
    let mut index_count = 0u32;
    for ClippedPrimitive {
        clip_rect,
        primitive,
    } in primitives
    {
        let Primitive::Mesh(mesh) = primitive else {
            continue;
        };
        if mesh.indices.is_empty() {
            continue;
        }
        let pixel = |value: f32, limit: f32| (value * viewport.scale).round().clamp(0.0, limit);
        let (x0, y0) = (
            pixel(clip_rect.min.x, width),
            pixel(clip_rect.min.y, height),
        );
        let (x1, y1) = (
            pixel(clip_rect.max.x, width),
            pixel(clip_rect.max.y, height),
        );
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        for vertex in &mesh.vertices {
            let v = &mut geometry.vertices;
            v.extend_from_slice(&vertex.pos.x.to_le_bytes());
            v.extend_from_slice(&vertex.pos.y.to_le_bytes());
            v.extend_from_slice(&vertex.uv.x.to_le_bytes());
            v.extend_from_slice(&vertex.uv.y.to_le_bytes());
            v.extend_from_slice(&vertex.color.to_array());
        }
        for index in &mesh.indices {
            geometry
                .indices
                .extend_from_slice(&(vertex_count + index).to_le_bytes());
        }
        let batch = Batch {
            texture: texture_key(mesh.texture_id),
            clip_x: x0 as u32,
            clip_y: y0 as u32,
            clip_width: (x1 - x0) as u32,
            clip_height: (y1 - y0) as u32,
            first_index: index_count,
            index_count: mesh.indices.len() as u32,
        };
        vertex_count += mesh.vertices.len() as u32;
        index_count += batch.index_count;
        match geometry.batches.last_mut() {
            // The same texture and clip: one draw call.
            Some(last)
                if last.texture == batch.texture
                    && (last.clip_x, last.clip_y, last.clip_width, last.clip_height)
                        == (
                            batch.clip_x,
                            batch.clip_y,
                            batch.clip_width,
                            batch.clip_height,
                        ) =>
            {
                last.index_count += batch.index_count;
            }
            _ => geometry.batches.push(batch),
        }
    }
    geometry
}

impl Frame {
    /// The last frame of a document whose window stopped on an error: it
    /// draws nothing and closes, saying why.
    pub(crate) fn stopped(viewport: Viewport, failure: String) -> Frame {
        Frame {
            viewport,
            textures: Vec::new(),
            freed: Vec::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            batches: Vec::new(),
            state: crate::DocumentState::stopped(failure),
            menus: Some(Vec::new()),
            elements: None,
            copied: None,
            open_urls: Vec::new(),
            timings: FrameTimings::default(),
        }
    }

    /// Take in a frame made before this one that the app has not taken:
    /// its texture changes come first, and what it carried that this one
    /// does not (menus, elements, copied text, URLs) is kept.
    pub(crate) fn absorb(&mut self, older: Frame) {
        // A texture the older frame freed is not drawn by this one: its
        // pixels are not needed, only the release.
        let mut textures = older.textures;
        textures.retain(|update| !older.freed.contains(&update.id));
        for update in std::mem::take(&mut self.textures) {
            if !update.partial {
                // A whole texture replaces what came before for it.
                textures.retain(|earlier| earlier.id != update.id);
            }
            textures.push(update);
        }
        // A texture this frame frees may still be drawn by it (egui frees
        // after painting), so its earlier pixels stay.
        self.textures = textures;
        let mut freed = older.freed;
        freed.append(&mut self.freed);
        self.freed = freed;
        self.menus = self.menus.take().or(older.menus);
        self.elements = self.elements.take().or(older.elements);
        self.copied = self.copied.take().or(older.copied);
        let mut urls = older.open_urls;
        urls.append(&mut self.open_urls);
        self.open_urls = urls;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(id: u64, partial: bool, x: u32) -> TextureUpdate {
        TextureUpdate {
            id,
            partial,
            x,
            y: 0,
            width: 1,
            height: 1,
            magnify_linear: true,
            minify_linear: true,
            wrap: TextureWrap::Clamp,
            pixels: vec![0; 4],
        }
    }

    fn frame(textures: Vec<TextureUpdate>, freed: Vec<u64>, menus: Option<Vec<Menu>>) -> Frame {
        let mut frame = Frame::stopped(
            Viewport {
                width: 1.0,
                height: 1.0,
                scale: 1.0,
                max_texture_side: 2048,
            },
            String::new(),
        );
        frame.textures = textures;
        frame.freed = freed;
        frame.menus = menus;
        frame
    }

    /// A frame the app never took must not lose a texture change: the font
    /// atlas grows by partial updates, and one dropped is a missing glyph
    /// until the atlas is rebuilt.
    #[test]
    fn frames_not_taken_keep_every_texture_change_in_order() {
        let menu = Menu {
            title: "Records".into(),
            rows: Vec::new(),
        };
        let older = frame(
            vec![
                upload(0, false, 0),
                upload(0, true, 1),
                upload(2, false, 0),
                upload(4, false, 0),
            ],
            vec![4],
            Some(vec![menu.clone()]),
        );
        let mut newer = frame(vec![upload(0, true, 2), upload(2, false, 5)], vec![6], None);
        newer.absorb(older);
        let kept: Vec<_> = newer
            .textures
            .iter()
            .map(|t| (t.id, t.partial, t.x))
            .collect();
        // Texture 0: its whole image and both regions, in order. Texture 2:
        // only its newer whole image, which replaces the older. Texture 4,
        // freed before the newer frame: only its release.
        assert_eq!(
            kept,
            [(0, false, 0), (0, true, 1), (0, true, 2), (2, false, 5)]
        );
        assert_eq!(newer.freed, [4, 6]);
        assert_eq!(newer.menus, Some(vec![menu]));
    }
}
