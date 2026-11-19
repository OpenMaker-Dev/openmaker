//! Read-only decoder for the file-level structures of docs/re/file-metadata.md:
//! the file's locale and formats (`[2, 0d]`), the document preferences with
//! the file script triggers (`[2, 18]`), relationships (`[3, fb]`), summary
//! field definitions (`[T, 3, 5, F, 09]`), value lists (`[21]`), layout and
//! object script triggers (`[4, 5, L, ..., 30]`) and the serial ids of the
//! catalogs FileMaker shows. One tab-separated row per record:
//!
//! ```text
//! locale   <file> <field>=<value>...
//! prefs    <file> <field>=<value>...
//! rel      <file> <id> <field>=<value>...
//! summary  <file> <table id> <field id> <field>=<value>...
//! vl       <file> <id> <field>=<value>...
//! trigger  <file> <layout id> <owner> <field>=<value>...
//! id       <file> <catalog> <stored key hex> <serial id> <name>
//! error    <file> <path> <message>
//! ```
//!
//! Ids are serial-key values (the ids FileMaker's XML and Design functions
//! show); `0` is an empty key. With `--summary` the rows are replaced by a
//! coverage count per kind and value.
//!
//! `cargo run -p om-hbam --example file_metadata -- [--summary] <file.fmp12>...`
use std::collections::BTreeMap;

use om_hbam::{serial, HbamFile, Node};

/// A big-endian reader over a stored stream (`SwapIStream` order).
struct Stream<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Stream<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Stream { bytes, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// `SwapIStream::Get(uchar*, max)`: a length byte, then the key's segments.
    fn key(&mut self) -> Option<&'a [u8]> {
        let n = self.u8()? as usize;
        self.take(n)
    }

    /// `SwapIStream::InternalSCSUExpand`: a UTF-16 unit count (1 byte below
    /// 0x80, u16 with 0x8000 set below 0x4000, `c2` + u16, `c4` + u32), then
    /// masked SCSU.
    fn string(&mut self) -> Option<String> {
        let first = self.u8()?;
        let units = match first {
            0x00..=0x7f => first as usize,
            0xc2 => self.u16()? as usize,
            0xc4 => self.u32()? as usize,
            _ => (((first & 0x3f) as usize) << 8) | self.u8()? as usize,
        };
        if units == 0 {
            return Some(String::new());
        }
        let (text, used) = om_scsu::decode_scsu_units(&self.bytes[self.pos..], units).ok()?;
        self.pos += used;
        Some(text)
    }
}

/// The id a key string shows in FileMaker: its single segment as a serial key
/// (`80 0d` is 141); empty or `00` is 0; several segments are joined by `.`.
fn key_id(key: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut rest = key;
    while let Some((&n, tail)) = rest.split_first() {
        let n = n as usize;
        if tail.len() < n {
            return format!("x{}", hex(key));
        }
        let seg = &tail[..n];
        parts.push(match serial::decode(seg) {
            Some(v) => v.to_string(),
            None => format!("x{}", hex(seg)),
        });
        rest = &tail[n..];
    }
    if parts.is_empty() {
        "0".into()
    } else {
        parts.join(".")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A catalog text value. Besides the forms `Node::text` reads, a chunked
/// text may carry a 12-byte header (byte count, character count and a third
/// word, observed 1, 3, 4, 5 or 7) followed by the pieces `1..n`.
fn text_of(node: &Node<'_>) -> Option<String> {
    if let Some(Ok(text)) = node.text() {
        return Some(text);
    }
    let header = node.get(0)?.value.filter(|v| v.len() == 12)?;
    let units = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut bytes = Vec::new();
    for n in 1.. {
        match node
            .children
            .iter()
            .find(|c| serial::decode(c.segment.0) == Some(n))
        {
            Some(piece) => bytes.extend_from_slice(piece.value?),
            None => break,
        }
    }
    om_scsu::decode_scsu_units(&bytes, units)
        .ok()
        .map(|(text, _)| text)
}

fn clean(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

/// `DBFieldSpec::Write`: occurrence key, field key, u16 repetition.
fn field_spec(s: &mut Stream<'_>) -> Option<String> {
    let to = key_id(s.key()?);
    let field = key_id(s.key()?);
    let rep = s.u16()?;
    Some(format!("{to}:{field}:{rep}"))
}

/// `DBSortSpec::Write` (script-steps.md 11.1.1) as
/// `flags|field[,summary][,vl=V][,desc][,lang=L];...`.
fn sort_spec(s: &mut Stream<'_>) -> Option<String> {
    let flags = s.u16()?;
    let n = s.u16()?;
    let mut entries = Vec::new();
    for _ in 0..n {
        let field = field_spec(s)?;
        let summary = field_spec(s)?;
        let vl = key_id(s.key()?);
        let order = s.u8()?;
        let locale = s.u32()?;
        let mut e = field;
        if !summary.starts_with("0:0:") {
            e.push_str(&format!(",summary={summary}"));
        }
        if vl != "0" {
            e.push_str(&format!(",vl={vl}"));
        }
        if order & 1 != 0 {
            e.push_str(",desc");
        }
        if order & !1 != 0 {
            e.push_str(&format!(",order={order}"));
        }
        if locale != 0 {
            e.push_str(&format!(",lang={locale}"));
        }
        entries.push(e);
    }
    Some(format!("{flags}|{}", entries.join(";")))
}

struct Out {
    summary: bool,
    counts: BTreeMap<String, usize>,
}

impl Out {
    fn row(&mut self, kind: &str, file: &str, fields: &[String]) {
        if self.summary {
            *self.counts.entry(format!("{kind} rows")).or_default() += 1;
            for f in fields {
                if let Some((k, v)) = f.split_once('=') {
                    if matches!(
                        k,
                        "kind"
                            | "op"
                            | "event"
                            | "type"
                            | "flags"
                            | "options"
                            | "order"
                            | "elements"
                            | "owner"
                    ) {
                        for v in v.split(',') {
                            *self.counts.entry(format!("{kind} {k}={v}")).or_default() += 1;
                        }
                    }
                }
            }
        } else {
            println!("{kind}\t{file}\t{}", fields.join("\t"));
        }
    }
}

fn locale(out: &mut Out, file: &str, value: &[u8]) -> Option<()> {
    // Draco::Locale::Write (libSupport 0x1c1620) / Read (0x1bd570).
    let mut s = Stream::new(value);
    let mut f = Vec::new();
    f.push(format!("type={}", s.u32()?));
    f.push(format!("pad={}", s.u8()?));
    let flags = s.u32()?;
    f.push(format!("flags={flags:#010x}"));
    for (bit, name) in [
        (31, "hour0"),
        (30, "day0"),
        (29, "month0"),
        (28, "century"),
        (27, "ampm"),
        (25, "num0"),
        (24, "metric"),
        (23, "currencyLeading"),
    ] {
        f.push(format!("{name}={}", flags >> bit & 1));
    }
    f.push(format!("ampmSuffix={}", u32::from(flags >> 22 & 1 == 0)));
    for name in [
        "separator",
        "quoteOpen",
        "quoteClose",
        "thousands",
        "decimal",
        "dateSep",
        "timeSep",
    ] {
        let c = s.u16()?;
        f.push(format!("{name}=U+{c:04X}"));
    }
    f.push(format!("version={}", s.u8()?));
    f.push(format!("order={}", s.u8()?));
    let elements: Vec<String> = (0..4)
        .map(|_| s.u8().map(|e| e.to_string()))
        .collect::<Option<_>>()?;
    f.push(format!("elements={}", elements.join(",")));
    for name in ["am", "pm", "h24", "currency", "plus", "minus"] {
        f.push(format!("{name}={}", clean(&s.string()?)));
    }
    for (name, n) in [
        ("days", 7),
        ("shortDays", 7),
        ("months", 12),
        ("shortMonths", 12),
        ("quarters", 4),
        ("shortQuarters", 4),
        ("elementSeps", 5),
    ] {
        let list: Vec<String> = (0..n)
            .map(|_| s.string().map(|t| clean(&t)))
            .collect::<Option<_>>()?;
        f.push(format!("{name}={}", list.join("|")));
    }
    if !s.at_end() {
        f.push(format!("trailing={}", hex(&value[s.pos..])));
    }
    out.row("locale", file, &f);
    Some(())
}

fn prefs(out: &mut Out, file: &str, value: &[u8]) -> Option<()> {
    // Draco::DocumentPrefs::Read (libDBEngine 0xf526a0): every member after
    // the print styles is optional (older files end early).
    let mut s = Stream::new(value);
    let mut f = vec![
        format!("options={:#06x}", s.u32()?),
        format!("fieldOrder={}", s.u8()?),
        format!("fieldAscending={}", s.u8()?),
        format!("onFirstWindowOpen={}", key_id(s.key()?)),
        format!("onLastWindowClose={}", key_id(s.key()?)),
        format!("defaultLayout={}", key_id(s.key()?)),
        format!("printType={}", s.u16()?),
        format!("relationsPrintStyle={}", s.u16()?),
    ];
    let optional = |s: &mut Stream<'_>, f: &mut Vec<String>| -> Option<()> {
        f.push(format!("thumbnailOptions={}", s.u16()?));
        f.push(format!("thumbnailCacheMB={}", s.u32()?));
        f.push(format!("onWindowOpen={}", key_id(s.key()?)));
        f.push(format!("onWindowClose={}", key_id(s.key()?)));
        f.push(format!("customLayoutSize={}x{}", s.u16()?, s.u16()?));
        f.push(format!("onFileAVPlayerChange={}", key_id(s.key()?)));
        f.push(format!("iconType={}", s.u16()?));
        f.push(format!("scaleFactor={}", s.u16()?));
        f.push(format!("onWindowTransaction={}", key_id(s.key()?)));
        f.push(format!("transactionField={}", clean(&s.string()?)));
        Some(())
    };
    let _ = optional(&mut s, &mut f);
    if !s.at_end() {
        f.push(format!("trailing={}", hex(&value[s.pos..])));
    }
    let triggers: Vec<&str> = f
        .iter()
        .filter_map(|x| x.split_once('='))
        .filter(|(k, v)| k.starts_with("on") && *v != "0")
        .map(|(k, _)| k)
        .collect();
    let event = if triggers.is_empty() {
        "none".to_string()
    } else {
        triggers.join(",")
    };
    f.push(format!("event={event}"));
    out.row("prefs", file, &f);
    Some(())
}

fn relationship(out: &mut Out, file: &str, id: &str, node: &Node<'_>) -> Option<()> {
    // DBRelation::Definition::Read (0x10fef10) and DBJoinPred::Read (0x10fcd30).
    let def = node.get(2)?.assembled()?;
    let mut s = Stream::new(&def);
    let left = key_id(s.key()?);
    let right = key_id(s.key()?);
    let ltr = sort_spec(&mut s)?;
    let rtl = sort_spec(&mut s)?;
    let options = s.u32()?;
    let mut f = vec![
        id.to_string(),
        format!("left={left}"),
        format!("right={right}"),
        format!("options={options:#x}"),
        format!("sortRight={ltr}"),
        format!("sortLeft={rtl}"),
    ];
    if !s.at_end() {
        f.push(format!("trailing={}", hex(&def[s.pos..])));
    }
    let mut ops = Vec::new();
    let mut preds = Vec::new();
    for p in node
        .get(3)
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
    {
        let v = p.assembled()?;
        let mut s = Stream::new(&v);
        let op = s.u8()?;
        let l = key_id(s.key()?);
        let r = key_id(s.key()?);
        ops.push(op.to_string());
        let n = serial::decode(p.segment.0)
            .map(|v| v.to_string())
            .unwrap_or_else(|| hex(p.segment.0));
        preds.push(format!("{n}:{l}:{op}:{r}"));
    }
    f.push(format!("op={}", ops.join(",")));
    f.push(format!("predicates={}", preds.join(";")));
    out.row("rel", file, &f);
    Some(())
}

fn summary(out: &mut Out, file: &str, table: &str, field: &str, def: &[u8]) {
    // DBField::GetSummaryDef (0xee89c0): the 30-byte SummaryDef, zero-padded.
    let mut raw = [0u8; 30];
    let n = def.len().min(30);
    raw[..n].copy_from_slice(&def[..n]);
    let spec = |b: &[u8]| {
        let key = |k: &[u8]| key_id(&k[1..1 + (k[0] as usize).min(5)]);
        format!(
            "{}:{}:{}",
            key(&b[0..6]),
            key(&b[6..12]),
            u16::from_be_bytes([b[12], b[13]])
        )
    };
    let f = vec![
        table.to_string(),
        field.to_string(),
        format!("kind={}", raw[0]),
        format!("flags={:#04x}", raw[1]),
        format!("field={}", spec(&raw[2..16])),
        format!("aux={}", spec(&raw[16..30])),
        format!("stored={}", def.len()),
    ];
    out.row("summary", file, &f);
}

fn value_list(out: &mut Out, file: &str, id: &str, node: &Node<'_>) -> Option<()> {
    // DBValueList::Definition::Read (0x121ecd0).
    let name = node.get(0x10).and_then(text_of).unwrap_or_default();
    let mut f = vec![id.to_string(), format!("name={}", clean(&name))];
    match node.get(2).and_then(|n| n.assembled()) {
        Some(def) => {
            let mut s = Stream::new(&def);
            f.push(format!("type={}", s.u8()?));
            f.push(format!("options={:#x}", s.u16()?));
            f.push(format!("primary={}", field_spec(&mut s)?));
            f.push(format!("secondary={}", field_spec(&mut s)?));
            f.push(format!("start={}", key_id(s.key()?)));
            f.push(format!("file={}", key_id(s.key()?)));
            f.push(format!("list={}", key_id(s.key()?)));
            f.push(format!("separator={}", clean(&s.string()?)));
            if !s.at_end() {
                f.push(format!("sort={}", sort_spec(&mut s)?));
            }
            if !s.at_end() {
                f.push(format!("trailing={}", hex(&def[s.pos..])));
            }
        }
        None => f.push("type=none".into()),
    }
    if let Some(custom) = node.get(3).and_then(text_of) {
        f.push(format!("custom={}", clean(&custom)));
    }
    let calc = node
        .children
        .iter()
        .filter(|c| !c.children.is_empty() && c.get(6).is_some())
        .map(|c| hex(c.segment.0));
    let calc: Vec<String> = calc.collect();
    if !calc.is_empty() {
        f.push(format!("calcs={}", calc.join(",")));
    }
    let other: Vec<String> = node
        .children
        .iter()
        .map(|c| hex(c.segment.0))
        .filter(|k| {
            !matches!(
                k.as_str(),
                "02" | "03" | "10" | "d8" | "d9" | "fc" | "fc01" | "fc02" | "fc03"
            )
        })
        .collect();
    if !other.is_empty() {
        f.push(format!("keys={}", other.join(",")));
    }
    out.row("vl", file, &f);
    Some(())
}

fn trigger_cache(out: &mut Out, file: &str, layout: &str, owner: &str, node: &Node<'_>) {
    // ScriptTriggerCache::Read (libFMEngine 0xfea760): `04` count (u16) and
    // `05/<event>` members; ScriptTrigger::Read (0xfe9de0) reads `01` and
    // the parameter formula at `31`.
    let count = node
        .get(4)
        .and_then(|n| n.value)
        .map(hex)
        .unwrap_or_default();
    let members = node
        .get(5)
        .map(|n| n.children.as_slice())
        .unwrap_or_default();
    if members.is_empty() {
        out.row(
            "trigger",
            file,
            &[
                layout.into(),
                owner.into(),
                "event=none".into(),
                format!("count={count}"),
            ],
        );
    }
    for m in members {
        let Some(v) = m.get(1).and_then(|n| n.assembled()) else {
            out.row(
                "error",
                file,
                &[layout.into(), owner.into(), "trigger without 01".into()],
            );
            continue;
        };
        let parse = || -> Option<Vec<String>> {
            let mut s = Stream::new(&v);
            let event = s.u16()?;
            let script = key_id(s.key()?);
            let fileref = key_id(s.key()?);
            let flags = s.u32()?;
            let option = s.u32()?;
            let extra = s.u16()?;
            let extra_bytes = s.take(extra as usize)?;
            let mut f = vec![
                layout.into(),
                owner.into(),
                format!("event={event}"),
                format!("member={}", hex(m.segment.0)),
                format!("script={script}"),
                format!("fileRef={fileref}"),
                format!("flags={flags:#x}"),
                format!("option={option:#x}"),
                format!("extra={}", hex(extra_bytes)),
                format!("count={count}"),
            ];
            if let Some(p) = m.get(0x31) {
                let keys: Vec<String> = p.children.iter().map(|c| hex(c.segment.0)).collect();
                f.push(format!("parameter={}", keys.join(",")));
            }
            if !s.at_end() {
                f.push(format!("trailing={}", hex(&v[s.pos..])));
            }
            Some(f)
        };
        match parse() {
            Some(f) => out.row("trigger", file, &f),
            None => out.row(
                "error",
                file,
                &[
                    layout.into(),
                    owner.into(),
                    format!("short trigger {}", hex(&v)),
                ],
            ),
        }
    }
}

fn layout_triggers(out: &mut Out, file: &str, layout_id: &str, layout: &Node<'_>, prefix: &str) {
    if let Some(t) = layout.child(&[0x30]) {
        trigger_cache(out, file, layout_id, &format!("{prefix}layout"), t);
    }
    for obj in layout
        .get(5)
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
    {
        if let Some(t) = obj.child(&[0x30]) {
            trigger_cache(
                out,
                file,
                layout_id,
                &format!("{prefix}object:{}", hex(obj.segment.0)),
                t,
            );
        }
    }
}

fn ids(out: &mut Out, file: &str, root: &Node<'_>) {
    let catalogs: [(&str, &[u8]); 8] = [
        ("table", &[3, 0x10]),
        ("occurrence", &[3, 0x11]),
        ("relationship", &[3, 0xfb]),
        ("layout", &[4]),
        ("script", &[0x11]),
        ("valuelist", &[0x21]),
        ("function", &[0x10]),
        ("account", &[0x17, 1]),
    ];
    for (name, path) in catalogs {
        let mut node = Some(root);
        for seg in path {
            node = node.and_then(|n| n.child(&[*seg]));
        }
        for m in node
            .and_then(|n| n.get(5))
            .map(|n| n.children.as_slice())
            .unwrap_or_default()
        {
            let label = m.get(0x10).and_then(text_of).unwrap_or_default();
            let id = serial::decode(m.segment.0)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into());
            out.row(
                "id",
                file,
                &[name.into(), hex(m.segment.0), id, clean(&label)],
            );
        }
    }
    // Fields per base table.
    for t in &root.children {
        if t.segment.0.first().map_or(true, |b| *b < 0x80) || t.segment.0.len() != 2 {
            continue;
        }
        for m in t
            .child(&[3])
            .and_then(|n| n.get(5))
            .map(|n| n.children.as_slice())
            .unwrap_or_default()
        {
            let label = m.get(0x10).and_then(text_of).unwrap_or_default();
            let id = serial::decode(m.segment.0)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into());
            out.row(
                "id",
                file,
                &[
                    format!("field:{}", key_id(&[2, t.segment.0[0], t.segment.0[1]])),
                    hex(m.segment.0),
                    id,
                    clean(&label),
                ],
            );
        }
    }
}

fn inspect(out: &mut Out, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let file_name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = HbamFile::open(path)?;
    let tree = file.key_tree()?;
    let root = &tree.root;
    let status = root.child(&[2]);
    match status
        .and_then(|n| n.child(&[0x0d]))
        .and_then(|n| n.assembled())
    {
        Some(v) => {
            if locale(out, &file_name, &v).is_none() {
                out.row(
                    "error",
                    &file_name,
                    &["02/0d".into(), format!("short locale {}", hex(&v))],
                );
            }
        }
        None => out.row("locale", &file_name, &["type=absent".into()]),
    }
    match status
        .and_then(|n| n.child(&[0x18]))
        .and_then(|n| n.assembled())
    {
        Some(v) => {
            if prefs(out, &file_name, &v).is_none() {
                out.row(
                    "error",
                    &file_name,
                    &["02/18".into(), format!("short prefs {}", hex(&v))],
                );
            }
        }
        None => out.row("prefs", &file_name, &["options=absent".into()]),
    }
    let rels = root
        .child(&[3])
        .and_then(|n| n.child(&[0xfb]))
        .and_then(|n| n.get(5));
    for r in rels.map(|n| n.children.as_slice()).unwrap_or_default() {
        let id = serial::decode(r.segment.0)
            .map(|v| v.to_string())
            .unwrap_or_default();
        if relationship(out, &file_name, &id, r).is_none() {
            out.row(
                "error",
                &file_name,
                &[
                    format!("03/fb/05/{}", hex(r.segment.0)),
                    "undecodable relationship".into(),
                ],
            );
        }
    }
    for t in &root.children {
        if t.segment.0.len() != 2 || t.segment.0[0] < 0x80 {
            continue;
        }
        let table = key_id(&[2, t.segment.0[0], t.segment.0[1]]);
        for fld in t
            .child(&[3])
            .and_then(|n| n.get(5))
            .map(|n| n.children.as_slice())
            .unwrap_or_default()
        {
            let is_summary = fld.get(2).and_then(|d| d.value).and_then(|d| d.first()) == Some(&3);
            if is_summary {
                let field = serial::decode(fld.segment.0)
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let def = fld
                    .get(9)
                    .and_then(|n| n.assembled())
                    .map(|v| v.into_owned())
                    .unwrap_or_default();
                summary(out, &file_name, &table, &field, &def);
            }
        }
    }
    for v in root
        .child(&[0x21])
        .and_then(|n| n.get(5))
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
    {
        let id = serial::decode(v.segment.0)
            .map(|v| v.to_string())
            .unwrap_or_default();
        if value_list(out, &file_name, &id, v).is_none() {
            out.row(
                "error",
                &file_name,
                &[
                    format!("21/05/{}", hex(v.segment.0)),
                    "undecodable value list".into(),
                ],
            );
        }
    }
    for l in root
        .child(&[4])
        .and_then(|n| n.get(5))
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
    {
        let id = serial::decode(l.segment.0)
            .map(|v| v.to_string())
            .unwrap_or_default();
        layout_triggers(out, &file_name, &id, l, "");
        if let Some(table_view) = l.get(0x0d) {
            layout_triggers(out, &file_name, &id, table_view, "tableview:");
        }
    }
    ids(out, &file_name, root);
    Ok(())
}

fn main() {
    let mut out = Out {
        summary: false,
        counts: BTreeMap::new(),
    };
    let mut files = 0;
    for arg in std::env::args().skip(1) {
        if arg == "--summary" {
            out.summary = true;
            continue;
        }
        files += 1;
        if let Err(e) = inspect(&mut out, &arg) {
            out.row("error", &arg, &["open".into(), e.to_string()]);
        }
    }
    if out.summary {
        println!("files\t{files}");
        for (k, v) in &out.counts {
            println!("{k}\t{v}");
        }
    }
}
