//! A [`SqlSource`] and [`CalcHost`] over a local `.fmp12` file, for tests
//! and tools: the file's occurrences, fields, records, globals, container
//! names, relationships and value lists, read with `om-hbam`, and its
//! calculation fields computed with `om-calc` (stored results where the
//! record has them, the formula otherwise).
//!
//! Where things are (file-format.md 3.4-3.6, schema-path.md 5, containers.md
//! 2, calc-encoding.md 8):
//!
//! | path | holds |
//! |---|---|
//! | `<T>/3/5/<F>/2` | field definition: kind, type, option word (bytes 8-11: `0x10000` global, `0x20000` unstored calculation), repetitions (bytes 24-25) |
//! | `<T>/3/5/<F>/5` | a calculation's formula (`4` context, `5` text, `6` Polish) |
//! | `<T>/3/5/<F>/252`, `<T>/3/1/252`, `3/17/5/<O>/252` | modification counts (a length byte and a serial key) |
//! | `<T>/5/<R>/<F>` | repetition 1 of a field; repetition n at `F * 256 + n`; `252` the record's modification count |
//! | `<T>/8/<F>` | a global field's value |
//! | `<T>/31/5/<M>/FNAM` | a container library member's file name; the record holds `hex(M)` + U+E000 |
//! | `3/251/5/<R>` | relationships: `2` the two occurrences, `3/<n>` the join predicates |
//! | `33/5/<V>` | value lists: `16` name, `3` custom values, `2` definition (field or related) |
//!
//! Occurrences of another file are "missing": no fields, no records.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use om_calc::eval::{
    self, CalcHost, Context, FieldInfo, HostFunction, Storage, VarScope, Variables,
};
use om_calc::ops::{self, CompareOp};
use om_calc::{CalcError, Container, Ctx, Date, Locale, Text, Time, Timestamp, Value, ValueType};
use om_formula::{Formula, NameTable, Names};
use om_hbam::{serial, Cell, HbamFile, KeyTree, Node};

use crate::error::SqlError;
use crate::source::{
    BaseTableDef, Cursor, FieldClass, FieldDef, FieldType, SqlSource, TableDef, ValueListDef,
    ValueListValues,
};

const GLOBAL: u32 = 0x0001_0000;
const UNSTORED: u32 = 0x0002_0000;
const EVALUATE_ALWAYS: u32 = 0x0000_0020;
const MOD_COUNT: u64 = 252;
const FNAM: u64 = 0x464E_414D;

struct Field {
    def: FieldDef,
    formula: Option<Arc<Formula>>,
    unstored: bool,
    /// Evaluated even when every field it references is empty (option
    /// `0x20`; without it the result is empty then).
    always: bool,
    /// The fields the formula references.
    references: Vec<om_formula::Field>,
}

struct Record {
    id: u64,
    mod_count: u64,
    cells: BTreeMap<u64, Cell>,
}

struct Table {
    id: u64,
    name: String,
    mod_count: u64,
    fields: Vec<Field>,
    records: Vec<Record>,
    globals: BTreeMap<u64, Cell>,
}

struct Occurrence {
    def: TableDef,
    table: Option<u64>,
}

/// A relationship's join predicates: operator, left field, right field.
type Predicates = Vec<(u8, u64, u64)>;

struct Relationship {
    left: u64,
    right: u64,
    /// Operator, left field, right field.
    predicates: Vec<(u8, u64, u64)>,
}

struct ValueList {
    def: ValueListDef,
    values: Option<Vec<String>>,
    field: Option<(u64, u64)>,
}

/// A local `.fmp12` file as a SQL source and calculation host.
pub struct FileHost {
    file: HbamFile,
    locale: Locale,
    now: Timestamp,
    user: String,
    tables: BTreeMap<u64, Table>,
    occurrences: Vec<Occurrence>,
    relationships: Vec<Relationship>,
    value_lists: Vec<ValueList>,
    names: NameTable,
    locals: Variables,
    globals: Variables,
    selectors: HashMap<u8, Value>,
    file_name: String,
    /// The cursors of the calculations being computed for a query.
    cursors: Vec<Cursor>,
}

fn key_bytes(id: u64) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    bytes[first..].to_vec()
}

/// A catalog id's serial-key value (`TableId`, `BaseTableId`).
fn serial_value(id: u64) -> u64 {
    serial::decode(&key_bytes(id)).unwrap_or(id)
}

/// The cell of repetition `rep` of field `field` in a record: the field's
/// own key for the first, `F * 256 + n` after it. `None` when that does not
/// fit in a key: the field id comes from the file, and a repetition whose
/// key would pass `u64::MAX` cannot have a cell, so it reads as empty.
fn cell_key(field: u64, rep: u32) -> Option<u64> {
    if rep <= 1 {
        return Some(field);
    }
    field.checked_mul(256)?.checked_add(u64::from(rep))
}

/// A modification count as stored: a length byte and a serial key.
fn mod_count(value: Option<&[u8]>) -> u64 {
    let Some((&len, key)) = value.and_then(|v| v.split_first()) else {
        return 0;
    };
    if usize::from(len) != key.len() {
        return 0;
    }
    serial::decode(key).unwrap_or(0)
}

/// One key string `[u8 total][u8 len][segment]`, advancing `input`.
fn read_key(input: &mut &[u8]) -> Option<u64> {
    let (&length, rest) = input.split_first()?;
    let key = rest.get(..usize::from(length))?;
    let remaining = rest.get(usize::from(length)..)?;
    let (&key_length, number) = key.split_first()?;
    if usize::from(key_length) != number.len() {
        return None;
    }
    let value = om_hbam::slot::be_uint(number)?;
    *input = remaining;
    Some(value)
}

/// A value node's bytes: plain, or a chunked value's pieces `1..n` after its
/// header `0` (other children, such as `255`, are not part of the value).
fn node_bytes(node: &Node<'_>) -> Option<Vec<u8>> {
    if let Some(v) = node.value {
        return Some(v.to_vec());
    }
    node.get(0)?;
    let mut out = Vec::new();
    for n in 1.. {
        match node.get(n) {
            Some(piece) => out.extend_from_slice(piece.value?),
            None => break,
        }
    }
    Some(out)
}

fn cell_bytes(cell: &Cell) -> Option<Vec<u8>> {
    match cell {
        Cell::Value(v) => Some(v.clone()),
        Cell::Nested(items) => {
            let mut pieces: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
            let mut header = false;
            for (path, value) in items {
                if path.len() != 1 {
                    continue;
                }
                let id = om_hbam::slot::be_uint(&path[0])?;
                match (id, value) {
                    (0, Some(_)) => header = true,
                    (n, Some(v)) if n > 0 && n < 255 => {
                        pieces.insert(n, v.clone());
                    }
                    _ => {}
                }
            }
            header.then(|| pieces.into_values().flatten().collect())
        }
    }
}

fn text_of_cell(cell: &Cell) -> Option<String> {
    om_scsu::decode_scsu(&cell_bytes(cell)?).ok()
}

impl FileHost {
    /// Opens a file read-only with its own locale (the record `[2, 0d]`,
    /// file-metadata.md 2; en-US when it has none that decodes); `now` is
    /// the clock queries see.
    pub fn open_file(path: impl AsRef<Path>, now: Timestamp) -> Result<FileHost, String> {
        let path = path.as_ref();
        let file = HbamFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let locale = file
            .key_tree_under(&[2])
            .ok()
            .and_then(|tree| Locale::from_record(&tree.at(&[2, 0x0d])?.assembled()?))
            .unwrap_or_else(Locale::en_us);
        FileHost::with_file(file, path, locale, now)
    }

    /// Opens a file read-only with `locale` in place of the file's (its
    /// stored dates and numbers are text in it) and `now` the clock queries
    /// see. The file's name without extension is
    /// `FileMaker_Tables.BaseFileName`.
    pub fn open(
        path: impl AsRef<Path>,
        locale: Locale,
        now: Timestamp,
    ) -> Result<FileHost, String> {
        let path = path.as_ref();
        let file = HbamFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        FileHost::with_file(file, path, locale, now)
    }

    fn with_file(
        file: HbamFile,
        path: &Path,
        locale: Locale,
        now: Timestamp,
    ) -> Result<FileHost, String> {
        let file_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let catalog = file.catalog().map_err(|e| e.to_string())?;
        let mut host = FileHost {
            file,
            locale,
            now,
            user: "Admin".to_string(),
            tables: BTreeMap::new(),
            occurrences: Vec::new(),
            relationships: Vec::new(),
            value_lists: Vec::new(),
            names: NameTable::default(),
            locals: Variables::new(),
            globals: Variables::new(),
            selectors: HashMap::new(),
            file_name: file_name.clone(),
            cursors: Vec::new(),
        };
        for t in &catalog.tables {
            let table = host.read_table(t).map_err(|e| e.to_string())?;
            host.tables.insert(t.id, table);
        }
        let schema = host.file.key_tree_under(&[3]).map_err(|e| e.to_string())?;
        for o in &catalog.table_occurrences {
            let table = o.base_table().filter(|t| host.tables.contains_key(t));
            let base = table.map(|t| (host.tables[&t].name.clone(), file_name.clone()));
            let mod_count = mod_count(
                schema
                    .at(&[3, 17, 5, o.id, MOD_COUNT])
                    .and_then(|n| n.value),
            );
            host.occurrences.push(Occurrence {
                def: TableDef {
                    id: o.id,
                    name: o.name.clone(),
                    table_id: serial_value(o.id),
                    base,
                    mod_count,
                },
                table,
            });
            if let Some(t) = table {
                host.names.add_occurrence(o.id, &o.name, t);
            }
        }
        host.occurrences.sort_by_key(|o| o.def.name.to_lowercase());
        for t in host.tables.values() {
            for f in &t.fields {
                host.names.add_field(t.id, f.def.id, &f.def.name);
                host.names
                    .set_summary_field(t.id, f.def.id, f.def.class == FieldClass::Summary);
            }
        }
        host.relationships = read_relationships(&schema);
        host.value_lists = host.read_value_lists().map_err(|e| e.to_string())?;
        Ok(host)
    }

    /// The account `USER` returns (`Admin` unless set).
    pub fn set_user(&mut self, user: &str) {
        self.user = user.to_string();
    }

    /// The answer to `Get ( selector )` in calculations on this host.
    pub fn set_get(&mut self, selector: u8, value: Value) {
        self.selectors.insert(selector, value);
    }

    /// The occurrence named `name` (case-insensitive).
    pub fn occurrence(&self, name: &str) -> Option<u64> {
        self.occurrences
            .iter()
            .find(|o| o.def.name.eq_ignore_ascii_case(name))
            .map(|o| o.def.id)
    }

    fn read_table(&self, t: &om_hbam::Table) -> om_hbam::Result<Table> {
        let tree = self.file.key_tree_under(&[t.id, 3])?;
        let mut fields = Vec::new();
        for f in &t.fields {
            let node = tree.at(&[t.id, 3, 5, f.id]);
            let o = &f.options;
            let word = |at: usize| {
                o.get(at..at + 4)
                    .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            };
            let reps = o
                .get(24..26)
                .map_or(1, |b| u32::from(u16::from_be_bytes([b[0], b[1]])))
                .max(1);
            let class = match f.kind {
                om_hbam::FieldKind::Calculation => FieldClass::Calculated,
                om_hbam::FieldKind::Summary => FieldClass::Summary,
                _ => FieldClass::Normal,
            };
            let ty = match f.data_type {
                om_hbam::DataType::Number => FieldType::Number,
                om_hbam::DataType::Date => FieldType::Date,
                om_hbam::DataType::Time => FieldType::Time,
                om_hbam::DataType::Timestamp => FieldType::Timestamp,
                om_hbam::DataType::Container => FieldType::Container,
                _ => FieldType::Text,
            };
            let formula = node
                .and_then(|n| n.get(5))
                .and_then(|n| {
                    let polish = node_bytes(n.get(6)?)?;
                    let context = n.get(4).and_then(node_bytes);
                    Formula::decode(context.as_deref(), None, &polish).ok()
                })
                .filter(|_| class == FieldClass::Calculated)
                .map(Arc::new);
            let references = formula
                .as_ref()
                .map(|f| f.polish.field_references())
                .unwrap_or_default();
            fields.push(Field {
                always: word(8) & EVALUATE_ALWAYS != 0,
                references,
                def: FieldDef {
                    id: f.id,
                    name: f.name.clone(),
                    ty,
                    class,
                    global: word(8) & GLOBAL != 0,
                    repetitions: reps,
                    mod_count: mod_count(node.and_then(|n| n.get(MOD_COUNT)).and_then(|n| n.value)),
                },
                formula,
                unstored: word(8) & UNSTORED != 0,
            });
        }
        let table_mod = mod_count(tree.at(&[t.id, 3, 1, MOD_COUNT]).and_then(|n| n.value));
        let records = self
            .file
            .records(t.id)?
            .into_iter()
            .map(|r| Record {
                id: r.id,
                mod_count: match r.cells.get(&MOD_COUNT) {
                    Some(Cell::Value(v)) => mod_count(Some(v)),
                    _ => 0,
                },
                cells: r.cells,
            })
            .collect();
        let globals_tree = self.file.key_tree_under(&[t.id, 8])?;
        let globals = globals_tree
            .at(&[t.id, 8])
            .map(|n| {
                n.children
                    .iter()
                    .filter_map(|c| Some((c.id()?, Cell::from_node(c))))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Table {
            id: t.id,
            name: t.name.clone(),
            mod_count: table_mod,
            fields,
            records,
            globals,
        })
    }

    fn read_value_lists(&self) -> om_hbam::Result<Vec<ValueList>> {
        let tree = self.file.key_tree_under(&[33])?;
        let Some(entries) = tree.at(&[33, 5]) else {
            return Ok(Vec::new());
        };
        let mut lists = Vec::new();
        for e in &entries.children {
            let Some(id) = e.id() else { continue };
            let name = e
                .get(16)
                .and_then(node_bytes)
                .and_then(|b| om_scsu::decode_scsu(&b).ok())
                .unwrap_or_default();
            let definition = e.get(2).and_then(node_bytes);
            let (source, field) = match definition.as_deref() {
                Some([1, _, related, _, rest @ ..]) => {
                    let mut input: &[u8] = rest;
                    let mut input_with_len =
                        definition.as_deref().unwrap_or(&[]).get(3..).unwrap_or(&[]);
                    let occurrence = read_key(&mut input_with_len);
                    let field = read_key(&mut input_with_len);
                    let _ = &mut input;
                    let src = if *related == 1 { "Related" } else { "Field" };
                    (src, occurrence.zip(field))
                }
                _ => ("Custom", None),
            };
            let values = e
                .get(3)
                .and_then(node_bytes)
                .and_then(|b| om_scsu::decode_scsu(&b).ok())
                .map(|s| s.split('\r').map(str::to_string).collect());
            lists.push(ValueList {
                def: ValueListDef {
                    name,
                    id,
                    source: source.to_string(),
                    mod_count: mod_count(e.get(MOD_COUNT).and_then(|n| n.value)),
                },
                values: if source == "Custom" { values } else { None },
                field,
            });
        }
        lists.sort_by_key(|l| l.def.name.to_lowercase());
        Ok(lists)
    }

    fn table_of(&self, occurrence: u64) -> Option<&Table> {
        let o = self.occurrences.iter().find(|o| o.def.id == occurrence)?;
        self.tables.get(&o.table?)
    }

    fn cx(&self) -> Ctx<'_> {
        Ctx::new(&self.locale, self.now.date())
    }

    /// The text form of a container value: the library member's file name.
    fn container_name(&self, table: u64, text: &str) -> String {
        let Some(hex) = text.strip_suffix('\u{e000}') else {
            return text.to_string();
        };
        let Ok(member) = u64::from_str_radix(hex, 16) else {
            return text.to_string();
        };
        let segments: Vec<Vec<u8>> = [table, 31, 5, member, FNAM]
            .iter()
            .map(|id| key_bytes(*id).into_iter().collect::<Vec<u8>>())
            .map(|k| if k.is_empty() { vec![0] } else { k })
            .collect();
        let path: Vec<&[u8]> = segments.iter().map(Vec::as_slice).collect();
        let bytes = self
            .file
            .subtree(&path)
            .ok()
            .flatten()
            .and_then(|n| node_bytes(&n));
        bytes.and_then(|b| fnam_name(&b)).unwrap_or_default()
    }

    /// A stored value by field type.
    fn decode(&self, table: u64, field: &FieldDef, cell: Option<&Cell>) -> Value {
        let ty = value_type(field.ty);
        let Some(text) = cell.and_then(text_of_cell) else {
            return Value::empty(ty);
        };
        let cx = self.cx();
        match field.ty {
            FieldType::Text => Value::text(text),
            FieldType::Number => {
                if text.is_empty() {
                    Value::empty(ty)
                } else {
                    Value::Text(Text::plain(text)).convert_declared(ValueType::Number, &cx)
                }
            }
            FieldType::Date => match Date::parse(&text, cx.locale, cx.today) {
                Ok(Some(d)) => Value::Date(d),
                _ => Value::empty(ty),
            },
            FieldType::Time => match Time::parse(&text, cx.locale, cx.today) {
                Ok(Some(t)) => Value::Time(t),
                _ => Value::empty(ty),
            },
            FieldType::Timestamp => match Timestamp::parse(&text, cx.locale, cx.today) {
                Ok(Some(t)) => Value::Timestamp(t),
                _ => Value::empty(ty),
            },
            FieldType::Container => Value::Container(Container {
                text: Text::plain(self.container_name(table, &text)),
                ..Container::default()
            }),
        }
    }

    fn stored<'t>(
        &self,
        table: &'t Table,
        record: u64,
        field: &FieldDef,
        rep: u32,
    ) -> Option<Option<&'t Cell>> {
        let key = cell_key(field.id, rep);
        if field.global {
            return Some(key.and_then(|key| table.globals.get(&key)));
        }
        let r = table.records.iter().find(|r| r.id == record)?;
        Some(key.and_then(|key| r.cells.get(&key)))
    }

    /// A field's value, computing calculations the record does not store.
    fn field_on(
        &mut self,
        occurrence: u64,
        record: u64,
        field: u64,
        rep: u32,
        cursor: Cursor,
    ) -> Result<Value, CalcError> {
        let table = self.table_of(occurrence).ok_or(CalcError::FIELD_MISSING)?;
        let f = table
            .fields
            .iter()
            .find(|f| f.def.id == field)
            .ok_or(CalcError::FIELD_MISSING)?;
        let def = f.def.clone();
        if def.class == FieldClass::Summary {
            return Ok(Value::empty(value_type(def.ty)));
        }
        let stored = self.stored(table, record, &def, rep).flatten().cloned();
        let formula = f.formula.clone();
        let (always, references) = (f.always, f.references.clone());
        let tid = table.id;
        if def.class == FieldClass::Calculated && f.unstored && !def.global {
            if let Some(formula) = formula {
                let context = Context {
                    occurrence: formula.occurrence().or(Some(occurrence)),
                    record: Some(record),
                    repetition: if def.repetitions > 1 { rep } else { 0 },
                    result_type: Some(value_type(def.ty)),
                    ..Context::default()
                };
                // A calculation sees the query's found set; it is empty
                // when every field it references is (unless set to
                // evaluate always), and a calculation error reads as empty.
                // Containers do not count as empty references (a formula
                // over an empty photo still evaluates).
                self.cursors.push(cursor);
                let skip = !always
                    && !references.is_empty()
                    && references.iter().all(|r| {
                        let occurrence = r.occurrence.id().or(context.occurrence).unwrap_or(0);
                        let container = r
                            .field
                            .id()
                            .and_then(|f| self.field(occurrence, f))
                            .is_some_and(|i| i.value_type == ValueType::Container);
                        !container
                            && eval::evaluate(&om_formula::Expr::Field(r.clone()), self, &context)
                                .value
                                .is_empty()
                    });
                let e = if skip {
                    None
                } else {
                    Some(eval::evaluate_formula(&formula, self, &context))
                };
                self.cursors.pop();
                let Some(e) = e else {
                    return Ok(Value::empty(value_type(def.ty)));
                };
                if e.value.is_invalid() {
                    return Ok(Value::empty(value_type(def.ty)));
                }
                return Ok(e.value);
            }
        }
        Ok(self.decode(tid, &def, stored.as_ref()))
    }

    fn records_of(&self, occurrence: u64) -> Vec<u64> {
        self.table_of(occurrence)
            .map(|t| t.records.iter().map(|r| r.id).collect())
            .unwrap_or_default()
    }

    /// The occurrences from `from` to `to` along relationships, each step
    /// with its predicates oriented from the earlier occurrence.
    fn path(&self, from: u64, to: u64) -> Option<Vec<(u64, Predicates)>> {
        let mut previous: HashMap<u64, (u64, Predicates)> = HashMap::new();
        let mut queue = VecDeque::from([from]);
        while let Some(at) = queue.pop_front() {
            if at == to {
                let mut path = Vec::new();
                let mut node = to;
                while node != from {
                    let (before, preds) = previous[&node].clone();
                    path.push((node, preds));
                    node = before;
                }
                path.reverse();
                return Some(path);
            }
            for r in &self.relationships {
                let step = if r.left == at {
                    Some((r.right, r.predicates.clone()))
                } else if r.right == at {
                    Some((
                        r.left,
                        r.predicates
                            .iter()
                            .map(|&(op, l, rt)| (op, rt, l))
                            .collect(),
                    ))
                } else {
                    None
                };
                if let Some((next, preds)) = step {
                    if next != from && !previous.contains_key(&next) {
                        previous.insert(next, (at, preds));
                        queue.push_back(next);
                    }
                }
            }
        }
        None
    }
}

/// The file name of an `FNAM` stream (containers.md 2.4): the name of its
/// first entry, the last component of a path.
fn fnam_name(bytes: &[u8]) -> Option<String> {
    let rest = bytes.get(4..)?;
    let (&type_len, rest) = rest.split_first()?;
    let rest = rest.get(usize::from(type_len)..)?;
    if rest.len() >= 2 {
        let len = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
        if rest.len() == 2 + len {
            return om_scsu::decode_scsu(&rest[2..]).ok();
        }
    }
    // Older reference lists: path components, the last one length-prefixed.
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        let len = usize::from(bytes[i]);
        if i + 1 + len == bytes.len() && len > 0 && i > 0 && bytes[i - 1] == 0xda {
            return om_scsu::decode_scsu(&bytes[i + 1..]).ok();
        }
    }
    None
}

fn read_relationships(tree: &KeyTree<'_>) -> Vec<Relationship> {
    let Some(entries) = tree.at(&[3, 251, 5]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in &entries.children {
        let relation = (|| {
            let definition = node_bytes(entry.get(2)?)?;
            let mut input = definition.as_slice();
            let left = read_key(&mut input)?;
            let right = read_key(&mut input)?;
            let mut predicates = Vec::new();
            for p in &entry.get(3)?.children {
                let bytes = node_bytes(p)?;
                let (&op, mut rest) = bytes.split_first()?;
                let lf = read_key(&mut rest)?;
                let rf = read_key(&mut rest)?;
                predicates.push((op, lf, rf));
            }
            Some(Relationship {
                left,
                right,
                predicates,
            })
        })();
        out.extend(relation);
    }
    out
}

fn value_type(t: FieldType) -> ValueType {
    match t {
        FieldType::Text => ValueType::Text,
        FieldType::Number => ValueType::Number,
        FieldType::Date => ValueType::Date,
        FieldType::Time => ValueType::Time,
        FieldType::Timestamp => ValueType::Timestamp,
        FieldType::Container => ValueType::Container,
    }
}

const CURRENT_DATE: u8 = 2;
const FILE_NAME: u8 = 5;
const FOUND_COUNT: u8 = 7;
const TOTAL_RECORD_COUNT: u8 = 19;
const RECORD_NUMBER: u8 = 20;
const CURRENT_TIME: u8 = 29;
const ACCOUNT_NAME: u8 = 32;
const RECORD_ID: u8 = 37;
const RECORD_MODIFICATION_COUNT: u8 = 38;
const CURRENT_HOST_TIMESTAMP: u8 = 60;
const CURRENT_TIMESTAMP: u8 = 63;

impl CalcHost for FileHost {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    fn today(&self) -> Date {
        self.now.date()
    }

    fn names(&self) -> &dyn Names {
        &self.names
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        let f = self
            .table_of(occurrence)?
            .fields
            .iter()
            .find(|f| f.def.id == field)?;
        let storage = match (f.def.class, f.def.global) {
            (FieldClass::Summary, _) => Storage::Summary,
            (_, true) => Storage::Global,
            (FieldClass::Calculated, _) if f.unstored => match &f.formula {
                Some(formula) => Storage::Unstored(formula.clone()),
                None => Storage::Stored,
            },
            _ => Storage::Stored,
        };
        Some(FieldInfo {
            value_type: value_type(f.def.ty),
            repetitions: f.def.repetitions,
            storage,
        })
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        rep: u32,
    ) -> Result<Value, CalcError> {
        let table = self.table_of(occurrence).ok_or(CalcError::FIELD_MISSING)?;
        let f = table
            .fields
            .iter()
            .find(|f| f.def.id == field)
            .ok_or(CalcError::FIELD_MISSING)?;
        if f.def.class == FieldClass::Summary {
            return Ok(Value::empty(value_type(f.def.ty)));
        }
        let stored = self.stored(table, record, &f.def, rep).flatten();
        Ok(self.decode(table.id, &f.def, stored))
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        let missing = || CalcError::from_code(103).expect("nonzero");
        let path = self.path(occurrence, related).ok_or_else(missing)?;
        let cx = self.cx();
        let mut at = occurrence;
        let mut records = vec![record];
        for (next, predicates) in path {
            let candidates = self.records_of(next);
            let mut found = Vec::new();
            for candidate in candidates {
                let all = predicates.iter().all(|&(op, left, right)| {
                    if op == 6 {
                        return true;
                    }
                    records.iter().any(|&r| {
                        let key = self
                            .field_value(at, r, left, 1)
                            .unwrap_or_else(|_| Value::empty_text());
                        let other = self
                            .field_value(next, candidate, right, 1)
                            .unwrap_or_else(|_| Value::empty_text());
                        let cmp = match op {
                            1 => CompareOp::Ne,
                            2 => CompareOp::Lt,
                            3 => CompareOp::Le,
                            4 => CompareOp::Gt,
                            5 => CompareOp::Ge,
                            _ => CompareOp::Eq,
                        };
                        !key.is_empty()
                            && !other.is_empty()
                            && ops::compare_op(&key, cmp, &other, &cx).is_true(&cx)
                    })
                });
                if all {
                    found.push(candidate);
                }
            }
            records = found;
            at = next;
        }
        Ok(records)
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        Ok(self.records_of(occurrence).get(index).copied())
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        self.table_of(occurrence).map(|t| t.name.as_str())
    }

    fn get(&mut self, selector: u8, context: &Context) -> Result<Value, CalcError> {
        if let Some(v) = self.selectors.get(&selector) {
            return Ok(v.clone());
        }
        let records = context
            .occurrence
            .map(|o| self.records_of(o))
            .unwrap_or_default();
        let position = context
            .record
            .and_then(|r| records.iter().position(|id| *id == r));
        let cursor = self.cursors.last().cloned();
        Ok(match (selector, context.record) {
            (FOUND_COUNT, Some(_)) if cursor.is_some() => {
                Value::number(cursor.as_ref().map_or(0, |c| c.found_count) as i64)
            }
            (RECORD_NUMBER, Some(_)) if cursor.is_some() => {
                Value::number(cursor.as_ref().map_or(0, |c| c.record_number) as i64)
            }
            (RECORD_ID, Some(r)) => Value::number(r as i64),
            (RECORD_ID, None) => Value::empty_text(),
            (RECORD_NUMBER, _) => Value::number(position.map_or(0, |p| p as i64 + 1)),
            (FOUND_COUNT | TOTAL_RECORD_COUNT, Some(_)) => Value::number(records.len() as i64),
            (FOUND_COUNT | TOTAL_RECORD_COUNT, None) => Value::number(0),
            (RECORD_MODIFICATION_COUNT, Some(r)) => {
                let t = context.occurrence.and_then(|o| self.table_of(o));
                let n = t
                    .and_then(|t| t.records.iter().find(|x| x.id == r))
                    .map_or(0, |x| x.mod_count);
                Value::number(n as i64)
            }
            (CURRENT_DATE, _) => Value::Date(self.now.date()),
            (CURRENT_TIME, _) => Value::Time(self.now.time_of_day()),
            (CURRENT_TIMESTAMP | CURRENT_HOST_TIMESTAMP, _) => Value::Timestamp(self.now),
            (ACCOUNT_NAME, _) => Value::text(self.user.as_str()),
            (FILE_NAME, _) => Value::text(self.file_name.as_str()),
            _ => Value::empty_text(),
        })
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        match scope {
            VarScope::Local => &mut self.locals,
            VarScope::Global => &mut self.globals,
        }
    }

    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> (Value, Option<CalcError>) {
        if let Some(r) = crate::calc::host_call(self, function, args) {
            return r;
        }
        match self.call(function, args, context) {
            Ok(v) => (v, None),
            Err(e) => (Value::invalid(), Some(e)),
        }
    }
}

impl SqlSource for FileHost {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    fn now(&self) -> Timestamp {
        self.now
    }

    fn user_name(&self) -> String {
        self.user.clone()
    }

    fn tables(&self) -> Vec<TableDef> {
        self.occurrences.iter().map(|o| o.def.clone()).collect()
    }

    fn fields(&self, table: &TableDef) -> Vec<FieldDef> {
        self.table_of(table.id)
            .map(|t| t.fields.iter().map(|f| f.def.clone()).collect())
            .unwrap_or_default()
    }

    fn records(&mut self, table: &TableDef) -> Result<Vec<u64>, SqlError> {
        Ok(self.records_of(table.id))
    }

    fn value(
        &mut self,
        table: &TableDef,
        record: u64,
        field: &FieldDef,
        repetition: u32,
        cursor: Cursor,
    ) -> Result<Value, CalcError> {
        self.field_on(table.id, record, field.id, repetition, cursor)
    }

    fn mod_count(&mut self, table: &TableDef, record: u64) -> u64 {
        self.table_of(table.id)
            .and_then(|t| t.records.iter().find(|r| r.id == record))
            .map_or(0, |r| r.mod_count)
    }

    fn base_tables(&self) -> Vec<BaseTableDef> {
        let mut out: Vec<BaseTableDef> = self
            .tables
            .values()
            .map(|t| BaseTableDef {
                name: t.name.clone(),
                id: serial_value(t.id),
                source: "<Internal>".to_string(),
                mod_count: t.mod_count,
                fields: t.fields.iter().map(|f| f.def.clone()).collect(),
            })
            .collect();
        out.sort_by_key(|b| b.name.to_lowercase());
        out
    }

    fn value_lists(&self) -> Vec<ValueListDef> {
        self.value_lists.iter().map(|v| v.def.clone()).collect()
    }

    fn value_list_values(&mut self, name: &str) -> Option<ValueListValues> {
        let list = self
            .value_lists
            .iter()
            .find(|v| v.def.name.eq_ignore_ascii_case(name))?;
        if list.def.source == "Related" {
            return Some(ValueListValues::Unsupported);
        }
        if let Some(values) = &list.values {
            return Some(ValueListValues::Values(values.clone()));
        }
        let Some((occurrence, field)) = list.field else {
            return Some(ValueListValues::Values(Vec::new()));
        };
        let mut values: Vec<String> = Vec::new();
        for r in self.records_of(occurrence) {
            if let Ok(v) = self.field_on(occurrence, r, field, 1, Cursor::default()) {
                let t = v.to_text(&self.cx()).into_string();
                if !t.is_empty() && !values.contains(&t) {
                    values.push(t);
                }
            }
        }
        values.sort_by(|a, b| crate::value::text_cmp(a, b));
        Some(ValueListValues::Values(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repetition keys of the largest field ids (#361): `u64::MAX / 256`
    /// has them up to `u64::MAX`; past it only the first repetition has a
    /// cell.
    #[test]
    fn repetition_keys_stop_at_the_largest() {
        let largest = u64::MAX / 256;
        assert_eq!(cell_key(largest, 1), Some(largest));
        assert_eq!(cell_key(largest, 255), Some(u64::MAX));
        assert_eq!(cell_key(largest + 1, 1), Some(largest + 1));
        assert_eq!(cell_key(largest + 1, 2), None);
    }
}
