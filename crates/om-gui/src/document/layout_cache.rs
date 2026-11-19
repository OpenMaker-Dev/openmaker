//! Work reused while painting one immutable loaded layout: object lookups
//! and culling, resolved styles, resized bounds and library image bytes.
//!
//! The owner calls [`LayoutCache::clear`] whenever it accepts a layout
//! snapshot: every render reply, even one whose file, layout and object ids
//! all match the last, and a snapshot put back when the connection is lost.
//! Snapshots are never told apart by address, which an allocator can reuse
//! once a layout is dropped. A layout with another id starts over as well.
//! Record data, scrolling and zoom need no clear: conditional styles are
//! kept per list of formats that hold, bounds for the window growth they
//! were resized by.

use std::{
    cell::{RefCell, RefMut},
    collections::HashMap,
};

use egui::{Color32, ImageSource, Rect, Vec2};
use om_layout::{Layout, LayoutObject, Part, ResolvedStyle};

/// The condition lists kept per style. A found set whose records turn an
/// object's formats on in more combinations resolves the rest again.
const COMBINATIONS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Styled {
    Object(u64),
    /// By index: parts need not have distinct ids.
    Part(usize),
    Background,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Style {
    Formatted(i32),
    Text,
    ConditionalText,
}

type StyleKey = (Styled, Style, i32);

/// A style by the conditional formats that hold, in stored order.
type Variants = Vec<(Box<[usize]>, ResolvedStyle)>;

#[derive(Default)]
struct Snapshot {
    layout: Option<u64>,
    indices: HashMap<u64, usize>,
    on_layout: HashMap<u64, bool>,
    styles: HashMap<StyleKey, Variants>,
    properties: HashMap<(u64, i32, i32, i32), bool>,
    growth: Option<[u32; 2]>,
    bounds: HashMap<u64, Option<Rect>>,
    images: HashMap<(u64, Option<Color32>), Option<ImageSource<'static>>>,
}

impl Snapshot {
    fn prepare(&mut self, layout: &Layout) {
        if self.layout == Some(layout.id) {
            return;
        }
        *self = Self {
            layout: Some(layout.id),
            ..Self::default()
        };
        for (index, object) in layout.objects.objects.iter().enumerate() {
            // Match ObjectStore::get when malformed data repeats an id.
            self.indices.entry(object.id).or_insert(index);
        }
    }

    /// Styles are kept by object id, so only for the object its id finds:
    /// never a later duplicate or a copy from elsewhere.
    fn owns(&self, layout: &Layout, object: &LayoutObject) -> bool {
        self.indices
            .get(&object.id)
            .and_then(|&index| layout.objects.objects.get(index))
            .is_some_and(|kept| std::ptr::eq(kept, object))
    }
}

#[derive(Default)]
pub(super) struct LayoutCache {
    snapshot: RefCell<Snapshot>,
    /// Tests paint without reusing anything, to compare.
    #[cfg(test)]
    uncached: bool,
}

impl LayoutCache {
    #[cfg(test)]
    pub fn uncached() -> Self {
        Self {
            uncached: true,
            ..Self::default()
        }
    }

    pub fn clear(&self) {
        *self.snapshot.borrow_mut() = Snapshot::default();
    }

    fn prepared(&self, layout: &Layout) -> Option<RefMut<'_, Snapshot>> {
        #[cfg(test)]
        if self.uncached {
            return None;
        }
        let mut snapshot = self.snapshot.borrow_mut();
        snapshot.prepare(layout);
        Some(snapshot)
    }

    pub fn object<'a>(&self, layout: &'a Layout, id: u64) -> Option<&'a LayoutObject> {
        let Some(snapshot) = self.prepared(layout) else {
            return layout.objects.get(id);
        };
        layout.objects.objects.get(*snapshot.indices.get(&id)?)
    }

    /// Browse culling uses original f64 geometry, independent of scrolling
    /// and window growth. Avoid repeating all ancestor searches each frame.
    pub fn on_layout(&self, layout: &Layout, id: u64) -> bool {
        let on_layout = || {
            layout.absolute_bounds(id).is_none_or(|bounds| {
                !(bounds.left > layout.width()
                    || bounds.top > layout.height()
                    || bounds.right < 0.0
                    || bounds.bottom < 0.0)
            })
        };
        match self.prepared(layout) {
            Some(mut snapshot) => *snapshot.on_layout.entry(id).or_insert_with(on_layout),
            None => on_layout(),
        }
    }

    /// [`super::formatted_style`].
    pub fn formatted_style(
        &self,
        layout: &Layout,
        object: &LayoutObject,
        conditions: &[usize],
        component: i32,
        state: i32,
    ) -> ResolvedStyle {
        self.style(
            layout,
            |snapshot| {
                snapshot.owns(layout, object).then_some((
                    Styled::Object(object.id),
                    Style::Formatted(component),
                    state,
                ))
            },
            conditions,
            || super::formatted_style(layout, object, conditions, component, state),
        )
    }

    /// [`super::text_style`].
    pub fn text_style(
        &self,
        layout: &Layout,
        object: &LayoutObject,
        conditions: &[usize],
        state: i32,
    ) -> ResolvedStyle {
        self.style(
            layout,
            |snapshot| {
                snapshot.owns(layout, object).then_some((
                    Styled::Object(object.id),
                    Style::Text,
                    state,
                ))
            },
            conditions,
            || super::text_style(layout, object, conditions, state),
        )
    }

    /// [`super::conditional_text_style`].
    pub fn conditional_text_style(
        &self,
        layout: &Layout,
        object: &LayoutObject,
        conditions: &[usize],
        state: i32,
    ) -> ResolvedStyle {
        self.style(
            layout,
            |snapshot| {
                snapshot.owns(layout, object).then_some((
                    Styled::Object(object.id),
                    Style::ConditionalText,
                    state,
                ))
            },
            conditions,
            || super::conditional_text_style(layout, object, conditions, state),
        )
    }

    pub fn part_style(
        &self,
        layout: &Layout,
        part: &Part,
        component: i32,
        state: i32,
    ) -> ResolvedStyle {
        self.style(
            layout,
            |_| {
                let index = layout.parts.iter().position(|p| std::ptr::eq(p, part))?;
                Some((Styled::Part(index), Style::Formatted(component), state))
            },
            &[],
            || layout.part_style(part, component, state),
        )
    }

    pub fn background_style(&self, layout: &Layout, component: i32, state: i32) -> ResolvedStyle {
        self.style(
            layout,
            |_| Some((Styled::Background, Style::Formatted(component), state)),
            &[],
            || layout.background_style(component, state),
        )
    }

    /// `key` is `None` for what the snapshot cannot keep: it is resolved
    /// every time.
    fn style(
        &self,
        layout: &Layout,
        key: impl FnOnce(&Snapshot) -> Option<StyleKey>,
        conditions: &[usize],
        resolve: impl FnOnce() -> ResolvedStyle,
    ) -> ResolvedStyle {
        let Some(mut snapshot) = self.prepared(layout) else {
            return resolve();
        };
        let Some(key) = key(&snapshot) else {
            return resolve();
        };
        let variants = snapshot.styles.entry(key).or_default();
        if let Some((_, style)) = variants.iter().find(|(held, _)| **held == *conditions) {
            return style.clone();
        }
        if variants.len() >= COMBINATIONS {
            variants.clear();
        }
        let style = resolve();
        variants.push((conditions.into(), style.clone()));
        style
    }

    /// Whether an integer property outside [`ResolvedStyle`] cascades to a
    /// nonzero value, as a portal's alternate and active row flags do.
    pub fn property_set(
        &self,
        layout: &Layout,
        object: &LayoutObject,
        component: i32,
        state: i32,
        property: i32,
    ) -> bool {
        let set = || {
            layout
                .object_property(object, component, state, property)
                .and_then(|property| property.int)
                .is_some_and(|value| value != 0)
        };
        match self.prepared(layout) {
            Some(mut snapshot) if snapshot.owns(layout, object) => *snapshot
                .properties
                .entry((object.id, component, state, property))
                .or_insert_with(set),
            _ => set(),
        }
    }

    /// [`super::resized_bounds`], kept while the window's growth is the
    /// same: scrolling reuses them, a resize or zoom resizes them again.
    pub fn bounds(&self, layout: &Layout, id: u64, growth: Vec2) -> Option<Rect> {
        let Some(mut snapshot) = self.prepared(layout) else {
            return super::resized_bounds(layout, id, growth);
        };
        let key = [growth.x.to_bits(), growth.y.to_bits()];
        if snapshot.growth != Some(key) {
            snapshot.growth = Some(key);
            snapshot.bounds.clear();
        }
        *snapshot
            .bounds
            .entry(id)
            .or_insert_with(|| super::resized_bounds(layout, id, growth))
    }

    /// [`super::library_image`]: its bytes are shared with egui's loaders
    /// instead of being copied, tinted and hashed again every frame.
    pub fn library_image(
        &self,
        layout: &Layout,
        key: u64,
        tint: Option<Color32>,
    ) -> Option<ImageSource<'static>> {
        let Some(mut snapshot) = self.prepared(layout) else {
            return super::library_image(layout, key, tint);
        };
        snapshot
            .images
            .entry((key, tint))
            .or_insert_with(|| super::library_image(layout, key, tint))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_layout::{LayoutDef, ObjectStore, Rect};

    fn object(id: u64, left: f64, parent: Option<u64>) -> LayoutObject {
        LayoutObject {
            id,
            parent,
            bounds: Some(Rect {
                left,
                right: left + 10.0,
                top: 0.0,
                bottom: 10.0,
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn layout(id: u64, objects: Vec<LayoutObject>) -> Layout {
        Layout {
            id,
            def: Some(LayoutDef {
                base_width: 100.0,
                ..Default::default()
            }),
            parts: vec![Part {
                part_type: 4,
                height: 100.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                objects,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn lookups_preserve_first_duplicate_missing_and_full_precision_culling() {
        let mut layout = layout(
            7,
            vec![object(9, 100.000_000_01, None), object(9, 1.0, None)],
        );
        let cache = LayoutCache::default();
        for _ in 0..3 {
            assert!(std::ptr::eq(
                cache.object(&layout, 9).unwrap(),
                layout.objects.get(9).unwrap()
            ));
            assert!(cache.object(&layout, 10).is_none());
            assert!(!cache.on_layout(&layout, 9)); // f32 would round onto the edge
        }
        layout.objects.objects.reverse();
        layout.def.as_mut().unwrap().base_width = 200.0;
        cache.clear(); // replacement snapshot reuses its file/layout/object ids
        assert!(std::ptr::eq(
            cache.object(&layout, 9).unwrap(),
            &layout.objects.objects[0]
        ));
        assert!(cache.on_layout(&layout, 9));
    }

    #[test]
    fn culling_uses_ancestor_coordinates_and_rebuilds_for_another_layout() {
        let mut layout = layout(8, vec![object(1, 110.0, None), object(2, -5.0, Some(1))]);
        let cache = LayoutCache::default();
        assert!(!cache.on_layout(&layout, 2));
        layout.id = 9;
        layout.def.as_mut().unwrap().base_width = 200.0;
        assert!(cache.on_layout(&layout, 2));
        assert!(std::ptr::eq(
            cache.object(&layout, 2).unwrap(),
            layout.objects.get(2).unwrap()
        ));
    }

    use std::sync::Arc;

    use egui::{load::Bytes, vec2};
    use om_layout::style::ComponentStyle;
    use om_layout::{
        CfCondition, Color, ConditionalFormat, LibraryImage, LibraryImageFormat, ObjectStyle,
        Property, StyleSet,
    };

    /// What [`poison`] writes over every kept style.
    const MARKER: Color = Color {
        red: 0.5,
        green: 0.25,
        blue: 0.125,
        alpha: 1.0,
    };

    fn red(red: f32) -> Color {
        Color {
            red,
            alpha: 1.0,
            ..Default::default()
        }
    }

    fn property(id: i32, color: Color) -> Property {
        Property {
            id,
            color: Some(color),
            ..Default::default()
        }
    }

    fn local(components: Vec<(i32, i32, Vec<Property>)>) -> ObjectStyle {
        ObjectStyle {
            local_styles: Some(StyleSet {
                components: components
                    .into_iter()
                    .map(|(component, state, properties)| ComponentStyle {
                        component,
                        state,
                        properties,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Object 1, styled by `shade` in several components and states, with
    /// two conditional formats; parts, background and images to match.
    fn styled(id: u64, shade: f32) -> Layout {
        let format = |properties| ConditionalFormat {
            condition: CfCondition {
                style: Some(local(vec![(1, 1, properties)])),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut object = object(1, 10.0, None);
        object.flags = 0xf000_0000; // grows with the window
        let flag = Property {
            id: 610,
            int: Some(i32::from(shade > 0.5)),
            ..Default::default()
        };
        object.style = Some(local(vec![
            (
                1,
                1,
                vec![property(7, red(shade)), property(70, red(0.1)), flag],
            ),
            (1, 2, vec![property(7, red(shade / 2.0))]),
            (3, 1, vec![property(70, red(0.2))]),
            (4, 1, vec![property(514, red(0.3))]),
        ]));
        object.calcs.conditions = vec![
            format(vec![property(7, red(0.6)), property(70, red(0.7))]),
            format(vec![property(7, red(0.8))]),
        ];
        let mut layout = layout(id, vec![object]);
        let def = layout.def.as_mut().unwrap();
        def.background_style = Some(local(vec![(1, 1, vec![property(7, red(shade))])]));
        layout.parts[0].style = Some(local(vec![(1, 1, vec![property(7, red(shade))])]));
        layout.parts.push(Part {
            part_type: 7,
            top: 100.0,
            height: 20.0,
            style: Some(local(vec![(1, 1, vec![property(7, red(1.0 - shade))])])),
            ..Default::default()
        });
        layout.library_images.insert(
            9,
            LibraryImage {
                format: LibraryImageFormat::Png,
                data: vec![1, 2, (shade * 100.0) as u8],
            },
        );
        layout.library_images.insert(
            10,
            LibraryImage {
                format: LibraryImageFormat::Svg,
                data: br#"<svg xmlns="http://www.w3.org/2000/svg"><path class="fm_fill"/></svg>"#
                    .to_vec(),
            },
        );
        layout
    }

    /// Overwrite everything the cache keeps: a read that returns the marker
    /// reused it, one that does not resolved it again.
    fn poison(cache: &LayoutCache) {
        let mut snapshot = cache.snapshot.borrow_mut();
        for variants in snapshot.styles.values_mut() {
            for (_, style) in variants {
                style.background = Some(MARKER);
            }
        }
        for value in snapshot.properties.values_mut() {
            *value = !*value;
        }
        for bounds in snapshot.bounds.values_mut() {
            *bounds = Some(egui::Rect::ZERO);
        }
        for image in snapshot.images.values_mut() {
            *image = None;
        }
    }

    fn bytes(source: Option<ImageSource<'static>>) -> (String, Bytes) {
        match source {
            Some(ImageSource::Bytes { uri, bytes }) => (uri.into_owned(), bytes),
            _ => panic!("no image bytes"),
        }
    }

    /// Every cached read for `layout`, beside the same reads uncached.
    #[derive(Debug, PartialEq)]
    struct Reads {
        styles: Vec<ResolvedStyle>,
        flag: bool,
        bounds: Option<egui::Rect>,
        image: Option<String>,
    }

    fn reads(cache: &LayoutCache, layout: &Layout, growth: Vec2) -> [Reads; 2] {
        let object = &layout.objects.objects[0];
        let uncached = LayoutCache::uncached();
        [cache, &uncached].map(|cache| {
            let mut styles = vec![
                cache.background_style(layout, 1, 1),
                cache.part_style(layout, &layout.parts[0], 1, 1),
                cache.part_style(layout, &layout.parts[1], 1, 1),
            ];
            for conditions in [&[][..], &[0], &[1], &[0, 1], &[1, 0]] {
                for (component, state) in [(1, 1), (1, 2), (3, 1), (4, 1)] {
                    styles
                        .push(cache.formatted_style(layout, object, conditions, component, state));
                }
                for state in [1, 2] {
                    styles.push(cache.text_style(layout, object, conditions, state));
                    styles.push(cache.conditional_text_style(layout, object, conditions, state));
                }
            }
            Reads {
                styles,
                flag: cache.property_set(layout, object, 1, 1, 610),
                bounds: cache.bounds(layout, 1, growth),
                image: cache
                    .library_image(layout, 9, None)
                    .map(|image| bytes(Some(image)).0),
            }
        })
    }

    #[test]
    fn kept_work_is_the_uncached_work_and_is_reused_until_cleared() {
        let layout = styled(4, 0.9);
        let cache = LayoutCache::default();
        for _ in 0..2 {
            let [cached, uncached] = reads(&cache, &layout, vec2(5.0, 0.0));
            assert_eq!(cached, uncached);
        }
        poison(&cache);
        let [reused, uncached] = reads(&cache, &layout, vec2(5.0, 0.0));
        assert!(reused.styles.iter().all(|s| s.background == Some(MARKER)));
        assert_ne!(reused.flag, uncached.flag);
        assert_eq!(reused.bounds, Some(egui::Rect::ZERO));
        assert_eq!(reused.image, None);
        cache.clear();
        let [cached, uncached] = reads(&cache, &layout, vec2(5.0, 0.0));
        assert_eq!(cached, uncached);
    }

    #[test]
    fn a_snapshot_with_the_same_ids_replaces_the_last_only_through_clear() {
        let cache = LayoutCache::default();
        let first = styled(4, 0.9);
        let [kept, _] = reads(&cache, &first, Vec2::ZERO);
        // Same file, layout and object ids; other styles, flags, geometry
        // and image bytes.
        let mut replacement = styled(4, 0.2);
        replacement.objects.objects[0].bounds.as_mut().unwrap().top = 30.0;
        let [cached, uncached] = reads(&cache, &replacement, Vec2::ZERO);
        assert_ne!(kept, uncached);
        // Nothing but the owner's clear tells the two snapshots apart.
        assert_eq!(cached, kept);
        cache.clear();
        let [cached, uncached] = reads(&cache, &replacement, Vec2::ZERO);
        assert_eq!(cached, uncached);
    }

    #[test]
    fn another_layout_starts_over_without_a_clear() {
        let cache = LayoutCache::default();
        let [first, _] = reads(&cache, &styled(4, 0.9), Vec2::ZERO);
        let [cached, uncached] = reads(&cache, &styled(5, 0.2), Vec2::ZERO);
        assert_ne!(first, uncached);
        assert_eq!(cached, uncached);
    }

    #[test]
    fn conditional_formats_switch_per_record_without_a_clear() {
        let layout = styled(4, 0.9);
        let object = &layout.objects.objects[0];
        let cache = LayoutCache::default();
        // Records alternate between holding the first format and none.
        for conditions in [&[0][..], &[], &[0], &[], &[0, 1]] {
            assert_eq!(
                cache.formatted_style(&layout, object, conditions, 1, 1),
                super::super::formatted_style(&layout, object, conditions, 1, 1)
            );
        }
        assert_ne!(
            cache.formatted_style(&layout, object, &[0], 1, 1),
            cache.formatted_style(&layout, object, &[], 1, 1)
        );
        poison(&cache);
        for conditions in [&[0][..], &[], &[0, 1]] {
            let style = cache.formatted_style(&layout, object, conditions, 1, 1);
            assert_eq!(style.background, Some(MARKER), "{conditions:?} kept");
        }
        // A found set with more combinations than are kept stays correct.
        for index in 0..COMBINATIONS * 2 {
            let conditions = [0, index % 2, 2 + index];
            assert_eq!(
                cache.formatted_style(&layout, object, &conditions, 1, 1),
                super::super::formatted_style(&layout, object, &conditions, 1, 1)
            );
        }
        let snapshot = cache.snapshot.borrow();
        assert!(snapshot.styles.values().all(|v| v.len() <= COMBINATIONS));
    }

    #[test]
    fn bounds_survive_scrolling_and_follow_the_windows_growth() {
        let layout = styled(4, 0.9);
        let cache = LayoutCache::default();
        let narrow = Vec2::ZERO;
        let wide = vec2(80.0, 40.0);
        assert_eq!(
            cache.bounds(&layout, 1, narrow),
            super::super::resized_bounds(&layout, 1, narrow)
        );
        // Scrolling moves the origin painting adds, never the growth.
        poison(&cache);
        assert_eq!(cache.bounds(&layout, 1, narrow), Some(egui::Rect::ZERO));
        // A resize or zoom changes the growth.
        let resized = cache.bounds(&layout, 1, wide);
        assert_eq!(resized, super::super::resized_bounds(&layout, 1, wide));
        assert_ne!(resized, super::super::resized_bounds(&layout, 1, narrow));
        assert_eq!(
            cache.bounds(&layout, 1, narrow),
            super::super::resized_bounds(&layout, 1, narrow)
        );
    }

    #[test]
    fn library_images_share_their_bytes_and_keep_tints_apart() {
        let layout = styled(4, 0.9);
        let cache = LayoutCache::default();
        let read = |key, tint| bytes(cache.library_image(&layout, key, tint));
        let (red_uri, red_bytes) = read(10, Some(Color32::RED));
        let (again_uri, again_bytes) = read(10, Some(Color32::RED));
        let (blue_uri, blue_bytes) = read(10, Some(Color32::BLUE));
        let [Bytes::Shared(red_bytes), Bytes::Shared(again_bytes), Bytes::Shared(blue_bytes)] =
            [red_bytes, again_bytes, blue_bytes]
        else {
            panic!("library bytes are shared");
        };
        assert!(Arc::ptr_eq(&red_bytes, &again_bytes));
        assert_eq!(red_uri, again_uri);
        assert_ne!(red_uri, blue_uri);
        assert!(std::str::from_utf8(&red_bytes).unwrap().contains("#ff0000"));
        assert!(std::str::from_utf8(&blue_bytes)
            .unwrap()
            .contains("#0000ff"));
        let uncached = bytes(super::super::library_image(&layout, 10, Some(Color32::RED)));
        assert_eq!((red_uri, &*red_bytes), (uncached.0, &*uncached.1));
        assert!(cache.library_image(&layout, 11, None).is_none());
    }

    #[test]
    fn only_the_object_or_part_itself_is_kept() {
        let mut layout = styled(4, 0.9);
        let mut duplicate = layout.objects.objects[0].clone();
        duplicate.style = Some(local(vec![(1, 1, vec![property(7, red(0.4))])]));
        layout.objects.objects.push(duplicate);
        layout.parts[1].id = layout.parts[0].id;
        let copy = layout.objects.objects[0].clone();
        let cache = LayoutCache::default();
        for _ in 0..2 {
            for object in [&layout.objects.objects[1], &copy] {
                assert_eq!(
                    cache.formatted_style(&layout, object, &[], 1, 1),
                    layout.object_style(object, 1, 1)
                );
            }
            for part in &layout.parts {
                assert_eq!(
                    cache.part_style(&layout, part, 1, 1),
                    layout.part_style(part, 1, 1)
                );
            }
        }
        cache.formatted_style(&layout, &layout.objects.objects[0], &[], 1, 1);
        poison(&cache);
        assert_eq!(
            cache
                .formatted_style(&layout, &layout.objects.objects[0], &[], 1, 1)
                .background,
            Some(MARKER)
        );
        assert_ne!(
            cache.formatted_style(&layout, &copy, &[], 1, 1).background,
            Some(MARKER)
        );
    }
}
