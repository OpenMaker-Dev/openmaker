//! Value nodes as the catalog stores them (file-format.md 4.3).
//!
//! A value is a plain leaf, or, when it is longer than 1000 bytes (and for
//! text whose compressed form is not one byte per character), a node whose
//! child `00` is a header (u32 byte length; text adds the UTF-16 length, and
//! long text a third word, `4`) and whose children `1..n` are 1000-byte
//! pieces keyed as serial keys.

use om_hbam::{serial, OwnedNode};
use std::borrow::Cow;

/// The longest value stored in one leaf.
const PIECE: usize = 1000;

/// The bytes of a value node, reassembling a chunked value. `None` for a
/// node that is neither a plain value nor a well-formed chunked one.
pub fn node_bytes(node: &OwnedNode) -> Option<Cow<'_, [u8]>> {
    if let Some(value) = &node.value {
        return node.children.is_empty().then_some(Cow::Borrowed(value));
    }
    let (header, pieces) = node.children.split_first()?;
    let header_value = header.value.as_deref()?;
    if header.segment != [0] || !header.children.is_empty() || header_value.len() < 4 {
        return None;
    }
    let total = u32::from_be_bytes(header_value[..4].try_into().ok()?) as usize;
    let mut out = Vec::with_capacity(total);
    for (n, piece) in pieces.iter().enumerate() {
        if serial::decode(&piece.segment) != Some(n as u64 + 1) || !piece.children.is_empty() {
            return None;
        }
        out.extend_from_slice(piece.value.as_deref()?);
    }
    (out.len() == total).then_some(Cow::Owned(out))
}

/// The text of a value node (XOR 0x5A masked SCSU).
pub fn node_text(node: &OwnedNode) -> Option<String> {
    om_scsu::decode_scsu(&node_bytes(node)?).ok()
}

/// A binary value as FileMaker writes it: plain up to 1000 bytes, chunked
/// above with a 4-byte header.
pub fn bytes_node(segment: &[u8], bytes: &[u8]) -> OwnedNode {
    if bytes.len() <= PIECE {
        return OwnedNode::leaf(segment, bytes);
    }
    chunked(segment, (bytes.len() as u32).to_be_bytes().to_vec(), bytes)
}

/// A text value as FileMaker writes it: plain when its masked SCSU form has
/// one byte per UTF-16 unit and fits one leaf, otherwise chunked with a
/// header of the byte length and the UTF-16 length (and a third word, 4, when
/// it needs more than one piece).
pub fn text_node(segment: &[u8], text: &str) -> OwnedNode {
    text_form(
        segment,
        &om_scsu::encode_scsu(text),
        text.encode_utf16().count(),
    )
}

/// [`text_node`] of text already in its masked SCSU form `bytes`, which are
/// kept as they are (another file's encoder may have chosen other bytes for
/// the same text). Bytes that are not text stay one leaf, as given.
pub fn encoded_text_node(segment: &[u8], bytes: &[u8]) -> OwnedNode {
    match om_scsu::decode_scsu(bytes) {
        Ok(text) => text_form(segment, bytes, text.encode_utf16().count()),
        Err(_) => OwnedNode::leaf(segment, bytes),
    }
}

fn text_form(segment: &[u8], bytes: &[u8], units: usize) -> OwnedNode {
    if bytes.len() == units && bytes.len() <= PIECE {
        return OwnedNode::leaf(segment, bytes);
    }
    let mut header = (bytes.len() as u32).to_be_bytes().to_vec();
    header.extend((units as u32).to_be_bytes());
    if bytes.len() > PIECE {
        header.extend(4u32.to_be_bytes());
    }
    chunked(segment, header, bytes)
}

fn chunked(segment: &[u8], header: Vec<u8>, bytes: &[u8]) -> OwnedNode {
    let mut children = vec![OwnedNode::leaf([0], header)];
    for (i, piece) in bytes.chunks(PIECE).enumerate() {
        children.push(OwnedNode::leaf(serial::encode(i as u64 + 1), piece));
    }
    OwnedNode::list(segment, children)
}

/// A copy of `node` under another segment.
pub fn renamed(node: &OwnedNode, segment: &[u8]) -> OwnedNode {
    OwnedNode {
        segment: segment.to_vec(),
        ..node.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_chunk_above_one_piece() {
        let short = bytes_node(&[6], &[1, 2, 3]);
        assert_eq!(short.value.as_deref(), Some(&[1, 2, 3][..]));
        let long: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let node = bytes_node(&[6], &long);
        assert_eq!(node.children.len(), 4);
        assert_eq!(
            node.children[0].value.as_deref(),
            Some(&[0, 0, 9, 0xc4][..])
        );
        assert_eq!(node.children[3].value.as_ref().unwrap().len(), 500);
        assert_eq!(node_bytes(&node).unwrap(), long);
    }

    #[test]
    fn multi_byte_text_is_chunked_with_its_utf16_length() {
        // Latin-1 text compresses to one byte per character and stays plain.
        assert!(text_node(&[1], "Bestätigt").value.is_some());
        let node = text_node(&[1], "タスク");
        let header = node.children[0].value.as_deref().unwrap();
        assert_eq!(header.len(), 8);
        assert_eq!(&header[4..], &3u32.to_be_bytes());
        assert_eq!(node_text(&node).unwrap(), "タスク");
        assert!(text_node(&[1], "$x").value.is_some());
        let long = "x".repeat(1500);
        let node = text_node(&[1], &long);
        assert_eq!(node.children[0].value.as_ref().unwrap().len(), 12);
        assert_eq!(node_text(&node).unwrap(), long);
    }

    #[test]
    fn twelve_byte_text_headers_assemble() {
        let node = OwnedNode::list(
            [1],
            vec![
                OwnedNode::leaf([0], [0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 4]),
                OwnedNode::leaf([1], [b'h' ^ 0x5a, b'i' ^ 0x5a]),
            ],
        );
        assert_eq!(node_text(&node).unwrap(), "hi");
    }
}
