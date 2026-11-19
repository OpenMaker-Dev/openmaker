//! Images embedded in the file's library, rather than application resources.
use std::collections::{BTreeMap, BTreeSet};

use om_hbam::KeyTree;

use crate::ObjectStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryImageFormat {
    Svg,
    Png,
    Jpeg,
    Gif,
    Bmp,
    Tiff,
    WebP,
}

impl LibraryImageFormat {
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Svg => "image/svg+xml",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::WebP => "image/webp",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::WebP => "webp",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryImage {
    pub format: LibraryImageFormat,
    /// The original encoded image stream. No vendor application asset is used.
    pub data: Vec<u8>,
}

pub(crate) fn decode_images(
    tree: &KeyTree<'_>,
    objects: &ObjectStore,
) -> BTreeMap<u64, LibraryImage> {
    let keys: BTreeSet<_> = objects
        .objects
        .iter()
        .flat_map(|object| {
            [
                object.button.as_ref().map(|button| button.library_key),
                object.graphic.as_ref().map(|graphic| graphic.library_key),
            ]
            .into_iter()
            .flatten()
            .filter(|key| *key != 0)
        })
        .collect();
    keys.into_iter()
        .filter_map(|key| {
            let member = tree.at(&[31, 5, key])?;
            // DBLibraryMember stores each binary stream directly under its member
            // using the four-byte QuadChar as the key. Chunked HBAM values are
            // assembled here before passing their exact image bytes to the view.
            // Prefer a vector original to any raster preview present in the file.
            let image = member
                .get(u64::from(u32::from_be_bytes(*b"SVG ")))
                .and_then(|node| node.assembled())
                .and_then(|data| decode_stream(&data))
                .or_else(|| {
                    member
                        .children
                        .iter()
                        .filter_map(|node| {
                            let data = node.assembled()?;
                            decode_stream(&data)
                        })
                        .next()
                })?;
            Some((key, image))
        })
        .collect()
}

fn decode_stream(data: &[u8]) -> Option<LibraryImage> {
    let format = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        LibraryImageFormat::Png
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        LibraryImageFormat::Jpeg
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        LibraryImageFormat::Gif
    } else if data.starts_with(b"BM") {
        LibraryImageFormat::Bmp
    } else if data.starts_with(b"II\x2a\0") || data.starts_with(b"MM\0\x2a") {
        LibraryImageFormat::Tiff
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        LibraryImageFormat::WebP
    } else {
        let text = std::str::from_utf8(data)
            .ok()?
            .trim_start_matches('\u{feff}')
            .trim_start();
        if !text.starts_with("<svg") && !text.starts_with("<?xml") && !text.starts_with("<!--") {
            return None;
        }
        let start = text.find("<svg")?;
        if !text
            .as_bytes()
            .get(start + 4)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'>' | b'/'))
        {
            return None;
        }
        LibraryImageFormat::Svg
    };
    Some(LibraryImage {
        format,
        data: data.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ButtonObject, Layout, LayoutObject};
    use om_hbam::{KeySegment, Node};

    fn node<'a>(key: &'a [u8], value: Option<&'a [u8]>, children: Vec<Node<'a>>) -> Node<'a> {
        Node {
            segment: KeySegment(key),
            value,
            push: !children.is_empty(),
            children,
        }
    }

    #[test]
    fn library_images_are_referenced_and_chunked_originals_are_reassembled() {
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><path d='M0 0L8 4L0 8'/></svg>";
        let length = (svg.len() as u32).to_be_bytes();
        let tree = KeyTree {
            root: node(
                &[],
                None,
                vec![node(
                    &[31],
                    None,
                    vec![node(
                        &[5],
                        None,
                        vec![
                            node(
                                &[1],
                                None,
                                vec![node(
                                    b"SVG ",
                                    None,
                                    vec![
                                        node(&[0], Some(&length), vec![]),
                                        node(&[1], Some(&svg[..30]), vec![]),
                                        node(&[2], Some(&svg[30..]), vec![]),
                                    ],
                                )],
                            ),
                            node(&[2], None, vec![node(b"SVG ", Some(b"<svg/>"), vec![])]),
                        ],
                    )],
                )],
            ),
        };
        let objects = ObjectStore {
            objects: vec![LayoutObject {
                button: Some(ButtonObject {
                    library_key: 1,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let images = decode_images(&tree, &objects);
        assert_eq!(images.len(), 1);
        assert_eq!(images[&1].format, LibraryImageFormat::Svg);
        assert_eq!(images[&1].data, svg);
    }

    #[test]
    fn decode_from_tree_hydrates_button_images_and_tolerates_missing_library() {
        // One button object (id 7) references library key 1.
        let store = [0x12, 9, 8, 10, 16, 7, 0x82, 1, 2, 0x20, 1];
        let image = b"<?xml version='1.0'?><svg xmlns='http://www.w3.org/2000/svg'></svg>";
        let layout = node(
            &[4],
            None,
            vec![node(
                &[5],
                None,
                vec![node(&[2], None, vec![node(&[7], Some(&store), vec![])])],
            )],
        );
        let mut tree = KeyTree {
            root: node(&[], None, vec![layout]),
        };
        assert!(Layout::decode_from_tree(&tree, 2)
            .unwrap()
            .unwrap()
            .library_images
            .is_empty());
        tree.root.children.push(node(
            &[31],
            None,
            vec![node(
                &[5],
                None,
                vec![node(&[1], None, vec![node(b"SVG ", Some(image), vec![])])],
            )],
        ));
        let layout = Layout::decode_from_tree(&tree, 2).unwrap().unwrap();
        assert_eq!(layout.library_images[&1].data, image);
        assert_eq!(
            layout.library_images[&1].format.mime_type(),
            "image/svg+xml"
        );
    }

    #[test]
    fn unknown_streams_and_malformed_image_signatures_are_not_images() {
        for value in [
            b"metadata".as_slice(),
            b"<svgarbage>",
            b"\xff<svg>",
            b"RIFF1234NOPE",
        ] {
            assert_eq!(decode_stream(value), None);
        }
        for (bytes, format) in [
            (b"<svg/>".as_slice(), LibraryImageFormat::Svg),
            (b"\x89PNG\r\n\x1a\ndata", LibraryImageFormat::Png),
            (b"\xff\xd8\xffdata", LibraryImageFormat::Jpeg),
            (b"GIF89adata", LibraryImageFormat::Gif),
            (b"BMdata", LibraryImageFormat::Bmp),
            (b"II\x2a\0data", LibraryImageFormat::Tiff),
            (b"RIFF1234WEBPdata", LibraryImageFormat::WebP),
        ] {
            assert_eq!(decode_stream(bytes).unwrap().format, format);
        }
    }
}
