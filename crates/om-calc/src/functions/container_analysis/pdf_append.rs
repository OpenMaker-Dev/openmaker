//! The byte-preserving copy used to append page-only reports. Its input
//! profile is the classic, single-revision PDF 1.7 syntax our writers emit.
//! The binary marker selects the profile. The file itself must satisfy it.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::ops::Range;

use super::pdf_assemble::{
    finish_pdf, Planned, Writer, CATALOG, FIRST_PAGE, MARKER, MAX_DEPTH, MAX_OBJECTS, MAX_PAGES,
    PAGE_TREE,
};

const HEADER: &[u8] = b"%PDF-1.7\n%";

struct Source<'a> {
    objects: Vec<Option<&'a [u8]>>,
    catalog: u32,
    tree: u32,
    pages: Vec<u32>,
}

impl<'a> Source<'a> {
    fn read(data: &'a [u8]) -> Option<Self> {
        let body = data
            .strip_prefix(HEADER)?
            .strip_prefix(MARKER)?
            .strip_prefix(b"\n")?;
        let header_end = data.len() - body.len();
        let data = data.trim_ascii_end();
        let footer = data.strip_suffix(b"\n%%EOF")?;
        let footer_at = footer
            .windows(b"\nstartxref\n".len())
            .rposition(|w| w == b"\nstartxref\n")?;
        let offset = unsigned(&footer[footer_at + b"\nstartxref\n".len()..])? as usize;
        let table = footer.get(offset..footer_at)?.strip_prefix(b"xref\n0 ")?;
        let line_end = table.iter().position(|&b| b == b'\n')?;
        let size = unsigned(&table[..line_end])?;
        let table = &table[line_end + 1..];
        if !(2..=MAX_OBJECTS).contains(&size) {
            return None;
        }
        let (rows, trailer) = table.split_at_checked(size as usize * 20)?;
        let mut offsets = Vec::new();
        for (number, row) in (0..size).zip(rows.chunks_exact(20)) {
            let at = unsigned(&row[..10])? as usize;
            let generation = u16::try_from(unsigned(&row[11..16])?).ok()?;
            if row[10] != b' ' || row[16] != b' ' || !matches!(&row[18..], b" \n" | b"\r\n") {
                return None;
            }
            match row[17] {
                b'f' if number != 0 || generation == u16::MAX => {}
                b'n' if number != 0 && generation == 0 && (header_end..offset).contains(&at) => {
                    offsets.push((at, number));
                }
                _ => return None,
            }
        }
        let mut catalog = None;
        let mut declared_size = None;
        entries(trailer.strip_prefix(b"trailer\n")?, |key, value| {
            match key {
                b"Size" => declared_size = unsigned(value),
                b"Root" => catalog = reference(value),
                _ => return None,
            }
            Some(())
        })?;
        let catalog = catalog?;
        if declared_size != Some(size) {
            return None;
        }

        // The table must partition the body into objects with exactly the
        // indexed headers. This rejects unindexed objects, overlapping
        // offsets, and any xref that would need a recovery scan.
        offsets.sort_unstable();
        let first = offsets.first()?.0;
        if !data[header_end..first].iter().copied().all(space) {
            return None;
        }
        let mut objects = vec![None; size as usize];
        for (index, &(at, id)) in offsets.iter().enumerate() {
            let end = offsets.get(index + 1).map_or(offset, |next| next.0);
            let mut reader = Reader::new(data.get(at..end)?);
            if !data[at].is_ascii_digit()
                || unsigned(reader.word()?)? != id
                || reader.word()? != b"0"
                || reader.word()? != b"obj"
            {
                return None;
            }
            reader.skip_space();
            let start = reader.at;
            reader.body()?;
            let body_end = reader.at;
            if reader.word()? != b"endobj" || !reader.done() {
                return None;
            }
            objects[id as usize] = Some(&data[at + start..at + body_end]);
        }
        let object = |id: u32| objects.get(id as usize).copied().flatten();
        let mut tree = None;
        let mut catalog_type = false;
        entries(object(catalog)?, |key, value| {
            match (key, value) {
                (b"Type", b"/Catalog") => catalog_type = true,
                (b"Pages", value) => tree = reference(value),
                _ => return None,
            }
            Some(())
        })?;
        let tree = tree.filter(|&id| id != catalog && catalog_type)?;
        let mut pages = None;
        let mut count = None;
        let mut tree_type = false;
        entries(object(tree)?, |key, value| {
            match (key, value) {
                (b"Type", b"/Pages") => tree_type = true,
                (b"Kids", value) => pages = kids(value),
                (b"Count", value) => count = unsigned(value),
                _ => return None,
            }
            Some(())
        })?;
        let pages = pages?;
        if !tree_type || count? as usize != pages.len() {
            return None;
        }
        for &page in &pages {
            let mut page_type = false;
            let mut parent = None;
            let mut media = false;
            entries(object(page)?, |key, value| {
                match key {
                    b"Type" => page_type = value == b"/Page",
                    b"Parent" => parent = reference(value),
                    b"MediaBox" => media = media_box(value),
                    b"Annots" => return None,
                    _ => {}
                }
                Some(())
            })?;
            if !page_type || parent != Some(tree) || !media {
                return None;
            }
        }
        Some(Self {
            objects,
            catalog,
            tree,
            pages,
        })
    }
}

/// A profile check only. The copy also accounts for every object and reference.
pub(super) fn page_count(data: &[u8]) -> Option<usize> {
    Some(Source::read(data)?.pages.len())
}

pub(super) fn assemble(parts: &[impl AsRef<[u8]>], plan: &[Planned]) -> Option<Vec<u8>> {
    let sources = parts
        .iter()
        .map(|part| Source::read(part.as_ref()))
        .collect::<Option<Vec<_>>>()?;
    let total: usize = sources.iter().map(|source| source.pages.len()).sum();
    if total == 0 || plan.len() != total {
        return None;
    }
    let mut ids: Vec<_> = sources
        .iter()
        .map(|source| vec![0; source.objects.len()])
        .collect();
    let mut queue = VecDeque::new();
    for (index, planned) in plan.iter().enumerate() {
        let source = planned.source as usize;
        let id = *sources.get(source)?.pages.get(planned.page as usize)?;
        let mapped = &mut ids[source][id as usize];
        if *mapped != 0 || planned.overlay.is_some() || !planned.boxes.is_empty() {
            return None;
        }
        *mapped = FIRST_PAGE + index as u32;
        queue.push_back((source, id, true));
    }

    let mut out = [HEADER, MARKER, b"\n"].concat();
    let mut offsets = vec![0; FIRST_PAGE as usize + total];
    while let Some((source, id, is_page)) = queue.pop_front() {
        let source_pdf = &sources[source];
        let body = source_pdf.objects[id as usize]?;
        let number = ids[source][id as usize] as usize;
        offsets[number] = out.len();
        let _ = writeln!(Writer(&mut out), "{number} 0 obj");
        let mut copied = 0;
        let mut rewrite = |span: Range<usize>, target, parent| {
            let mapped = if is_page && parent {
                PAGE_TREE
            } else {
                if target == source_pdf.catalog || target == source_pdf.tree {
                    return None;
                }
                source_pdf.objects.get(target as usize)?.as_ref()?;
                let mapped = &mut ids[source][target as usize];
                if *mapped == 0 {
                    let next = offsets.len() as u32;
                    if next >= MAX_OBJECTS {
                        return None;
                    }
                    offsets.push(0);
                    *mapped = next;
                    queue.push_back((source, target, false));
                }
                *mapped
            };
            // Only references are rewritten. Literal strings, numeric
            // spellings and all encoded stream bytes pass through verbatim.
            out.extend_from_slice(&body[copied..span.start]);
            let _ = write!(Writer(&mut out), "{mapped} 0 R");
            copied = span.end;
            Some(())
        };
        let mut reader = Reader::new(body);
        reader.visit_ref = Some(&mut rewrite);
        reader.body()?;
        out.extend_from_slice(&body[copied..]);
        out.extend_from_slice(b"\nendobj\n");
    }
    for (source, ids) in sources.iter().zip(&ids) {
        for (id, body) in source.objects.iter().enumerate() {
            if body.is_some()
                && id != source.catalog as usize
                && id != source.tree as usize
                && ids[id] == 0
            {
                return None;
            }
        }
    }
    offsets[CATALOG as usize] = out.len();
    let _ = write!(
        Writer(&mut out),
        "{CATALOG} 0 obj\n<< /Type /Catalog /Pages {PAGE_TREE} 0 R >>\nendobj\n"
    );
    offsets[PAGE_TREE as usize] = out.len();
    let _ = write!(
        Writer(&mut out),
        "{PAGE_TREE} 0 obj\n<< /Type /Pages /Kids ["
    );
    for id in FIRST_PAGE..FIRST_PAGE + total as u32 {
        let _ = write!(Writer(&mut out), " {id} 0 R");
    }
    let _ = write!(Writer(&mut out), " ] /Count {total} >>\nendobj\n");
    Some(finish_pdf(out, &offsets))
}

fn unsigned(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn number(bytes: &[u8]) -> bool {
    let bytes = bytes
        .strip_prefix(b"-")
        .or_else(|| bytes.strip_prefix(b"+"))
        .unwrap_or(bytes);
    let mut dot = false;
    let mut digits = false;
    for &b in bytes {
        match b {
            b'0'..=b'9' => digits = true,
            b'.' if !dot => dot = true,
            _ => return false,
        }
    }
    digits
}

fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn regular(b: u8) -> bool {
    (b'!'..=b'~').contains(&b) && !b"()<>[]{}/%".contains(&b)
}

fn reference(data: &[u8]) -> Option<u32> {
    let mut reader = Reader::new(data);
    let id = unsigned(reader.word()?)?;
    (reader.word()? == b"0" && reader.word()? == b"R" && reader.done()).then_some(id)
}

fn kids(data: &[u8]) -> Option<Vec<u32>> {
    let mut reader = Reader::new(data.strip_prefix(b"[")?);
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    while !reader.tag(b"]") {
        let id = unsigned(reader.word()?)?;
        if ids.len() == MAX_PAGES
            || reader.word()? != b"0"
            || reader.word()? != b"R"
            || !seen.insert(id)
        {
            return None;
        }
        ids.push(id);
    }
    (reader.done() && !ids.is_empty()).then_some(ids)
}

fn media_box(data: &[u8]) -> bool {
    let Some(array) = data.strip_prefix(b"[") else {
        return false;
    };
    let mut reader = Reader::new(array);
    (0..4).all(|_| reader.word().is_some_and(number)) && reader.tag(b"]") && reader.done()
}

fn entries<'a>(data: &'a [u8], entry: impl FnMut(&'a [u8], &'a [u8]) -> Option<()>) -> Option<()> {
    let mut reader = Reader::new(data);
    reader.dict(0, entry)?;
    reader.done().then_some(())
}

type VisitRef<'a> = &'a mut dyn FnMut(Range<usize>, u32, bool) -> Option<()>;

struct Reader<'a, 'v> {
    data: &'a [u8],
    at: usize,
    visit_ref: Option<VisitRef<'v>>,
}

impl<'a, 'v> Reader<'a, 'v> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            at: 0,
            visit_ref: None,
        }
    }

    fn skip_space(&mut self) {
        while self.data.get(self.at).copied().is_some_and(space) {
            self.at += 1;
        }
    }

    fn word(&mut self) -> Option<&'a [u8]> {
        self.skip_space();
        let start = self.at;
        while self.data.get(self.at).copied().is_some_and(regular) {
            self.at += 1;
        }
        (start < self.at).then_some(&self.data[start..self.at])
    }

    fn tag(&mut self, bytes: &[u8]) -> bool {
        self.skip_space();
        if self.data[self.at..].starts_with(bytes) {
            self.at += bytes.len();
            return true;
        }
        false
    }

    fn done(&mut self) -> bool {
        self.skip_space();
        self.at == self.data.len()
    }

    fn body(&mut self) -> Option<()> {
        if !self.data[self.at..].starts_with(b"<<") {
            return self.value(0, false);
        }
        let mut length = None;
        self.dict(0, |key, value| {
            if key == b"Length" {
                length = unsigned(value);
            }
            Some(())
        })?;
        if self.data[self.at..].starts_with(b"\nstream\n") {
            self.at += b"\nstream\n".len();
            self.at = self.at.checked_add(length? as usize)?;
            let end = self.data.get(self.at..)?.strip_prefix(b"\nendstream")?;
            self.at = self.data.len() - end.len();
        }
        Some(())
    }

    fn name(&mut self) -> Option<&'a [u8]> {
        // Do not skip whitespace after the slash. Escapes are outside
        // the profile and could otherwise mask two spellings of a key.
        let start = self.at;
        while self.data.get(self.at).copied().is_some_and(regular) {
            self.at += 1;
        }
        let name = &self.data[start..self.at];
        (!name.is_empty() && !name.contains(&b'#')).then_some(name)
    }

    fn dict(
        &mut self,
        depth: usize,
        mut entry: impl FnMut(&'a [u8], &'a [u8]) -> Option<()>,
    ) -> Option<()> {
        if !self.tag(b"<<") {
            return None;
        }
        let mut keys = HashSet::new();
        while !self.tag(b">>") {
            if keys.len() == 4096 || !self.tag(b"/") {
                return None;
            }
            let key = self.name()?;
            if !keys.insert(key) {
                return None;
            }
            self.skip_space();
            let start = self.at;
            self.value(depth + 1, depth == 0 && key == b"Parent")?;
            entry(key, &self.data[start..self.at])?;
        }
        Some(())
    }

    fn value(&mut self, depth: usize, parent: bool) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_space();
        match *self.data.get(self.at)? {
            b'<' if self.data[self.at..].starts_with(b"<<") => self.dict(depth, |_, _| Some(())),
            b'<' => {
                self.at += 1;
                while self.data.get(self.at).is_some_and(u8::is_ascii_hexdigit) {
                    self.at += 1;
                }
                (self.data.get(self.at) == Some(&b'>')).then(|| self.at += 1)
            }
            b'(' => self.literal(),
            b'/' => {
                self.at += 1;
                self.name().map(drop)
            }
            b'[' => {
                self.at += 1;
                while !self.tag(b"]") {
                    self.value(depth + 1, false)?;
                }
                Some(())
            }
            _ => {
                let start = self.at;
                let word = self.word()?;
                if matches!(word, b"true" | b"false" | b"null") {
                    return Some(());
                }
                if !number(word) {
                    return None;
                }
                if let Some(id) = unsigned(word) {
                    let end = self.at;
                    let generation = self.word();
                    if generation.and_then(unsigned).is_some() && self.word() == Some(b"R") {
                        if generation != Some(b"0") {
                            return None;
                        }
                        if let Some(visit) = &mut self.visit_ref {
                            visit(start..self.at, id, parent)?;
                        }
                        return Some(());
                    }
                    self.at = end;
                }
                Some(())
            }
        }
    }

    fn literal(&mut self) -> Option<()> {
        self.at += 1;
        let mut open = 1usize;
        loop {
            let b = *self.data.get(self.at)?;
            self.at += 1;
            match b {
                b'\\' if self.data.get(self.at).is_some() => self.at += 1,
                b'(' => open += 1,
                b')' => {
                    open -= 1;
                    if open == 0 {
                        return Some(());
                    }
                }
                _ => {}
            }
        }
    }
}
