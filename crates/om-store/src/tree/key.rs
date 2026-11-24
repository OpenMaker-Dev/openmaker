//! Keys: paths encoded so that plain byte order is the key tree's order
//! (`docs/server-storage-format.md` section 2.1). Each segment's bytes are
//! written with `00` as `00 ff`, then `00 01`. A node's key is a prefix of
//! every key below it, so a subtree is one contiguous range of keys in
//! depth-first order, and siblings sort by their segments' bytes, a shorter
//! segment before a longer one it begins (`docs/re/file-format.md` 3.6).

use std::fmt;

use super::MAX_DEPTH;

const ESCAPE: u8 = 0xff;
const END: u8 = 0x01;

/// Appends the encoding of one segment.
pub fn push_segment(out: &mut Vec<u8>, segment: &[u8]) {
    for &b in segment {
        out.push(b);
        if b == 0 {
            out.push(ESCAPE);
        }
    }
    out.extend_from_slice(&[0, END]);
}

/// The key of a path. The empty path, the root, has the empty key.
pub fn encode<S: AsRef<[u8]>>(path: &[S]) -> Vec<u8> {
    let mut out = Vec::new();
    for segment in path {
        push_segment(&mut out, segment.as_ref());
    }
    out
}

/// The length of the key of a path, without building it.
pub fn encoded_len<S: AsRef<[u8]>>(path: &[S]) -> usize {
    path.iter()
        .map(|s| {
            let s = s.as_ref();
            s.len() + s.iter().filter(|&&b| b == 0).count() + 2
        })
        .sum()
}

/// Whether `bytes` are a whole key (`complete`), or the start of one, of a
/// path within [`MAX_DEPTH`] segments.
pub(crate) fn well_formed(bytes: &[u8], complete: bool) -> bool {
    let mut i = 0;
    let mut boundary = true;
    let mut segments = 0;
    while i < bytes.len() {
        if bytes[i] == 0 {
            match bytes.get(i + 1) {
                Some(&ESCAPE) => boundary = false,
                Some(&END) => {
                    boundary = true;
                    segments += 1;
                }
                Some(_) => return false,
                None => return !complete && segments < MAX_DEPTH,
            }
            i += 2;
        } else {
            boundary = false;
            i += 1;
        }
    }
    if complete {
        boundary && segments > 0 && segments <= MAX_DEPTH
    } else {
        // More of the key follows, so at least one more segment.
        segments < MAX_DEPTH
    }
}

/// Where the segment that starts at `from` in `key` ends, its terminator
/// included.
pub(crate) fn segment_end(key: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < key.len() {
        if key[i] == 0 {
            if key[i + 1] == END {
                return Some(i + 2);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

/// The segments of a well-formed key.
pub fn decode(key: &[u8]) -> Option<Vec<Vec<u8>>> {
    if !well_formed(key, true) && !key.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    let mut segment = Vec::new();
    let mut i = 0;
    while i < key.len() {
        if key[i] == 0 {
            if key[i + 1] == END {
                out.push(std::mem::take(&mut segment));
            } else {
                segment.push(0);
            }
            i += 2;
        } else {
            segment.push(key[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The least key above every key that begins with `key`, a node's key: the
/// exclusive end of its subtree. `None` for the root, whose subtree is
/// everything.
pub fn subtree_end(key: &[u8]) -> Option<Vec<u8>> {
    // A node's key ends in `00 01`; `00 02` is above everything that
    // continues it and below the next sibling, which differs earlier.
    let (&last, rest) = key.split_last()?;
    debug_assert_eq!(last, END);
    let mut end = rest.to_vec();
    end.push(END + 1);
    Some(end)
}

/// A key the tree holds, with its path.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(pub(crate) Vec<u8>);

impl Key {
    /// The encoded bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// The path's segments.
    pub fn path(&self) -> Vec<Vec<u8>> {
        // Keys reach a `Key` only after a well-formedness check.
        decode(&self.0).unwrap_or_default()
    }

    /// The number of segments.
    pub fn depth(&self) -> usize {
        let mut n = 0;
        let mut at = 0;
        while let Some(end) = segment_end(&self.0, at) {
            n += 1;
            at = end;
        }
        n
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path: Vec<String> = self
            .path()
            .iter()
            .map(|s| s.iter().map(|b| format!("{b:02x}")).collect())
            .collect();
        write!(f, "Key(/{})", path.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoding sorts as the key tree does: Rust's order on a list of
    /// byte strings is the key tree's depth-first order (a parent before
    /// its children, siblings by their bytes, a shorter segment first).
    #[test]
    fn keys_sort_as_the_key_tree() {
        let segments: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![0, 0],
            vec![0, 1],
            vec![0, 0xff],
            vec![1],
            vec![1, 0],
            vec![1, 0, 2],
            vec![1, 1],
            vec![0xff],
            vec![0xff, 0],
        ];
        let mut paths: Vec<Vec<Vec<u8>>> = vec![vec![]];
        for a in &segments {
            paths.push(vec![a.clone()]);
            for b in &segments {
                paths.push(vec![a.clone(), b.clone()]);
                paths.push(vec![a.clone(), b.clone(), vec![7]]);
            }
        }
        paths.sort();
        paths.dedup();
        let keys: Vec<Vec<u8>> = paths.iter().map(|p| encode(p)).collect();
        for (i, pair) in keys.windows(2).enumerate() {
            assert!(pair[0] < pair[1], "{:?} !< {:?}", paths[i], paths[i + 1]);
        }
        for (path, key) in paths.iter().zip(&keys) {
            assert_eq!(&decode(key).unwrap(), path);
            assert_eq!(encoded_len(path), key.len());
            // The subtree is the range [key, end): exactly the paths below.
            if let Some(end) = subtree_end(key) {
                for (other, other_key) in paths.iter().zip(&keys) {
                    let below = other.starts_with(path);
                    let inside = other_key >= key && *other_key < end;
                    assert_eq!(below, inside, "{path:?} / {other:?}");
                }
            }
        }
    }

    #[test]
    fn malformed_keys_are_refused() {
        assert!(well_formed(b"a\x00\x01", true));
        assert!(!well_formed(b"a\x00\x02", true));
        assert!(!well_formed(b"a", true));
        assert!(well_formed(b"a", false));
        assert!(well_formed(b"a\x00", false));
        assert!(!well_formed(b"a\x00", true));
        assert!(!well_formed(b"", true));
        assert!(decode(b"a\x00\x05").is_none());
        // At most 64 segments, whole or begun.
        let deepest = b"\x00\x01".repeat(MAX_DEPTH);
        assert!(well_formed(&deepest, true));
        assert!(!well_formed(&b"\x00\x01".repeat(MAX_DEPTH + 1), true));
        assert!(!well_formed(&deepest, false));
        assert!(well_formed(&b"\x00\x01".repeat(MAX_DEPTH - 1), false));
    }
}
