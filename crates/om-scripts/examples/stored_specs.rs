//! Decode, re-encode and print the specs stored inside script steps
//! (script-steps.md 11): sort orders (`0a`), find requests (`06`), import
//! options (`84`, `f0`), export options (`85`), translation profiles
//! (`86`, `87` of Import Records), page formats and print settings (`87`,
//! `88`), Insert File filters (`8c`) and file references (`20`).
//!
//! For every occurrence in the given files it decodes the stored bytes with
//! the layouts of script-steps.md 11, re-encodes them and compares the
//! result with the stored node byte for byte, then reports coverage per
//! structure. Read-only: nothing is written to the files.
//!
//! `cargo run --release -p om-scripts --example stored_specs -- [--emit] <file.fmp12>...`
//! (`@list` reads paths from a file, one per line). `--emit` also prints one
//! canonical line per decoded spec (`file, script id, step index, step id,
//! kind, text`), the form `tools/script_specs_xml.py` builds from FileMaker's
//! own Save as XML output to check the decoding against it.
use om_hbam::{serial, HbamFile, OwnedNode, RidList};
use om_scripts::{bytes_node, node_bytes, node_text, text_node, ScriptCatalog, ScriptStep};
use std::collections::BTreeMap;
use std::fmt::Write as _;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A big-endian reader over a stored stream.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at + n)?;
        self.at += n;
        Some(out)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    /// An `HBKey<N>`: `[u8 n][n bytes]`, kept with its length byte.
    fn key(&mut self) -> Option<Key> {
        let n = *self.bytes.get(self.at)? as usize;
        Some(Key(self.take(n + 1)?.to_vec()))
    }
    /// A `SwapOStream` string: a byte count (`n < 0x80`: one byte; below
    /// `0x4000`: u16 with `0x8000` set; `c2` u16; `c4` u32), then that many
    /// bytes of XOR-masked SCSU text.
    fn string(&mut self) -> Option<String> {
        let first = self.u8()?;
        let n = match first {
            0..=0x7f => usize::from(first),
            0x80..=0xbf => (usize::from(first & 0x3f) << 8) | usize::from(self.u8()?),
            0xc2 => usize::from(self.u16()?),
            0xc4 => self.u32()? as usize,
            _ => return None,
        };
        om_scsu::decode_scsu(self.take(n)?).ok()
    }
    fn rest(&mut self) -> &'a [u8] {
        let out = &self.bytes[self.at..];
        self.at = self.bytes.len();
        out
    }
    fn done(&self) -> bool {
        self.at == self.bytes.len()
    }
}

fn put_string(out: &mut Vec<u8>, text: &str) {
    let bytes = om_scsu::encode_scsu(text);
    let n = bytes.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x4000 {
        out.extend((n as u16 | 0x8000).to_be_bytes());
    } else if n <= 0xffff {
        out.push(0xc2);
        out.extend((n as u16).to_be_bytes());
    } else {
        out.push(0xc4);
        out.extend((n as u32).to_be_bytes());
    }
    out.extend(bytes);
}

/// An `HBKey<N>` as stored: length byte and key bytes, segments
/// `[u8 len][bytes]`. Kept verbatim (`00` and `02 01 00` both mean "none").
#[derive(Clone, Debug, PartialEq, Eq)]
struct Key(Vec<u8>);

impl Key {
    fn segments(&self) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let body = &self.0[1..];
        let mut i = 0;
        while i < body.len() {
            let n = body[i] as usize;
            match body.get(i + 1..i + 1 + n) {
                Some(seg) => out.push(seg),
                None => break,
            }
            i += 1 + n;
        }
        out
    }
    /// The id of a one-segment key (`None` for an empty key and for the
    /// "no key" segment `00`).
    fn id(&self) -> Option<u64> {
        match self.segments().as_slice() {
            [seg] => serial::decode(seg).filter(|id| *id != 0),
            _ => None,
        }
    }
    fn render(&self) -> String {
        match (self.id(), self.0.as_slice()) {
            (Some(id), _) => id.to_string(),
            (None, [0] | [2, 1, 0]) => "-".into(),
            (None, raw) => format!("x{}", hex(raw)),
        }
    }
}

/// A `DBFieldSpec` in a stream: table key, field key, u16 repetition.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FieldSpec {
    table: Key,
    field: Key,
    rep: u16,
}

impl FieldSpec {
    fn read(r: &mut Reader) -> Option<Self> {
        Some(Self {
            table: r.key()?,
            field: r.key()?,
            rep: r.u16()?,
        })
    }
    fn write(&self, out: &mut Vec<u8>) {
        out.extend(&self.table.0);
        out.extend(&self.field.0);
        out.extend(self.rep.to_be_bytes());
    }
    fn render(&self) -> String {
        format!(
            "{}:{}[{}]",
            self.table.render(),
            self.field.render(),
            self.rep
        )
    }
    fn is_none(&self) -> bool {
        self.field.id().is_none()
    }
}

// ---------------------------------------------------------------- sort (0a)

/// `DBSortSpec` (script-steps.md 11.1).
#[derive(Debug, PartialEq, Eq)]
struct SortSpec {
    /// `DBSortSpec::Flags`: 1 kDontMaintainOrder, 2 kSortBlanksLast.
    flags: u16,
    entries: Vec<SortEntry>,
}

#[derive(Debug, PartialEq, Eq)]
struct SortEntry {
    field: FieldSpec,
    /// "Reorder based on summary field".
    summary: FieldSpec,
    value_list: Key,
    /// `mSortOrderFlags`: bit 0 descending.
    order: u8,
    /// `Locale::Type` override, 0 = the field's own.
    locale: u32,
}

impl SortSpec {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        let flags = r.u16()?;
        let n = r.u16()?;
        let mut entries = Vec::new();
        for _ in 0..n {
            entries.push(SortEntry {
                field: FieldSpec::read(&mut r)?,
                summary: FieldSpec::read(&mut r)?,
                value_list: r.key()?,
                order: r.u8()?,
                locale: r.u32()?,
            });
        }
        r.done().then_some(Self { flags, entries })
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(self.flags.to_be_bytes());
        out.extend((self.entries.len() as u16).to_be_bytes());
        for e in &self.entries {
            e.field.write(&mut out);
            e.summary.write(&mut out);
            out.extend(&e.value_list.0);
            out.push(e.order);
            out.extend(e.locale.to_be_bytes());
        }
        out
    }
    /// `maintain=.. blanksLast=..` then per entry `Ascending|Descending|Custom
    /// TO:F [vl=..] [summary=TO:F] [lang=..]`, as FileMaker's XML names them.
    fn render(&self) -> String {
        let mut s = format!(
            "maintain={} blanksLast={}",
            self.flags & 1 == 0,
            self.flags & 2 != 0
        );
        for e in &self.entries {
            let kind = if e.value_list.id().is_some() {
                "Custom"
            } else if e.order & 1 != 0 {
                "Descending"
            } else {
                "Ascending"
            };
            let _ = write!(
                s,
                " | {kind} {}:{}",
                e.field.table.render(),
                e.field.field.render()
            );
            if e.field.rep != 1 {
                let _ = write!(s, " rep={}", e.field.rep);
            }
            if e.value_list.id().is_some() {
                let _ = write!(s, " vl={}", e.value_list.render());
            }
            if e.order & !1 != 0 {
                let _ = write!(s, " order={}", e.order);
            }
            if !e.summary.is_none() {
                let _ = write!(
                    s,
                    " summary={}:{}",
                    e.summary.table.render(),
                    e.summary.field.render()
                );
            }
            if e.locale != 0 {
                let _ = write!(s, " lang={}", e.locale);
            }
        }
        s
    }
}

// ------------------------------------------------------- find requests (06)

/// One criterion's key in a request row: the packed field spec key of
/// `DBFieldSpec::GetFieldSpecKey` (script-steps.md 11.2).
#[derive(Debug, PartialEq, Eq)]
struct CriterionKey {
    /// The table occurrence key's segments (empty for a field spec without
    /// one, whose key is not packed).
    table: Vec<Vec<u8>>,
    field: u64,
    rep: u64,
}

impl CriterionKey {
    fn decode(segment: &[u8]) -> Option<Self> {
        let (packed, body) = match segment.split_first()? {
            (0, rest) => (true, rest),
            _ => (false, segment),
        };
        let mut segs = Vec::new();
        let mut i = 0;
        while i < body.len() {
            let n = body[i] as usize;
            segs.push(body.get(i + 1..i + 1 + n)?.to_vec());
            i += 1 + n;
        }
        let last = segs.pop()?;
        if packed == segs.is_empty() {
            return None;
        }
        // The field id is the serial key the segment starts with; the bytes
        // after it are the repetition (absent for repetition 1).
        let len = last.first()?.leading_ones() as usize + 1;
        let field = serial::decode(last.get(..len)?)?;
        let rep = match &last[len..] {
            [] => 1,
            rest => serial::decode(rest)?,
        };
        Some(Self {
            table: segs,
            field,
            rep,
        })
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.table.is_empty() {
            out.push(0);
        }
        for seg in &self.table {
            out.push(seg.len() as u8);
            out.extend(seg);
        }
        let mut last = serial::encode(self.field);
        if self.rep != 1 {
            last.extend(serial::encode(self.rep));
        }
        out.push(last.len() as u8);
        out.extend(last);
        out
    }
    fn render(&self) -> String {
        let table = match self.table.as_slice() {
            [seg] => serial::decode(seg).map_or_else(|| hex(seg), |id| id.to_string()),
            [] => "-".into(),
            segs => segs.iter().map(|s| hex(s)).collect::<Vec<_>>().join("."),
        };
        format!("{table}:{}[{}]", self.field, self.rep)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    id: u64,
    omit: bool,
    /// `fc` (`KModCount`) of the request row, verbatim.
    mod_count: Option<Vec<u8>>,
    criteria: Vec<(CriterionKey, String)>,
}

/// `DBRequestSet::SaveSet`: `05` (`KItems`) the RID list of request ids,
/// `06` (`KRequests`) the request rows and `00` the last id.
#[derive(Debug, PartialEq, Eq)]
struct RequestSet {
    order: RidList,
    /// `[06, 06, 00]`: the last request id as a serial key `[u8 len][key]`.
    last_id: Option<Vec<u8>>,
    requests: Vec<Request>,
}

impl RequestSet {
    fn decode(node: &OwnedNode) -> Option<Self> {
        if node.value.is_some() || node.children.len() != 2 {
            return None;
        }
        let order = RidList::from_owned(node.child(&[5])?).ok()?;
        let rows = node.child(&[6])?;
        if rows.value.is_some() {
            return None;
        }
        let mut last_id = None;
        let mut requests = Vec::new();
        for row in &rows.children {
            if row.segment == [0] {
                last_id = Some(row.value.clone()?);
                continue;
            }
            let id = serial::decode(&row.segment)?;
            if row.value.is_some() {
                return None;
            }
            let mut req = Request {
                id,
                omit: false,
                mod_count: None,
                criteria: Vec::new(),
            };
            for cell in &row.children {
                match cell.segment.as_slice() {
                    // The omit state is an empty leaf at `KBegin`.
                    [0] if cell.value.as_deref() == Some(&[]) && cell.children.is_empty() => {
                        req.omit = true
                    }
                    [0xfc] if cell.children.is_empty() => req.mod_count = cell.value.clone(),
                    seg => req
                        .criteria
                        .push((CriterionKey::decode(seg)?, node_text(cell)?)),
                }
            }
            requests.push(req);
        }
        Some(Self {
            order,
            last_id,
            requests,
        })
    }
    fn encode(&self) -> OwnedNode {
        let mut rows = Vec::new();
        if let Some(last) = &self.last_id {
            rows.push(OwnedNode::leaf([0], last.clone()));
        }
        for req in &self.requests {
            let mut cells = Vec::new();
            if req.omit {
                cells.push(OwnedNode::leaf([0], []));
            }
            for (key, text) in &req.criteria {
                cells.push(text_node(&key.encode(), text));
            }
            if let Some(m) = &req.mod_count {
                cells.push(OwnedNode::leaf([0xfc], m.clone()));
            }
            cells.sort_by(|a, b| a.segment.cmp(&b.segment));
            rows.push(OwnedNode::list(serial::encode(req.id), cells));
        }
        rows.sort_by(|a, b| a.segment.cmp(&b.segment));
        OwnedNode::list(
            [6],
            vec![self.order.to_node([5]), OwnedNode::list([6], rows)],
        )
    }
    /// Requests in RID-list order: `find|omit` then `TO:F[rep] "criteria"`.
    fn render(&self) -> String {
        let mut parts = Vec::new();
        for id in self.order.iter() {
            let Some(req) = self.requests.iter().find(|r| r.id == id) else {
                parts.push(format!("missing {id}"));
                continue;
            };
            let mut s = String::from(if req.omit { "omit" } else { "find" });
            let mut crit: Vec<String> = req
                .criteria
                .iter()
                .map(|(k, t)| format!(" {} {:?}", k.render(), t))
                .collect();
            crit.sort();
            s.extend(crit);
            parts.push(s);
        }
        parts.join(" | ")
    }
}

// ------------------------------------------------- import options (84, f0)

/// `FMImportOptions::Write` (script-steps.md 11.3).
#[derive(Debug, PartialEq, Eq)]
struct ImportOptions {
    table: Key,
    action: u8,
    flags: u8,
    charset: u8,
    /// Target field key and mapping per source column.
    entries: Vec<(Key, u8)>,
}

impl ImportOptions {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        let table = r.key()?;
        let (action, flags, charset) = (r.u8()?, r.u8()?, r.u8()?);
        let n = r.u16()?;
        let mut entries = Vec::new();
        for _ in 0..n {
            entries.push((r.key()?, r.u8()?));
        }
        r.done().then_some(Self {
            table,
            action,
            flags,
            charset,
            entries,
        })
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = self.table.0.clone();
        out.extend([self.action, self.flags, self.charset]);
        out.extend((self.entries.len() as u16).to_be_bytes());
        for (k, m) in &self.entries {
            out.extend(&k.0);
            out.push(*m);
        }
        out
    }
    fn render(&self) -> String {
        let mut s = format!(
            "table={} action={} flags={:#04x} charset={} n={}",
            self.table.render(),
            self.action,
            self.flags,
            self.charset,
            self.entries.len()
        );
        for (i, (k, m)) in self.entries.iter().enumerate() {
            let _ = write!(s, " {}:{}={}", i + 1, k.render(), m);
        }
        s
    }
}

/// `FMImportOptions::WriteImportFlags` under `f0` (`KFlags`).
#[derive(Debug, PartialEq, Eq)]
struct ImportFlags {
    text: String,
    entries: Vec<(Key, u8)>,
}

impl ImportFlags {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        let text = r.string()?;
        let n = r.u16()?;
        let mut entries = Vec::new();
        for _ in 0..n {
            entries.push((r.key()?, r.u8()?));
        }
        r.done().then_some(Self { text, entries })
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, &self.text);
        out.extend((self.entries.len() as u16).to_be_bytes());
        for (k, m) in &self.entries {
            out.extend(&k.0);
            out.push(*m);
        }
        out
    }
    fn render(&self) -> String {
        let mut s = format!("text={:?} n={}", self.text, self.entries.len());
        for (i, (k, m)) in self.entries.iter().enumerate() {
            let _ = write!(s, " {}:{}={}", i + 1, k.render(), m);
        }
        s
    }
}

// ------------------------------------------------------ export options (85)

/// `FMExportOptions::Write` (script-steps.md 11.4). Older writers stop
/// before the last option bytes; `tail` keeps what is stored.
#[derive(Debug, PartialEq, Eq)]
struct ExportOptions {
    fields: Vec<(FieldSpec, FieldSpec)>,
    group_by: Vec<(FieldSpec, u8)>,
    tail: Vec<u8>,
}

impl ExportOptions {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        let n = r.u16()?;
        let mut fields = Vec::new();
        for _ in 0..n {
            fields.push((FieldSpec::read(&mut r)?, FieldSpec::read(&mut r)?));
        }
        let g = r.u16()?;
        let mut group_by = Vec::new();
        for _ in 0..g {
            group_by.push((FieldSpec::read(&mut r)?, r.u8()?));
        }
        let tail = r.rest().to_vec();
        (tail.len() <= 3).then_some(Self {
            fields,
            group_by,
            tail,
        })
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend((self.fields.len() as u16).to_be_bytes());
        for (f, s) in &self.fields {
            f.write(&mut out);
            s.write(&mut out);
        }
        out.extend((self.group_by.len() as u16).to_be_bytes());
        for (f, b) in &self.group_by {
            f.write(&mut out);
            out.push(*b);
        }
        out.extend(&self.tail);
        out
    }
    fn render(&self) -> String {
        let mut s = format!("tail={}", hex(&self.tail));
        for (g, b) in &self.group_by {
            let _ = write!(s, " | group {} {}", g.render(), b);
        }
        for (f, second) in &self.fields {
            let _ = write!(s, " | field {}", f.render());
            if !second.is_none() {
                let _ = write!(s, " {}", second.render());
            }
        }
        s
    }
}

// ------------------------------------------- translation profiles (86, 87)

/// A path list in a stream (`UniversalPathListToStream`): u32 count, then
/// three strings per path.
fn read_paths(r: &mut Reader) -> Option<Vec<[String; 3]>> {
    let n = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..n {
        out.push([r.string()?, r.string()?, r.string()?]);
    }
    Some(out)
}

fn put_paths(out: &mut Vec<u8>, paths: &[[String; 3]]) {
    out.extend((paths.len() as u32).to_be_bytes());
    for p in paths {
        for s in p {
            put_string(out, s);
        }
    }
}

/// One field of a profile body, in stream order.
#[derive(Debug, PartialEq, Eq)]
enum Part {
    U8(u8),
    U16(u16),
    U32(u32),
    Key(Key),
    Text(String),
    Paths(Vec<[String; 3]>),
    /// Bytes no layout of this writer explains (older writers), kept.
    Raw(Vec<u8>),
}

/// `FMTranslationProfile::Write(HBPosition, key)`: the file type QuadChar,
/// then the subclass body (script-steps.md 11.5).
#[derive(Debug, PartialEq, Eq)]
struct Profile {
    file_type: [u8; 4],
    parts: Vec<Part>,
}

impl Profile {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        let file_type: [u8; 4] = r.take(4)?.try_into().ok()?;
        // Every body starts with the base u16: 1 when a field names row is set.
        let mut parts = vec![Part::U16(r.u16()?)];
        let mut body = || -> Option<()> {
            match &file_type {
                b"FMPR" => parts.push(Part::Key(r.key()?)),
                b"BTCH" => {
                    parts.push(Part::Text(r.string()?));
                    for _ in 0..3 {
                        parts.push(Part::U8(r.u8()?));
                    }
                }
                b"XLS " | b"XLSX" => {
                    parts.push(Part::U32(r.u32()?));
                    parts.push(Part::Text(r.string()?));
                    parts.push(Part::Text(r.string()?));
                }
                b"XML " => {
                    for which in 0..2 {
                        let index = r.u8()?;
                        parts.push(Part::U8(index));
                        match (which, index) {
                            (_, 2) | (0, 1) => parts.push(Part::Text(r.string()?)),
                            (1, 1) => parts.push(Part::Paths(read_paths(&mut r)?)),
                            _ => {}
                        }
                    }
                }
                b"XMLE" => {
                    parts.push(Part::U16(r.u16()?));
                    let index = r.u16()?;
                    parts.push(Part::U16(index));
                    match index {
                        1 => parts.push(Part::Paths(read_paths(&mut r)?)),
                        2 => parts.push(Part::Text(r.string()?)),
                        _ => {}
                    }
                }
                b"ODBC" => {
                    for _ in 0..3 {
                        parts.push(Part::Text(r.string()?));
                    }
                    parts.push(Part::U16(r.u16()?));
                }
                _ => {}
            }
            Some(())
        };
        body()?;
        if !r.done() {
            parts.push(Part::Raw(r.rest().to_vec()));
        }
        Some(Self { file_type, parts })
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = self.file_type.to_vec();
        for p in &self.parts {
            match p {
                Part::U8(v) => out.push(*v),
                Part::U16(v) => out.extend(v.to_be_bytes()),
                Part::U32(v) => out.extend(v.to_be_bytes()),
                Part::Key(k) => out.extend(&k.0),
                Part::Text(t) => put_string(&mut out, t),
                Part::Paths(p) => put_paths(&mut out, p),
                Part::Raw(b) => out.extend(b),
            }
        }
        out
    }
    fn render(&self) -> String {
        let mut s = format!("type={:?}", String::from_utf8_lossy(&self.file_type));
        for p in &self.parts {
            let _ = match p {
                Part::U8(v) => write!(s, " {v}"),
                Part::U16(v) => write!(s, " {v}"),
                Part::U32(v) => write!(s, " {v}"),
                Part::Key(k) => write!(s, " key={}", k.render()),
                Part::Text(t) => write!(s, " {t:?}"),
                Part::Paths(p) => write!(s, " paths={p:?}"),
                Part::Raw(b) => write!(s, " raw={}", hex(b)),
            };
        }
        s
    }
}

/// Import Records' `87` (`KImportProfileEx`): the profile's field names
/// row (i16), predefined id (i32) and field delimiter text.
#[derive(Debug, PartialEq, Eq)]
struct ProfileEx {
    names_row: u16,
    predefined: u32,
    delimiter: String,
}

impl ProfileEx {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        let out = Self {
            names_row: r.u16()?,
            predefined: r.u32()?,
            delimiter: r.string()?,
        };
        r.done().then_some(out)
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = self.names_row.to_be_bytes().to_vec();
        out.extend(self.predefined.to_be_bytes());
        put_string(&mut out, &self.delimiter);
        out
    }
    fn render(&self) -> String {
        format!(
            "namesRow={} predefined={} delimiter={:?}",
            self.names_row as i16, self.predefined as i32, self.delimiter
        )
    }
}

/// Insert File's `8c` (`KFileFilterSpec`): one node per filter, keyed by
/// its index as two bytes, holding the name calculation (`05`) and the
/// extension list text (`07`).
fn filter_spec(node: &OwnedNode) -> Found {
    if node.value.is_some() {
        return found("filters", false, None);
    }
    let mut s = String::new();
    for f in &node.children {
        let name = f.child(&[5]).and_then(om_scripts::Calculation::decode);
        let ext = f.child(&[7]).and_then(node_text);
        let known = f
            .children
            .iter()
            .all(|c| matches!(c.segment.as_slice(), [5] | [7]));
        if f.segment.len() != 2 || f.value.is_some() || !known {
            return found("filters", false, None);
        }
        let _ = write!(
            s,
            "{}: name={} ext={:?}; ",
            u16::from_be_bytes([f.segment[0], f.segment[1]]),
            if name.is_some() { "calc" } else { "-" },
            ext.unwrap_or_default()
        );
    }
    found("filters", true, Some(s.trim_end().to_owned()))
}

// ------------------------------------------------------- everything else

/// A spec found in a step, decoded or not.
struct Found {
    kind: &'static str,
    exact: bool,
    canonical: Option<String>,
}

fn found(kind: &'static str, exact: bool, canonical: Option<String>) -> Found {
    Found {
        kind,
        exact,
        canonical,
    }
}

/// A stream stored as one value node (plain or chunked).
fn stream_spec<T>(
    kind: &'static str,
    node: &OwnedNode,
    decode: impl Fn(&[u8]) -> Option<T>,
    encode: impl Fn(&T) -> Vec<u8>,
    render: impl Fn(&T) -> String,
) -> Found {
    let Some(bytes) = node_bytes(node) else {
        return found(kind, false, None);
    };
    match decode(&bytes) {
        Some(spec) => {
            let exact = bytes_node(&node.segment, &encode(&spec)) == *node;
            found(kind, exact, Some(render(&spec)))
        }
        None => found(kind, false, None),
    }
}

/// The tagged chunks of a platform print record (`87`, `88`):
/// `[4-byte tag][u32 length][payload]`...
fn print_record(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let mut r = Reader::new(bytes);
    let mut out = Vec::new();
    while !r.done() {
        let tag = String::from_utf8_lossy(r.take(4)?).into_owned();
        let n = r.u32()? as usize;
        out.push((tag, r.take(n)?.to_vec()));
    }
    Some(out)
}

fn print_spec(kind: &'static str, node: &OwnedNode) -> Found {
    let Some(bytes) = node_bytes(node) else {
        return found(kind, false, None);
    };
    match print_record(&bytes) {
        Some(chunks) => {
            let mut rebuilt = Vec::new();
            let mut s = String::new();
            for (tag, payload) in &chunks {
                rebuilt.extend(tag.as_bytes());
                rebuilt.extend((payload.len() as u32).to_be_bytes());
                rebuilt.extend(payload);
                let shown = if tag == "Comm" {
                    hex(payload)
                } else {
                    payload.len().to_string()
                };
                let _ = write!(s, "{tag}:{shown} ");
            }
            let exact = rebuilt == *bytes && bytes_node(&node.segment, &rebuilt) == *node;
            found(kind, exact, Some(s.trim_end().to_owned()))
        }
        None => found(kind, false, None),
    }
}

/// The steps whose compound parameters live in slots (script-steps.md
/// 11.6): their slots, listed for `--emit` (not a stored spec).
const SLOT_STEPS: [u16; 9] = [42, 43, 74, 87, 91, 143, 144, 152, 160];

/// `n:` then each child of slot n: `l=` the slot's own list value, `t=` text,
/// `n=` numeric data (hex, 1 to 4 bytes), `c` a calculation, `f=` a field
/// spec, `a=` table occurrences, `p` a path list.
fn slot_summary(step: &ScriptStep) -> String {
    let mut s = format!("value={} flags={:#x}", step.value, step.flags);
    let Some(data) = &step.data else {
        return s;
    };
    for (n, slot) in data.slots() {
        let _ = write!(s, " {n}:");
        if let Some(v) = slot.value() {
            let _ = write!(s, "l={}", hex(v));
        }
        for key in slot.child_keys() {
            let node = slot.child(key).expect("present");
            let _ = match key {
                0x01 => write!(s, "t={:?},", node_text(node).unwrap_or_default()),
                0x03 => write!(
                    s,
                    "n={},",
                    node.value.as_deref().map(hex).unwrap_or_default()
                ),
                0x05 => write!(s, "c,"),
                0x10 => write!(
                    s,
                    "f={},",
                    node.value.as_deref().map(hex).unwrap_or_default()
                ),
                0x11 => write!(s, "a,"),
                0x82 => write!(s, "p,"),
                k => write!(s, "{k:02x},"),
            };
        }
    }
    s
}

fn step_specs(step: &ScriptStep) -> Vec<Found> {
    let mut out = Vec::new();
    let id = step.id();
    if SLOT_STEPS.contains(&id) {
        out.push(found("slots", true, Some(slot_summary(step))));
    }
    let Some(data) = &step.data else {
        return out;
    };
    for key in data.keys() {
        let node = data.key(key).expect("present");
        out.push(match (key, id) {
            (0x0a, _) => stream_spec(
                "sort",
                node,
                SortSpec::decode,
                SortSpec::encode,
                SortSpec::render,
            ),
            (0x06, _) => match RequestSet::decode(node) {
                Some(set) => found("find", set.encode() == *node, Some(set.render())),
                None => found("find", false, None),
            },
            (0x84, _) => stream_spec(
                "import",
                node,
                ImportOptions::decode,
                ImportOptions::encode,
                ImportOptions::render,
            ),
            (0xf0, 35 | 139) => stream_spec(
                "importflags",
                node,
                ImportFlags::decode,
                ImportFlags::encode,
                ImportFlags::render,
            ),
            (0x85, _) => stream_spec(
                "export",
                node,
                ExportOptions::decode,
                ExportOptions::encode,
                ExportOptions::render,
            ),
            (0x86, _) => stream_spec(
                "profile",
                node,
                Profile::decode,
                Profile::encode,
                Profile::render,
            ),
            (0x87, 35 | 139) => stream_spec(
                "profileex",
                node,
                ProfileEx::decode,
                ProfileEx::encode,
                ProfileEx::render,
            ),
            (0x87, 42 | 43) => print_spec("pageformat", node),
            (0x8c, _) => filter_spec(node),
            (0x20, _) => match node_bytes(node) {
                // An external data source id as a serial key `[u8 len][key]`.
                Some(b) => match b.split_first().and_then(|(n, id)| {
                    (*n as usize == id.len())
                        .then(|| serial::decode(id))
                        .flatten()
                }) {
                    Some(id) => found("fileref", true, Some(format!("source={id}"))),
                    None => found("fileref", false, None),
                },
                None => found("fileref", false, None),
            },
            (0x88, _) => print_spec("print", node),
            _ => continue,
        });
    }
    out
}

#[derive(Default)]
struct Tally {
    occurrences: usize,
    decoded: usize,
    exact: usize,
    examples: Vec<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut emit = false;
    let mut paths = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--emit" {
            emit = true;
        } else if let Some(list) = arg.strip_prefix('@') {
            paths.extend(std::fs::read_to_string(list)?.lines().map(str::to_owned));
        } else {
            paths.push(arg);
        }
    }
    let mut tally: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut files = 0;
    for path in paths.iter().filter(|p| !p.trim().is_empty()) {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = HbamFile::open(path)?;
        let tree = file.key_tree()?;
        let catalog = ScriptCatalog::decode(&tree)?;
        files += 1;
        for script in &catalog.scripts {
            for (i, step) in script.steps.iter().enumerate() {
                for f in step_specs(step) {
                    let t = tally.entry(f.kind).or_default();
                    t.occurrences += 1;
                    t.decoded += usize::from(f.canonical.is_some());
                    t.exact += usize::from(f.exact);
                    if !f.exact && t.examples.len() < 5 {
                        t.examples.push(format!("{name} | {} | #{i}", script.name));
                    }
                    if emit {
                        println!(
                            "{name}\t{}\t{i}\t{}\t{}\t{}",
                            script.id,
                            step.opcode,
                            f.kind,
                            f.canonical.as_deref().unwrap_or("UNDECODED")
                        );
                    }
                }
            }
        }
    }
    eprintln!("files {files}");
    for (kind, t) in &tally {
        eprintln!(
            "{kind:12} occurrences {:5}  decoded {:5}  re-encoded exactly {:5}",
            t.occurrences, t.decoded, t.exact
        );
        for e in &t.examples {
            eprintln!("    not exact: {e}");
        }
    }
    Ok(())
}
