//! Styled layout text stored beside its SCSU pieces, under raw key `ff`.
//!
//! The character and paragraph streams share a big-endian run table: a count,
//! `(UTF-16 offset, style index)` pairs, then a counted table of length-prefixed
//! styles. Only the observed fixed character/paragraph payloads are interpreted;
//! every payload is retained, including unsupported extensions. This is layout
//! content, not the independent styled values stored in record cells.

use std::ops::Range;

use crate::Color;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CharacterStyle {
    pub font_id: Option<u64>,
    /// Resolved through this file's font catalog during layout hydration.
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    pub color: Option<Color>,
    /// Text face bits: bold 256, italic 512, underline 1024, strike 1.
    pub faces: Option<u16>,
    /// Face bits explicitly overridden by this run. Constructed styles
    /// with faces but no mask retain their complete-face override behavior.
    pub face_mask: Option<u16>,
    /// Original payload, including unsupported mask bits and extensions.
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParagraphStyle {
    /// 1 left, 2 center, 3 right, 4 justify, as in the layout text style.
    pub text_align: Option<i32>,
    /// Leading value in `line_height_unit`; single-spaced lines use 1.
    pub line_height: Option<f32>,
    /// 0 lines, 1 points, 2 centimeters, 3 inches. None retains the legacy
    /// caller convention of a line multiplier. Unknown units stay raw.
    pub line_height_unit: Option<u8>,
    pub raw: Vec<u8>,
}

/// A half-open UTF-8 byte range, converted from the stored UTF-16 offsets.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledRun<T> {
    pub range: Range<usize>,
    pub style: T,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyledText {
    /// Original text, before merge fields or label calculations are evaluated.
    pub text: String,
    pub character_runs: Vec<StyledRun<CharacterStyle>>,
    pub paragraph_runs: Vec<StyledRun<ParagraphStyle>>,
}

impl StyledText {
    /// The same text/style stream stored in a record cell. This does not
    /// resolve font ids: those are local to the cell's file font catalog.
    pub fn from_cell(cell: &om_hbam::Cell) -> Option<Self> {
        let mut node = om_hbam::Node::default();
        match cell {
            om_hbam::Cell::Value(bytes) => node.value = Some(bytes),
            om_hbam::Cell::Nested(entries) => {
                fn insert<'a>(
                    node: &mut om_hbam::Node<'a>,
                    path: &'a [Vec<u8>],
                    value: Option<&'a [u8]>,
                ) {
                    let Some((key, rest)) = path.split_first() else {
                        node.value = value;
                        return;
                    };
                    let index = match node.children.iter().position(|n| n.segment.0 == key) {
                        Some(index) => index,
                        None => {
                            node.children.push(om_hbam::Node {
                                segment: om_hbam::KeySegment(key),
                                ..Default::default()
                            });
                            node.children.len() - 1
                        }
                    };
                    insert(&mut node.children[index], rest, value);
                }
                for (path, value) in entries {
                    insert(&mut node, path, value.as_deref());
                }
            }
        }
        Self::decode(&node)
    }

    pub fn resolve_fonts(&mut self, fonts: &std::collections::BTreeMap<u64, String>) {
        for run in &mut self.character_runs {
            run.style.font_family = run.style.font_id.and_then(|id| fonts.get(&id)).cloned();
        }
    }

    pub fn decode(node: &om_hbam::Node<'_>) -> Option<Self> {
        let text = catalog_text(node)?;
        let formatting = node.child(&[0xff]);
        let character_runs = formatting
            .and_then(|n| n.get(1))
            .and_then(|n| n.assembled())
            .and_then(|b| runs(&text, &b, character_style))
            .unwrap_or_default();
        let paragraph_runs = formatting
            .and_then(|n| n.get(2))
            .and_then(|n| n.assembled())
            .and_then(|b| runs(&text, &b, paragraph_style))
            .unwrap_or_default();
        Some(Self {
            text,
            character_runs,
            paragraph_runs,
        })
    }
}

/// A formatting sibling must not become an additional text chunk. In
/// particular, the raw `ff` key is not the serial encoding of integer 255.
fn catalog_text(node: &om_hbam::Node<'_>) -> Option<String> {
    if let Some(bytes) = node.value {
        return om_scsu::decode_scsu(bytes).ok();
    }
    let header = node.get(0)?.value?;
    if !matches!(header.len(), 4 | 8 | 12) {
        return None;
    }
    let length = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
    let mut bytes = Vec::new();
    for (i, piece) in node
        .children
        .iter()
        .filter(|n| n.id() != Some(0) && n.segment.0 != [0xff])
        .enumerate()
    {
        if piece.id() != Some(i as u64 + 1) || !piece.children.is_empty() {
            return None;
        }
        bytes.extend_from_slice(piece.value?);
    }
    (bytes.len() == length)
        .then(|| om_scsu::decode_scsu(&bytes).ok())
        .flatten()
}

fn character_style(bytes: &[u8]) -> CharacterStyle {
    let mut style = CharacterStyle {
        raw: bytes.to_vec(),
        ..Default::default()
    };
    // Both observed versions carry the same 12-byte prefix. The 13th byte
    // belongs to an extension whose meaning is not inferred here.
    if matches!(bytes.len(), 12 | 13) {
        // CharacterStyle::MergeDefault applies only masked attributes.
        // The remaining fixed-width slots hold defaults, not overrides.
        let mask = u16::from_be_bytes([bytes[0], bytes[1]]);
        style.font_id =
            (mask & 1 != 0).then(|| u64::from(u16::from_be_bytes([bytes[2], bytes[3]])));
        let face_mask = [
            (0x10, 256),
            (0x20, 512),
            (0x80, 4096),
            (0x100, 64 | 128 | 1024),
            (0x200, 8192 | 16384),
            (0x400, 1),
            (0x800, 2),
            (0x1000, 16 | 32),
            (0x2000, 4 | 8),
        ]
        .into_iter()
        .fold(0, |faces, (flag, bits)| {
            if mask & flag != 0 {
                faces | bits
            } else {
                faces
            }
        });
        if face_mask != 0 {
            style.face_mask = Some(face_mask);
            style.faces = Some(u16::from_be_bytes([bytes[4], bytes[5]]) & face_mask);
        }
        let size = u16::from_be_bytes([bytes[6], bytes[7]]);
        style.font_size = (mask & 2 != 0 && size > 0).then_some(f64::from(size));
        style.color = (mask & 4 != 0).then_some(Color {
            red: f32::from(bytes[8]) / 255.0,
            green: f32::from(bytes[9]) / 255.0,
            blue: f32::from(bytes[10]) / 255.0,
            alpha: f32::from(bytes[11]) / 255.0,
        });
    }
    style
}

fn paragraph_style(bytes: &[u8]) -> ParagraphStyle {
    let mut style = ParagraphStyle {
        raw: bytes.to_vec(),
        ..Default::default()
    };
    if matches!(bytes.len(), 35 | 36) {
        // ParagraphStyle::Read/Write use a 35-byte core: mask u16;
        // left/indent/right/leading f32; leading unit u8; before f32 + unit;
        // after f32 + unit; justification u8; tab count u8 + tab size u32.
        // Justification is byte29, not a word overlapping the after spacing.
        let mask = u16::from_be_bytes([bytes[0], bytes[1]]);
        let align = i32::from(bytes[29]);
        style.text_align = (mask & 0x80 != 0 && (1..=4).contains(&align)).then_some(align);
        let height = f32::from_be_bytes(bytes[14..18].try_into().unwrap());
        if mask & 8 != 0 && height.is_finite() && height > 0.0 && bytes[18] <= 3 {
            style.line_height = Some(height);
            style.line_height_unit = Some(bytes[18]);
        }
    }
    style
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn word(&mut self) -> Option<usize> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?) as usize)
    }
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let out = self.0.get(..len)?;
        self.0 = &self.0[len..];
        Some(out)
    }
}

fn runs<T: Clone>(
    text: &str,
    bytes: &[u8],
    decode: impl Fn(&[u8]) -> T,
) -> Option<Vec<StyledRun<T>>> {
    let mut reader = Reader(bytes);
    let count = reader.word()?;
    if count == 0 || count > reader.0.len() / 8 {
        return None;
    }
    let mut positions = Vec::with_capacity(count);
    for _ in 0..count {
        positions.push((reader.word()?, reader.word()?));
    }
    let count = reader.word()?;
    if count > reader.0.len() / 8 {
        return None;
    }
    let mut styles = Vec::with_capacity(count);
    for _ in 0..count {
        // The stored use count does not affect the run's appearance.
        reader.word()?;
        let len = reader.word()?;
        styles.push(decode(reader.take(len)?));
    }
    if !reader.0.is_empty() || positions.first()?.0 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(positions.len());
    let mut previous = 0;
    for (i, &(offset, style)) in positions.iter().enumerate() {
        if i > 0 && offset <= previous {
            return None;
        }
        let start = utf16_boundary(text, offset)?;
        let end = match positions.get(i + 1) {
            Some(&(offset, _)) => utf16_boundary(text, offset)?,
            None => text.len(),
        };
        if start > end {
            return None;
        }
        let style = styles.get(style)?.clone();
        if start != end {
            out.push(StyledRun {
                range: start..end,
                style,
            });
        }
        previous = offset;
    }
    Some(out)
}

fn utf16_boundary(text: &str, offset: usize) -> Option<usize> {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 == offset {
            return Some(byte);
        }
        utf16 += character.len_utf16();
        if utf16 > offset {
            return None;
        }
    }
    (utf16 == offset).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::{KeySegment, Node};

    fn table(positions: &[(u32, u32)], styles: &[&[u8]]) -> Vec<u8> {
        let mut out = (positions.len() as u32).to_be_bytes().to_vec();
        for &(start, style) in positions {
            out.extend(start.to_be_bytes());
            out.extend(style.to_be_bytes());
        }
        out.extend((styles.len() as u32).to_be_bytes());
        for style in styles {
            out.extend(1u32.to_be_bytes());
            out.extend((style.len() as u32).to_be_bytes());
            out.extend(*style);
        }
        out
    }

    #[test]
    fn character_mask_distinguishes_defaults_from_explicit_overrides() {
        let color = character_style(&[0, 4, 0, 19, 1, 0, 0, 12, 221, 0, 0, 255, 0]);
        assert_eq!(color.color.unwrap().red, 221.0 / 255.0);
        assert_eq!(color.font_id, None);
        assert_eq!(color.font_size, None);
        assert_eq!(color.faces, None);
        assert_eq!(color.face_mask, None);

        let size_and_bold = character_style(&[0, 0x12, 0, 19, 1, 0, 0, 14, 0, 0, 0, 255, 0]);
        assert_eq!(size_and_bold.font_size, Some(14.0));
        assert_eq!(size_and_bold.faces, Some(256));
        assert_eq!(size_and_bold.face_mask, Some(256));
        assert_eq!(size_and_bold.color, None);
        assert_eq!(size_and_bold.font_id, None);

        let normal_bold = character_style(&[0, 0x10, 0, 19, 0, 0, 0, 14, 0, 0, 0, 255]);
        assert_eq!(normal_bold.faces, Some(0));
        assert_eq!(normal_bold.face_mask, Some(256));
        let defaults = character_style(&[0; 12]);
        assert_eq!(defaults.font_id, None);
        assert_eq!(defaults.font_size, None);
        assert_eq!(defaults.color, None);
        assert_eq!(defaults.faces, None);
    }

    #[test]
    fn formatting_sibling_does_not_discard_chunked_layout_text() {
        let text: Vec<_> = b"Caption".iter().map(|b| b ^ 0x5a).collect();
        let mut header = (text.len() as u32).to_be_bytes().to_vec();
        header.extend(7u32.to_be_bytes());
        header.extend(4u32.to_be_bytes());
        let characters = table(&[(0, 0)], &[&[0, 0x17, 0, 22, 1, 0, 0, 9, 10, 20, 30, 255]]);
        let leaf = |segment, value| Node {
            segment: KeySegment(segment),
            value: Some(value),
            ..Default::default()
        };
        let node = Node {
            children: vec![
                leaf(&[0], &header),
                leaf(&[1], &text),
                Node {
                    segment: KeySegment(&[0xff]),
                    children: vec![leaf(&[1], &characters)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let decoded = StyledText::decode(&node).unwrap();
        assert_eq!(decoded.text, "Caption");
        assert_eq!(decoded.character_runs[0].style.font_id, Some(22));
        assert_eq!(decoded.character_runs[0].style.font_size, Some(9.0));
        assert_eq!(decoded.character_runs[0].style.faces, Some(256));
        assert_eq!(
            decoded.character_runs[0].style.color.unwrap().blue,
            30.0 / 255.0
        );
        assert_eq!(
            StyledText::from_cell(&om_hbam::Cell::from_node(&node)),
            Some(decoded)
        );
    }

    #[test]
    fn runs_convert_utf16_offsets_without_splitting_surrogates_or_utf8() {
        let one = [0, 7, 0, 0, 0, 0, 0, 8, 0, 0, 255, 255];
        let two = [0, 0x27, 0, 1, 2, 0, 0, 12, 255, 0, 0, 255];
        let decoded = runs(
            "a😀b",
            &table(&[(0, 0), (3, 1)], &[&one, &two]),
            character_style,
        )
        .unwrap();
        assert_eq!(decoded[0].range, 0..5);
        assert_eq!(decoded[1].range, 5..6);
        assert_eq!(decoded[1].style.faces, Some(512));
        for offsets in [
            vec![(0, 0), (2, 1)],
            vec![(0, 0), (5, 1)],
            vec![(0, 0), (0, 1)],
            vec![(1, 0)],
        ] {
            assert!(runs("a😀b", &table(&offsets, &[&one, &two]), character_style).is_none());
        }
    }

    #[test]
    fn paragraphs_keep_alignment_and_line_height_and_unknown_styles_remain_raw() {
        let mut paragraph = vec![0; 35];
        paragraph[0..2].copy_from_slice(&0x88u16.to_be_bytes());
        paragraph[14..18].copy_from_slice(&1.5f32.to_be_bytes());
        // Nonzero after spacing must not become part of justification.
        paragraph[24..28].copy_from_slice(&3.25f32.to_be_bytes());
        paragraph[28] = 1;
        paragraph[29] = 2;
        let decoded = runs("a\rb", &table(&[(0, 0)], &[&paragraph]), paragraph_style).unwrap();
        assert_eq!(decoded[0].style.text_align, Some(2));
        assert_eq!(decoded[0].style.line_height, Some(1.5));
        assert_eq!(decoded[0].style.line_height_unit, Some(0));
        paragraph[18] = 1;
        paragraph[14..18].copy_from_slice(&15f32.to_be_bytes());
        let points = paragraph_style(&paragraph);
        assert_eq!(points.line_height, Some(15.0));
        assert_eq!(points.line_height_unit, Some(1));
        paragraph[0..2].copy_from_slice(&0u16.to_be_bytes());
        let abstract_style = paragraph_style(&paragraph);
        assert_eq!(abstract_style.text_align, None);
        assert_eq!(abstract_style.line_height, None);
        let unknown = character_style(&[9, 8, 7]);
        assert_eq!(unknown.raw, [9, 8, 7]);
        assert_eq!(unknown.font_size, None);
        assert!(runs("a", &[0xff; 4], character_style).is_none());
        let truncated = table(&[(0, 1)], &[&[0; 12]]);
        assert!(runs("a", &truncated, character_style).is_none());
        assert!(runs("a", &truncated[..truncated.len() - 1], character_style).is_none());
    }
}
