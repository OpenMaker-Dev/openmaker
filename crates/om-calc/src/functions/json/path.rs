//! `keyOrIndexOrPath` (spec `calc-functions.md` 5.5, the Claris JSON page
//! and the JSON oracle answers in 9).
//!
//! A path is keys separated by `.` (empty keys between dots are skipped, so
//! `a..b`, `.a` and `a.` are `a.b`, `a` and `a`) and bracket parts: `[n]`
//! an index (`[]` is `[0]`), `[:]` the last element, `[+]` the position
//! after it, `['name']` a key that may hold dots, read up to the next `']`.
//! A key runs to the next `.` or `[`, so `a]` and `x y` are keys and
//! `a.b[2]e` reads `e` after the bracket. Anything else in brackets is a
//! syntax error at that byte offset (`* Offset 1 in "[a]"`). Keys in dot
//! notation are always keys, also on arrays (`a.1` does not index); only a
//! whole number given as the whole argument indexes the root array.
//!
//! Limits (OpenMaker's choice; FileMaker's behaviour is unmeasured,
//! calc-functions.md U-23): no value nests deeper than the reader's
//! [`STACK_LIMIT`], so a path names nothing below that depth and
//! [`set`] refuses to build anything deeper ([`SetError::TooDeep`]); and
//! one `JSONSetElement` call adds at most [`MAX_NEW_ELEMENTS`] elements to
//! arrays in all, however many indexes beyond an array's end its paths name
//! ([`SetError::TooLarge`]).

use super::reader::{STACK_LIMIT, STACK_LIMIT_MESSAGE};
use super::value::Json;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(u32),
    /// `[+]`.
    Append,
    /// `[:]`.
    Last,
}

/// The segments of a path, or the reader's message for a syntax error
/// (`* Offset 2 in "a["\n  JSON path syntax error.\n`). The whole path is
/// checked, but segments beyond the first `STACK_LIMIT` + 1 are not kept:
/// each segment goes one level down, and no value is that deep, so the
/// first ones already decide what the path names or that [`set`] fails.
pub fn parse(path: &str) -> Result<Vec<Seg>, String> {
    let b = path.as_bytes();
    let error = |at: usize| {
        Err(format!(
            "* Offset {at} in \"{path}\"\n  JSON path syntax error.\n"
        ))
    };
    let mut segs = Vec::new();
    let mut push = |seg: Seg| {
        if segs.len() <= STACK_LIMIT {
            segs.push(seg);
        }
    };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'.' => i += 1,
            b'[' => {
                i += 1;
                match b.get(i) {
                    None => return error(i),
                    Some(b'\'') => {
                        i += 1;
                        let Some(len) = path[i..].find("']") else {
                            return error(b.len());
                        };
                        push(Seg::Key(path[i..i + len].to_string()));
                        i += len + 2;
                    }
                    Some(&c @ (b'+' | b':' | b']')) => {
                        if c != b']' {
                            i += 1;
                            if b.get(i) != Some(&b']') {
                                return error(i);
                            }
                        }
                        i += 1;
                        push(match c {
                            b'+' => Seg::Append,
                            b':' => Seg::Last,
                            _ => Seg::Index(0),
                        });
                    }
                    Some(b'0'..=b'9') => {
                        let mut index: u32 = 0;
                        while let Some(&d @ b'0'..=b'9') = b.get(i) {
                            index = index.wrapping_mul(10).wrapping_add(u32::from(d - b'0'));
                            i += 1;
                        }
                        if b.get(i) != Some(&b']') {
                            return error(i);
                        }
                        i += 1;
                        push(Seg::Index(index));
                    }
                    Some(_) => return error(i),
                }
            }
            _ => {
                let start = i;
                while i < b.len() && b[i] != b'.' && b[i] != b'[' {
                    i += 1;
                }
                push(Seg::Key(path[start..i].to_string()));
            }
        }
    }
    Ok(segs)
}

/// The element a path names, if there is one, taken out of its document.
pub fn take(mut root: Json, segs: &[Seg]) -> Option<Json> {
    let mut node = &mut root;
    for seg in segs {
        node = match (seg, node) {
            (Seg::Key(k), Json::Object(m)) => m.get_mut(k)?,
            (Seg::Index(i), Json::Array(v)) => v.get_mut(*i as usize)?,
            (Seg::Last, Json::Array(v)) => v.last_mut()?,
            _ => return None,
        };
    }
    Some(std::mem::replace(node, Json::Null))
}

/// Why `JSONSetElement` could not set a path.
#[derive(Debug, PartialEq, Eq)]
pub enum SetError {
    /// A key into something other than an object or null (jsoncpp's
    /// exception text, which FileMaker returns after `? `).
    NotObject,
    /// An index into something other than an array or null.
    NotArray,
    /// The value would end up nested deeper than the reader's limit.
    TooDeep,
    /// The call would add more than [`MAX_NEW_ELEMENTS`] elements to arrays.
    TooLarge,
}

impl SetError {
    /// The text that follows `? ` in the result, or `None` for a plain `?`.
    /// A result too deep to read back reports what reading it would.
    pub fn message(&self) -> Option<&'static str> {
        match self {
            SetError::NotObject => {
                Some("in Json::Value::resolveReference(key, end): requires objectValue")
            }
            SetError::NotArray => {
                Some("in Json::Value::operator[](ArrayIndex): requires arrayValue")
            }
            SetError::TooDeep => Some(STACK_LIMIT_MESSAGE),
            SetError::TooLarge => None,
        }
    }
}

/// How many elements one `JSONSetElement` call may add to arrays, padding
/// with nulls up to the indexes it names and appending with `[+]`, over all
/// its paths together. jsoncpp keeps an array as a map from index to
/// element and pads with nulls only when it writes the text, so FileMaker
/// presumably builds any size (inference, unmeasured: U-23). OpenMaker
/// keeps every element (32 bytes, and `null,` in the text) and stops at a
/// million, more than a script adds to arrays in one call.
pub const MAX_NEW_ELEMENTS: usize = 1_000_000;

/// Sets the element a path names, creating objects and arrays where the
/// path finds null or nothing. `[:]` of an empty array sets nothing.
/// `added` counts the elements added to arrays so far in this call.
pub fn set(root: &mut Json, segs: &[Seg], value: Json, added: &mut usize) -> Result<(), SetError> {
    // The value's own nesting, below the level it is set at.
    let below = value.depth() - 1;
    let fits = |level: usize| level + below <= STACK_LIMIT;
    if !fits(1) {
        return Err(SetError::TooDeep);
    }
    let mut add = |count: usize| {
        if count > MAX_NEW_ELEMENTS - *added {
            return Err(SetError::TooLarge);
        }
        *added += count;
        Ok(())
    };
    let mut node = root;
    for (seg, level) in segs.iter().zip(2..) {
        node = match seg {
            Seg::Key(k) => {
                if let Json::Null = node {
                    *node = Json::Object(BTreeMap::new());
                }
                match node {
                    Json::Object(m) => m.entry(k.clone()).or_insert(Json::Null),
                    _ => return Err(SetError::NotObject),
                }
            }
            _ => {
                if let Json::Null = node {
                    *node = Json::Array(Vec::new());
                }
                let Json::Array(v) = node else {
                    return Err(SetError::NotArray);
                };
                match seg {
                    Seg::Index(i) => {
                        let i = *i as usize;
                        if v.len() <= i {
                            // The nulls before the element, and the element.
                            add(i - v.len())?;
                            add(1)?;
                            v.resize_with(i + 1, || Json::Null);
                        }
                        &mut v[i]
                    }
                    Seg::Append => {
                        add(1)?;
                        v.push(Json::Null);
                        v.last_mut().expect("an element was just pushed")
                    }
                    _ => match v.last_mut() {
                        Some(last) => last,
                        None => return Ok(()),
                    },
                }
            }
        };
        if !fits(level) {
            return Err(SetError::TooDeep);
        }
    }
    *node = value;
    Ok(())
}

/// Removes the element a path names; a path that names nothing changes
/// nothing.
pub fn delete(root: &mut Json, segs: &[Seg]) {
    let Some((last, parents)) = segs.split_last() else {
        return;
    };
    let mut node = root;
    for seg in parents {
        let next = match (seg, node) {
            (Seg::Key(k), Json::Object(m)) => m.get_mut(k),
            (Seg::Index(i), Json::Array(v)) => v.get_mut(*i as usize),
            (Seg::Last, Json::Array(v)) => v.last_mut(),
            _ => None,
        };
        let Some(next) = next else {
            return;
        };
        node = next;
    }
    match (last, node) {
        (Seg::Key(k), Json::Object(m)) => {
            m.remove(k);
        }
        (Seg::Index(i), Json::Array(v)) if (*i as usize) < v.len() => {
            v.remove(*i as usize);
        }
        (Seg::Last, Json::Array(v)) => {
            v.pop();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Seg {
        Seg::Key(k.into())
    }

    #[test]
    fn syntax() {
        assert_eq!(parse("").unwrap(), []);
        assert_eq!(parse(".").unwrap(), []);
        assert_eq!(parse("a..b.").unwrap(), [key("a"), key("b")]);
        assert_eq!(
            parse("a.b[2]e[:]['c.d'][+][]").unwrap(),
            [
                key("a"),
                key("b"),
                Seg::Index(2),
                key("e"),
                Seg::Last,
                key("c.d"),
                Seg::Append,
                Seg::Index(0)
            ]
        );
        assert_eq!(parse("a['q'r']").unwrap(), [key("a"), key("q'r")]);
        assert_eq!(parse("a]").unwrap(), [key("a]")]);
        for (bad, at) in [
            ("a[", 2),
            ("[a]", 1),
            ("['a", 3),
            ("a.b[0", 5),
            ("a.b[ 1 ]", 4),
            ("a.b[1.5]", 5),
            ("[-1]", 1),
        ] {
            assert_eq!(
                parse(bad).unwrap_err(),
                format!("* Offset {at} in \"{bad}\"\n  JSON path syntax error.\n")
            );
        }
    }

    #[test]
    fn setting() {
        let mut doc = Json::Null;
        let added = &mut 0;
        set(&mut doc, &parse("a[1]").unwrap(), Json::Int(9), added).unwrap();
        assert_eq!(super::super::value::compact(&doc), "{\"a\":[null,9]}");
        assert_eq!(
            set(&mut doc, &parse("a.b").unwrap(), Json::Null, added),
            Err(SetError::NotObject)
        );
        assert_eq!(
            set(&mut doc, &parse("[0]").unwrap(), Json::Null, added),
            Err(SetError::NotArray)
        );
        let mut empty = Json::Array(vec![]);
        set(&mut empty, &parse("[:].a").unwrap(), Json::Int(1), added).unwrap();
        assert_eq!(empty, Json::Array(vec![]));
        delete(&mut doc, &parse("a[0]").unwrap());
        assert_eq!(super::super::value::compact(&doc), "{\"a\":[9]}");
    }
}
