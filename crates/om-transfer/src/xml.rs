//! A small XML reader for the documents imports read (FMPXMLRESULT, the
//! parts of an Excel workbook) and the text escaping of Excel parts. It
//! reads elements, attributes, character data, CDATA and the predefined
//! and numeric entities; it skips declarations, comments, processing
//! instructions and DOCTYPEs. It does not validate.
//!
//! Hostile documents: events stream one at a time (nothing is held but
//! the current one), elements nest at most [`MAX_DEPTH`] deep, and only
//! the predefined entities and character references are known, so a
//! DOCTYPE's entities are never expanded (using one is an error).

/// The deepest element nesting read: libxml2's default limit.
pub(crate) const MAX_DEPTH: usize = 256;

/// The longest entity reference read (`&#x0010FFFF;` is 11 characters).
const MAX_ENTITY: usize = 32;

/// Escape character data as Excel parts do.
pub(crate) fn escape_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
}

/// One piece of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// A start tag; `empty` for `<a/>` (no end event follows).
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        empty: bool,
    },
    End(String),
    Text(String),
}

impl Event {
    /// The local name (after a namespace prefix).
    pub fn local(name: &str) -> &str {
        name.rsplit(':').next().unwrap_or(name)
    }
}

/// Not well-formed enough to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct XmlError;

fn unescape(raw: &str) -> Result<String, XmlError> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after
            .char_indices()
            .take(MAX_ENTITY)
            .find(|&(_, c)| c == ';')
            .ok_or(XmlError)?
            .0;
        let entity = &after[..end];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code =
                    if let Some(hex) = entity.strip_prefix("#x").or(entity.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).map_err(|_| XmlError)?
                    } else if let Some(dec) = entity.strip_prefix('#') {
                        dec.parse().map_err(|_| XmlError)?
                    } else {
                        return Err(XmlError);
                    };
                out.push(char::from_u32(code).ok_or(XmlError)?);
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The events of a document, in order; an error ends them.
pub(crate) fn events(doc: &str) -> Events<'_> {
    Events {
        rest: doc,
        depth: 0,
    }
}

/// The events of a document, read as they are asked for (module docs).
pub(crate) struct Events<'a> {
    rest: &'a str,
    /// Open elements.
    depth: usize,
}

impl Iterator for Events<'_> {
    type Item = Result<Event, XmlError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            match self.step() {
                Ok(Some(event)) => return Some(Ok(event)),
                Ok(None) => {}
                Err(e) => {
                    self.rest = "";
                    return Some(Err(e));
                }
            }
        }
    }
}

impl Events<'_> {
    /// Read one piece of the document: its event, or `None` for a piece
    /// skipped (a comment, a declaration).
    fn step(&mut self) -> Result<Option<Event>, XmlError> {
        let rest = self.rest;
        if let Some(body) = rest.strip_prefix("<![CDATA[") {
            let end = body.find("]]>").ok_or(XmlError)?;
            self.rest = &body[end + 3..];
            Ok(Some(Event::Text(body[..end].to_owned())))
        } else if let Some(body) = rest.strip_prefix("<!--") {
            let end = body.find("-->").ok_or(XmlError)?;
            self.rest = &body[end + 3..];
            Ok(None)
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            let end = rest.find('>').ok_or(XmlError)?;
            self.rest = &rest[end + 1..];
            Ok(None)
        } else if let Some(body) = rest.strip_prefix("</") {
            let end = body.find('>').ok_or(XmlError)?;
            self.rest = &body[end + 1..];
            self.depth = self.depth.saturating_sub(1);
            Ok(Some(Event::End(body[..end].trim().to_owned())))
        } else if let Some(body) = rest.strip_prefix('<') {
            let end = tag_end(body).ok_or(XmlError)?;
            let mut tag = &body[..end];
            let empty = tag.ends_with('/');
            if empty {
                tag = &tag[..tag.len() - 1];
            } else {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(XmlError);
                }
            }
            let (name, mut attrs_text) = match tag.find(|c: char| c.is_whitespace()) {
                Some(i) => (&tag[..i], &tag[i..]),
                None => (tag, ""),
            };
            let mut attrs = Vec::new();
            loop {
                attrs_text = attrs_text.trim_start();
                if attrs_text.is_empty() {
                    break;
                }
                let eq = attrs_text.find('=').ok_or(XmlError)?;
                let key = attrs_text[..eq].trim().to_owned();
                let after = attrs_text[eq + 1..].trim_start();
                let quote = after.chars().next().ok_or(XmlError)?;
                if quote != '"' && quote != '\'' {
                    return Err(XmlError);
                }
                let close = after[1..].find(quote).ok_or(XmlError)?;
                attrs.push((key, unescape(&after[1..1 + close])?));
                attrs_text = &after[close + 2..];
            }
            self.rest = &body[end + 1..];
            Ok(Some(Event::Start {
                name: name.to_owned(),
                attrs,
                empty,
            }))
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            self.rest = &rest[end..];
            Ok(Some(Event::Text(unescape(&rest[..end])?)))
        }
    }
}

/// The end of a start tag, past quoted attribute values.
fn tag_end(body: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in body.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

pub(crate) fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| Event::local(k) == name)
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_elements_text_and_entities() {
        let doc =
            "<?xml version=\"1.0\"?><a x=\"1 &amp; 2\"><b/>t&lt;&#128512;<![CDATA[<raw>]]></a>";
        let ev: Vec<Event> = events(doc).collect::<Result<_, _>>().unwrap();
        assert_eq!(
            ev,
            vec![
                Event::Start {
                    name: "a".into(),
                    attrs: vec![("x".into(), "1 & 2".into())],
                    empty: false
                },
                Event::Start {
                    name: "b".into(),
                    attrs: vec![],
                    empty: true
                },
                Event::Text("t<😀".into()),
                Event::Text("<raw>".into()),
                Event::End("a".into()),
            ]
        );
        assert!(events("<a x=\"1></a>").any(|e| e.is_err()));
    }

    #[test]
    fn nesting_and_entities_are_limited() {
        let deep = |n: usize| format!("{}{}", "<a>".repeat(n), "</a>".repeat(n));
        assert!(events(&deep(MAX_DEPTH)).all(|e| e.is_ok()));
        assert!(events(&deep(MAX_DEPTH + 1)).any(|e| e.is_err()));
        // Empty elements and closed ones do not nest.
        assert!(events(&"<a/><b></b>".repeat(10 * MAX_DEPTH)).all(|e| e.is_ok()));
        // An error ends the events.
        let mut ev = events("<a>&nope;</a><b/>");
        assert!(ev.next().unwrap().is_ok());
        assert!(ev.next().unwrap().is_err());
        assert!(ev.next().is_none());
        // A reference with no end nearby.
        let long = format!("<a>&{}</a>", "x".repeat(10_000));
        assert!(events(&long).any(|e| e.is_err()));
        assert_eq!(unescape("&#x0010FFFF;").unwrap(), "\u{10ffff}");
    }
}
