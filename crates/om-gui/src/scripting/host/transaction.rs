//! OnWindowTransaction (script-runtime.md 7.1, 13.26; file-metadata.md 3;
//! Claris "OnWindowTransaction"), as FileMaker Server's engine runs it
//! (battery s35-wintx): every commit that writes records fires it once,
//! Commit Records, leaving a record, Delete Record, Delete All Records,
//! Commit Transaction, a record and its related records together; a commit
//! with nothing changed, a revert and a failed commit do not. Its
//! parameter names the records, in FileMaker's formatted JSON:
//!
//! ```text
//! { "File" : { "Base table" : [ [ "New" | "Modified" | "Deleted", record id, field content ], ... ] } }
//! ```
//!
//! The file is its name without the extension; base tables are in key
//! order, each with its records in the order they were written. The field
//! content is the record's value of the trigger's field
//! (`mWndTransactionField`, else a field named `OnWindowTransaction`), read
//! after the commit (before a deletion); empty when the table has no such
//! field; a JSON object or array as itself, anything else as text. The
//! script does not run at the commit: it is queued below the running
//! script and runs when every script has ended, one per commit, in commit
//! order.

use std::collections::{BTreeMap, BTreeSet};

use om_data::calc::Value;
use om_data::DataError;

use super::WorkerHost;

/// The field the trigger reads when its field-name option is empty.
const DEFAULT_FIELD: &str = "OnWindowTransaction";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    New,
    Modified,
    Deleted,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Operation::New => "New",
            Operation::Modified => "Modified",
            Operation::Deleted => "Deleted",
        }
    }
}

/// One committed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The base table.
    pub table: u64,
    pub record: i64,
    pub operation: Operation,
    /// The trigger field's value.
    pub content: String,
}

/// Records staged by the open script transaction. Related writes can
/// succeed before the rest of a record commit fails, so track their
/// dependencies at the write, independently of the trigger entries.
#[derive(Default)]
pub(crate) struct Staged {
    pub(super) entries: Vec<Entry>,
    parents: BTreeMap<(u64, i64), (u64, i64)>,
    deleted: BTreeSet<(u64, i64)>,
}

impl Staged {
    pub(crate) fn created_related(&mut self, table: u64, record: i64, parent: (u64, u64)) {
        if let Ok(id) = i64::try_from(parent.1) {
            self.parents.insert((table, record), (parent.0, id));
        }
    }

    pub(crate) fn holds_related(&self, table: u64, record: i64) -> bool {
        self.parents.contains_key(&(table, record))
    }

    pub(crate) fn deleted(&mut self, table: u64, record: i64) {
        self.parents.remove(&(table, record));
        self.deleted.insert((table, record));
    }

    pub(super) fn validate_related_parents(&self) -> Result<(), DataError> {
        if self
            .parents
            .values()
            .any(|parent| self.deleted.contains(parent))
        {
            return Err(missing_related_parent());
        }
        Ok(())
    }
}

pub(crate) fn missing_related_parent() -> DataError {
    DataError::with_code(
        415,
        "A required parent of a new related record is pending deletion.",
    )
}

/// The trigger's parameter for `entries` of file `file` (its name without
/// the extension).
pub(crate) fn parameter(file: &str, entries: &[(String, Operation, i64, String)]) -> String {
    let mut tables: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    for (table, operation, record, content) in entries {
        tables
            .entry(table.clone())
            .or_default()
            .push(Json::Array(vec![
                Json::String(operation.name().to_owned()),
                Json::Number(record.to_string()),
                content_json(content),
            ]));
    }
    let tables = tables
        .into_iter()
        .map(|(table, rows)| (table, Json::Array(rows)))
        .collect();
    let root = Json::Object(BTreeMap::from([(file.to_owned(), Json::Object(tables))]));
    let mut out = String::new();
    write(&root, 0, &mut out);
    out
}

/// A JSON value of the parameter.
#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    /// As written.
    Number(String),
    String(String),
    Array(Vec<Json>),
    /// Keys in order, as FileMaker's JSON keeps them.
    Object(BTreeMap<String, Json>),
}

/// FileMaker's layout of the parameter (s35 C2): every object and array on
/// lines of their own, members and elements indented by tabs, returns
/// between lines.
fn write(value: &Json, depth: usize, out: &mut String) {
    let line = |depth: usize, out: &mut String| {
        out.push('\r');
        out.extend(std::iter::repeat_n('\t', depth));
    };
    match value {
        Json::Object(members) => {
            out.push('{');
            for (i, (key, member)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                line(depth + 1, out);
                out.push_str(&quoted(key));
                out.push_str(" : ");
                if matches!(member, Json::Object(_) | Json::Array(_)) {
                    line(depth + 1, out);
                }
                write(member, depth + 1, out);
            }
            line(depth, out);
            out.push('}');
        }
        Json::Array(elements) => {
            out.push('[');
            for (i, element) in elements.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                line(depth + 1, out);
                write(element, depth + 1, out);
            }
            line(depth, out);
            out.push(']');
        }
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(n),
        Json::String(text) => out.push_str(&quoted(text)),
    }
}

/// A field's content in the parameter: a JSON object or array as itself,
/// anything else (a number too) as text.
fn content_json(content: &str) -> Json {
    let trimmed = content.trim();
    if trimmed.starts_with(['{', '[']) {
        let mut reader = Reader {
            text: trimmed.as_bytes(),
            at: 0,
        };
        if let Some(value) = reader.value(0) {
            reader.space();
            if reader.at == trimmed.len() {
                return value;
            }
        }
    }
    Json::String(content.to_owned())
}

/// A strict JSON reader (RFC 8259), deep enough for field contents.
struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn space(&mut self) {
        while self
            .text
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.space();
        let found = self.text.get(self.at) == Some(&byte);
        if found {
            self.at += 1;
        }
        found
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > 256 {
            return None;
        }
        self.space();
        match *self.text.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut members = BTreeMap::new();
                if self.eat(b'}') {
                    return Some(Json::Object(members));
                }
                loop {
                    self.space();
                    let Json::String(key) = self.string()? else {
                        return None;
                    };
                    if !self.eat(b':') {
                        return None;
                    }
                    let value = self.value(depth + 1)?;
                    members.insert(key, value);
                    if self.eat(b'}') {
                        return Some(Json::Object(members));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut elements = Vec::new();
                if self.eat(b']') {
                    return Some(Json::Array(elements));
                }
                loop {
                    elements.push(self.value(depth + 1)?);
                    if self.eat(b']') {
                        return Some(Json::Array(elements));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'"' => self.string(),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'n' => self.word("null", Json::Null),
            _ => self.number(),
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Option<Json> {
        let end = self.at + word.len();
        (self.text.get(self.at..end)? == word.as_bytes()).then(|| {
            self.at = end;
            value
        })
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        while self
            .text
            .get(self.at)
            .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'))
        {
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).ok()?;
        let valid = text.parse::<f64>().is_ok()
            && !text.starts_with('+')
            && !text.starts_with('.')
            && !text.ends_with('.');
        valid.then(|| Json::Number(text.to_owned()))
    }

    fn string(&mut self) -> Option<Json> {
        if self.text.get(self.at) != Some(&b'"') {
            return None;
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            let rest = std::str::from_utf8(self.text.get(self.at..)?).ok()?;
            let c = rest.chars().next()?;
            self.at += c.len_utf8();
            match c {
                '"' => return Some(Json::String(out)),
                '\\' => {
                    let escape = *self.text.get(self.at)?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex =
                                std::str::from_utf8(self.text.get(self.at..self.at + 4)?).ok()?;
                            self.at += 4;
                            out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
                        }
                        _ => return None,
                    }
                }
                c if (c as u32) < 0x20 => return None,
                c => out.push(c),
            }
        }
    }
}

/// A JSON string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A transaction's records, once each, in the order first written: a
/// record created in it is New however often it changed, and gone when it
/// was deleted again; another record deleted is Deleted.
pub(crate) fn merged(entries: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for entry in entries {
        match out
            .iter()
            .position(|e| e.table == entry.table && e.record == entry.record)
        {
            None => out.push(entry),
            Some(i) => match (out[i].operation, entry.operation) {
                (Operation::New, Operation::Deleted) => {
                    out.remove(i);
                }
                (Operation::New, _) => {}
                (_, operation) => {
                    out[i].operation = operation;
                    out[i].content = entry.content;
                }
            },
        }
    }
    out
}

impl WorkerHost<'_> {
    /// The trigger field's value of `record` of base table `table`, as a
    /// calculation reads it.
    pub(super) fn transaction_content(&mut self, table: u64, record: u64) -> String {
        let Some(schema) = self.schema().cloned() else {
            return String::new();
        };
        let name = schema
            .file_options
            .as_ref()
            .map(|o| o.window_transaction_field.trim())
            .filter(|n| !n.is_empty())
            .unwrap_or(DEFAULT_FIELD)
            .to_owned();
        let Some(field) = schema
            .table(table)
            .and_then(|t| t.fields.values().find(|f| f.name == name))
            .map(|f| f.id)
        else {
            return String::new();
        };
        let Some(occurrence) = schema
            .occurrences()
            .find(|o| o.table == table)
            .map(|o| o.id)
        else {
            return String::new();
        };
        let value = self
            .file(|f| f.field_value(occurrence, record, field, 1))
            .and_then(Result::ok);
        value.map(|v| self.text(&v)).unwrap_or_default()
    }

    /// The `operation` records of `entries` are gone: records this client
    /// deleted, or created in a transaction that was rolled back. Each of
    /// the file's windows that showed one shows the record that takes its
    /// place (`Window::forget_deleted`).
    pub(super) fn release_records(&mut self, entries: &[Entry], operation: Operation) {
        let gone: Vec<(u64, i64)> = entries
            .iter()
            .filter(|e| e.operation == operation)
            .map(|e| (e.table, e.record))
            .collect();
        if gone.is_empty() {
            return;
        }
        // A waiting window's table follows the layout the UI left it on,
        // which may be another table's since it was last current.
        for i in 0..self.s.file.others.len() {
            let layout = self.s.file.others[i].state.layout;
            self.s.file.others[i].table = layout.and_then(|id| self.layout_table(id));
        }
        let file = &mut self.s.file;
        for (table, record) in gone {
            for window in std::iter::once(&mut file.window).chain(file.others.iter_mut()) {
                window.forget_deleted(table, record);
            }
        }
    }

    /// Records were committed in the window: OnWindowTransaction's script
    /// runs after the step, with their JSON. Inside a transaction they wait
    /// for Commit Transaction.
    pub(super) fn committed_records(&mut self, entries: Vec<Entry>) {
        self.release_records(&entries, Operation::Deleted);
        if let Some(held) = self.s.file.transaction.as_mut() {
            for entry in &entries {
                if entry.operation == Operation::Deleted {
                    held.deleted(entry.table, entry.record);
                }
            }
            held.entries.extend(entries);
            return;
        }
        // Once each, as Commit Transaction lists them: a new record a
        // self-relationship's related write also changed is New, not New
        // and Modified (#931).
        let entries = merged(entries);
        if entries.is_empty() {
            return;
        }
        let Some(schema) = self.schema().cloned() else {
            return;
        };
        let Some(script) = schema
            .file_options
            .as_ref()
            .and_then(|o| o.window_transaction)
        else {
            return;
        };
        let named: Vec<(String, Operation, i64, String)> = entries
            .into_iter()
            .map(|e| {
                let table = schema
                    .table(e.table)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                (table, e.operation, e.record, e.content)
            })
            .collect();
        let file = self.session_info().file_name;
        let parameter = parameter(&file, &named);
        let mode = self.s.file.window.state.mode;
        let window = u64::from(self.s.file.window.id());
        // Queued below the running script (s35 C3), for the window whose
        // records were committed.
        self.s
            .file
            .window_transactions
            .push_back(om_data::runtime::Trigger {
                window: Some(window),
                mode,
                from_step: true,
                preset_parameter: Some(Value::text(parameter)),
                ..om_data::runtime::Trigger::new(
                    om_data::runtime::TriggerEvent::WindowTransaction,
                    self.s.file.id,
                    script,
                )
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parameter_is_filemakers_formatted_json() {
        // s35 C2: the first firing of a probe, exactly.
        assert_eq!(
            parameter(
                "work",
                &[("Tasks".into(), Operation::New, 1, String::new())]
            ),
            "{\r\t\"work\" : \r\t{\r\t\t\"Tasks\" : \r\t\t[\r\t\t\t[\r\t\t\t\t\"New\",\
             \r\t\t\t\t1,\r\t\t\t\t\"\"\r\t\t\t]\r\t\t]\r\t}\r}"
        );
    }

    #[test]
    fn tables_go_in_key_order_and_json_content_stays_json() {
        let entries = vec![
            (
                "Tasks".to_owned(),
                Operation::Modified,
                6,
                "a \"b\"".to_owned(),
            ),
            ("Tasks".to_owned(), Operation::New, 7, "42".to_owned()),
            (
                "Attachments".to_owned(),
                Operation::New,
                3,
                "{\"b\":2}".to_owned(),
            ),
        ];
        let text = parameter("work", &entries);
        let attachments = text.find("Attachments").unwrap();
        assert!(attachments < text.find("Tasks").unwrap());
        assert!(text.contains("\"b\" : 2"));
        assert!(text.contains("\"a \\\"b\\\"\""));
        assert!(text.contains("\"42\""));
        assert_eq!(content_json("[1,2"), Json::String("[1,2".into()));
        assert_eq!(
            content_json("{\"a\":1,\"c\":[true,null]}"),
            Json::Object(BTreeMap::from([
                ("a".into(), Json::Number("1".into())),
                ("c".into(), Json::Array(vec![Json::Bool(true), Json::Null])),
            ]))
        );
    }
}
