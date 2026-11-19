//! Corpus check shared by `tests/corpus.rs` and `examples/check_corpus.rs`:
//! every stored formula of a set of `.fmp12` files is decoded, re-encoded,
//! printed, tokenized and compiled again (calc-encoding.md 11).
//!
//! Files are read with `om-hbam` the way its `dump_calcs` example walks them:
//! a formula is a node whose children are only `4`..`6`, with a `6`, at a
//! path of a calc-encoding.md section 8 owner.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use om_formula::{
    compile, compile_tokens, formatted, polish, print, Context, Expr, Formula, NameTable,
    PluginFunctionId, Token, FIELD_MISSING, FUNCTION_MISSING, TABLE_MISSING,
};
use om_hbam::{HbamFile, KeySegment, Node};

/// One stored formula.
pub struct Stored {
    pub location: String,
    pub context: Option<Vec<u8>>,
    pub formatted: Option<Vec<u8>>,
    pub polish: Vec<u8>,
    /// Parameters when the formula is a custom function body.
    pub parameters: Option<Vec<String>>,
}

/// The owner of a calculation from its path (calc-encoding.md 8), `None`
/// for record data, catalog indexes and the dependency index.
fn owner(path: &[KeySegment<'_>]) -> Option<&'static str> {
    let ids: Vec<Option<u64>> = path.iter().map(|s| s.as_uint()).collect();
    let id = |i: usize| ids.get(i).copied().flatten();
    Some(match id(0)? {
        r if r >= 0x8000 => {
            if (id(1), id(2)) != (Some(3), Some(5)) || ids.len() != 5 {
                return None;
            }
            "field"
        }
        4 if (id(1), id(3), id(4), id(6)) == (Some(5), Some(48), Some(5), Some(49)) => {
            "layout-trigger"
        }
        23 if (id(1), id(2), id(4)) == (Some(2), Some(5), Some(5)) => "record-access",
        _ if id(1) == Some(1) => return None,
        3 if (id(1), id(2), id(3)) == (Some(17), Some(1), Some(14)) => return None,
        4 if id(1) == Some(5) && id(3) == Some(5) && ids.len() >= 6 => "layout-object",
        4 if id(1) == Some(5) && (id(3), id(4)) == (Some(13), Some(5)) && ids.len() >= 7 => {
            "layout-object"
        }
        16 if ids.len() == 4 && id(3) == Some(35) => "custom-function",
        17 if ids.len() >= 7
            && (id(1), id(3), ids[ids.len() - 1]) == (Some(5), Some(5), Some(5)) =>
        {
            "script-step"
        }
        65 if ids.len() >= 4 && id(1) == Some(5) => "custom-menu",
        _ => "other",
    })
}

/// A file's names and formulas.
pub struct CorpusFile {
    pub name: String,
    pub names: NameTable,
    pub formulas: Vec<Stored>,
    /// Catalog entry names as stored (`.../5/<id>/16`, masked SCSU).
    pub catalog_names: Vec<Vec<u8>>,
}

pub fn load(path: &Path) -> Result<CorpusFile, Box<dyn std::error::Error>> {
    let file = HbamFile::open(path)?;
    let tree = file.key_tree()?;
    let catalog = om_hbam::Catalog::from_tree(&tree)?;
    let mut names = NameTable::default();
    for to in &catalog.table_occurrences {
        names.add_occurrence(to.id, &to.name, to.base_table().unwrap_or(0));
    }
    for table in &catalog.tables {
        for field in &table.fields {
            names.add_field(table.id, field.id, &field.name);
            let summary = field.kind == om_hbam::catalog::FieldKind::Summary;
            names.set_summary_field(table.id, field.id, summary);
        }
    }
    let mut parameters = HashMap::new();
    if let Some(functions) = tree.at(&[16, 5]) {
        for f in &functions.children {
            let text = |key| {
                f.get(key)
                    .and_then(|n| n.text())
                    .and_then(Result::ok)
                    .unwrap_or_default()
            };
            let params: Vec<String> = text(33)
                .split(';')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect();
            let id = f.id().unwrap_or_default();
            names.add_custom_function(id, &text(16), params.len() as u16);
            parameters.insert(id, params);
        }
    }
    let mut formulas = Vec::new();
    let mut catalog_names = Vec::new();
    tree.root.walk(&mut |path, node: &Node<'_>| {
        // Entry names: `<catalog>/5/<id>/16` and fields `<table>/3/5/<field>/16`.
        let ids: Vec<Option<u64>> = path.iter().map(|s| s.as_uint()).collect();
        let entry = match ids.as_slice() {
            [Some(c), Some(5), Some(_), Some(16)] => *c < 0x8000,
            [Some(t), Some(3), Some(5), Some(_), Some(16)] => *t >= 0x8000,
            _ => false,
        };
        if entry {
            if let Some(value) = node.assembled() {
                catalog_names.push(value.to_vec());
            }
        }
        let only_calc = !node.children.is_empty()
            && node.value.is_none()
            && node.children.iter().all(|c| matches!(c.id(), Some(4..=6)));
        if !only_calc {
            return;
        }
        let child = |id| node.get(id).and_then(|n| n.assembled()).map(|b| b.to_vec());
        let Some(polish) = child(6) else { return };
        let Some(owner) = owner(path) else { return };
        let formatted = child(5);
        if formatted.is_none() && matches!(owner, "layout-object" | "other") {
            return;
        }
        let location = path
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("/");
        let parameters = (owner == "custom-function")
            .then(|| parameters.get(&path[2].as_uint().unwrap_or(0)).cloned())
            .flatten();
        formulas.push(Stored {
            location,
            context: child(4),
            formatted,
            polish,
            parameters,
        });
    });
    // Third-party plug-in functions are named by the installed plug-in;
    // give each one seen a stand-in name so the text round-trips.
    let mut plugins = BTreeMap::new();
    for f in &formulas {
        if let Some(bytes) = &f.formatted {
            if let Ok(tokens) = formatted::decode(bytes) {
                for t in tokens {
                    if let Token::PluginFunction(id) = t {
                        if om_formula::catalog::builtin(id).is_none() {
                            plugins.insert((id.plugin, id.function), id);
                        }
                    }
                }
            }
        }
    }
    for id in plugins.values() {
        names.add_plugin_function(*id, &plugin_name(*id), 0, None);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(CorpusFile {
        name,
        names,
        formulas,
        catalog_names,
    })
}

fn plugin_name(id: PluginFunctionId) -> String {
    let plugin: String = id.plugin.iter().map(|&b| b as char).collect();
    format!("Plugin_{plugin}_{}", id.function)
}

#[derive(Default)]
pub struct Report {
    pub formulas: usize,
    pub polish_decoded: usize,
    pub polish_reencoded: usize,
    pub formatted_present: usize,
    pub formatted_decoded: usize,
    pub formatted_reencoded: usize,
    /// The stored tokens compile to the stored Polish stream.
    pub tokens_compiled: usize,
    /// The printed text tokenizes to the stored formatted stream.
    pub text_tokenized: usize,
    /// The printed text compiles to the stored Polish stream.
    pub text_compiled: usize,
    /// Formulas naming a field, table occurrence or function the file no
    /// longer has: printed as `<Field Missing>` and the like, which
    /// FileMaker cannot compile either.
    pub missing_names: usize,
    /// Distinct text values of the formulas (literals, names, whitespace,
    /// comments), all re-encoded byte for byte with the streams.
    pub strings: HashSet<String>,
    /// Catalog names that re-encode to FileMaker's bytes, of those that
    /// decode.
    pub catalog_names: usize,
    pub catalog_names_exact: usize,
    pub failures: Vec<String>,
}

impl Report {
    fn fail(&mut self, stage: &str, file: &str, f: &Stored, detail: String) {
        if self.failures.len() < 200 {
            self.failures
                .push(format!("{stage}: {file} {}: {detail}", f.location));
        }
    }

    pub fn complete(&self) -> bool {
        self.catalog_names_exact == self.catalog_names
            && self.polish_reencoded == self.formulas
            && self.formatted_reencoded == self.formatted_present
            && self.tokens_compiled == self.formatted_present
            && self.text_tokenized + self.missing_names == self.formatted_present
            && self.text_compiled + self.missing_names == self.formatted_present
    }

    pub fn summary(&self) -> String {
        format!(
            "formulas: {}\n\
             polish decoded: {}, re-encoded byte for byte: {}\n\
             formatted present: {}, decoded: {}, re-encoded byte for byte: {}\n\
             stored tokens compiled to the stored polish: {}\n\
             printed text tokenized to the stored formatted stream: {}\n\
             printed text compiled to the stored polish: {}\n\
             not printable as compilable text (missing names): {}\n\
             distinct formula strings: {} ({} above U+00FF)\n\
             catalog names re-encoded by om-scsu byte for byte: {} of {}",
            self.formulas,
            self.polish_decoded,
            self.polish_reencoded,
            self.formatted_present,
            self.formatted_decoded,
            self.formatted_reencoded,
            self.tokens_compiled,
            self.text_tokenized,
            self.text_compiled,
            self.missing_names,
            self.strings.len(),
            self.strings
                .iter()
                .filter(|s| s.chars().any(|c| c > '\u{FF}'))
                .count(),
            self.catalog_names_exact,
            self.catalog_names,
        )
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every text value in a token list or tree.
fn collect_strings(tokens: &[Token], expr: &Expr, out: &mut HashSet<String>) {
    for t in tokens {
        match t {
            Token::Whitespace(x)
            | Token::BlockComment(x)
            | Token::LineComment(x)
            | Token::Text(x) => {
                out.insert(x.value.clone());
            }
            Token::Symbol(s) => {
                out.insert(s.name.value.clone());
            }
            Token::Variable(v) => {
                out.insert(v.clone());
            }
            _ => {}
        }
    }
    let mut stack = vec![expr];
    while let Some(e) = stack.pop() {
        match e {
            Expr::Text(t) => {
                out.insert(t.value.clone());
            }
            Expr::Variable(v) => {
                out.insert(v.clone());
            }
            Expr::List(v)
            | Expr::Call { args: v, .. }
            | Expr::Evaluate { args: v, .. }
            | Expr::CustomCall { args: v, .. }
            | Expr::PluginCall { args: v, .. } => stack.extend(v),
            Expr::Case(p) => stack.extend(p.iter().flat_map(|(a, b)| [a, b])),
            Expr::Choose { test, results } => {
                stack.push(test);
                stack.extend(results);
            }
            Expr::Let { bindings, body } => {
                stack.extend(bindings);
                stack.push(body);
            }
            Expr::While {
                initial,
                condition,
                logic,
                result,
            } => {
                stack.extend(initial);
                stack.extend(logic);
                stack.push(condition);
                stack.push(result);
            }
            Expr::GetField { arg, .. } => stack.push(arg),
            _ => {}
        }
    }
}

pub fn check(file: &CorpusFile, report: &mut Report) {
    for raw in &file.catalog_names {
        if let Ok(text) = om_scsu::decode_scsu(raw) {
            report.catalog_names += 1;
            if om_scsu::encode_scsu(&text) == *raw {
                report.catalog_names_exact += 1;
            } else if report.failures.len() < 200 {
                report.failures.push(format!(
                    "catalog name: {} {text:?}: {}",
                    file.name,
                    hex(raw)
                ));
            }
        }
    }
    for f in &file.formulas {
        report.formulas += 1;
        let polish_tree = match polish::decode(&f.polish) {
            Ok(e) => e,
            Err(e) => {
                report.fail("polish decode", &file.name, f, e.to_string());
                continue;
            }
        };
        report.polish_decoded += 1;
        if polish::encode(&polish_tree) == f.polish {
            report.polish_reencoded += 1;
        } else {
            report.fail("polish re-encode", &file.name, f, hex(&f.polish));
        }
        let Some(bytes) = &f.formatted else { continue };
        report.formatted_present += 1;
        let formula = match Formula::decode(f.context.as_deref(), Some(bytes), &f.polish) {
            Ok(formula) => formula,
            Err(e) => {
                report.fail("formatted decode", &file.name, f, e.to_string());
                continue;
            }
        };
        report.formatted_decoded += 1;
        let tokens = formula.formatted.as_deref().unwrap_or_default();
        collect_strings(tokens, &formula.polish, &mut report.strings);
        if formatted::encode(tokens) == *bytes {
            report.formatted_reencoded += 1;
        } else {
            report.fail("formatted re-encode", &file.name, f, hex(bytes));
        }
        let parameters = f.parameters.clone().unwrap_or_default();
        // Stored formulas were compiled with `Self` allowed where FileMaker
        // allows it (field calculations, conditional formatting).
        let context = Context {
            occurrence: formula.occurrence(),
            parameters: &parameters,
            support_self: true,
            ..Context::new(&file.names)
        };
        match compile_tokens(tokens, &context) {
            Ok(expr) if polish::encode(&expr) == f.polish => report.tokens_compiled += 1,
            Ok(expr) => report.fail(
                "compile tokens",
                &file.name,
                f,
                format!(
                    "got {} want {}",
                    hex(&polish::encode(&expr)),
                    hex(&f.polish)
                ),
            ),
            Err(e) => report.fail("compile tokens", &file.name, f, e.to_string()),
        }
        let text = print(tokens, &context);
        if [FIELD_MISSING, TABLE_MISSING, FUNCTION_MISSING]
            .iter()
            .any(|m| text.contains(m))
        {
            report.missing_names += 1;
            continue;
        }
        match compile(&text, &context) {
            Ok(compiled) => {
                if formatted::encode(&compiled.tokens) == *bytes {
                    report.text_tokenized += 1;
                } else {
                    report.fail(
                        "tokenize text",
                        &file.name,
                        f,
                        format!(
                            "{text:?}\n  got  {}\n  want {}",
                            hex(&formatted::encode(&compiled.tokens)),
                            hex(bytes)
                        ),
                    );
                }
                if polish::encode(&compiled.expr) == f.polish {
                    report.text_compiled += 1;
                } else {
                    report.fail("compile text", &file.name, f, format!("{text:?}"));
                }
            }
            Err(e) => report.fail("compile text", &file.name, f, format!("{e}: {text:?}")),
        }
    }
}

/// `text` escaped as `tools/fms-calc-harness` prints display text: `\\`,
/// `\r`, `\n`, `\t`, and `\uXXXX` for other controls and U+E000..U+F8FF.
fn escaped(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || ('\u{E000}'..'\u{F900}').contains(&c) => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

/// One row per formula with a formatted stream: file, path, printed text.
pub fn print_all(file: &CorpusFile, out: &mut impl std::io::Write) -> std::io::Result<()> {
    for f in &file.formulas {
        let Some(Ok(tokens)) = f.formatted.as_deref().map(formatted::decode) else {
            continue;
        };
        let text = print(&tokens, &Context::new(&file.names));
        writeln!(out, "{}\t{}\t{}", file.name, f.location, escaped(&text))?;
    }
    Ok(())
}

/// The input escapes of `tools/fms-calc-harness`: `\n`, `\r`, `\t`, `\\`,
/// `\uXXXX`; any other backslash stays.
fn unescape(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' || chars.peek().is_none() {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let code: String = chars.clone().take(4).collect();
                match u32::from_str_radix(&code, 16).ok().and_then(char::from_u32) {
                    Some(u) if code.len() == 4 => {
                        out.push(u);
                        chars.nth(3);
                    }
                    _ => out.push_str("\\u"),
                }
            }
            Some(n) => {
                out.push('\\');
                out.push(n);
            }
            None => {}
        }
    }
    out
}

fn context_key(hex_key: &str) -> Option<u64> {
    let bytes: Vec<u8> = (0..hex_key.len() / 2)
        .filter_map(|i| u8::from_str_radix(&hex_key[2 * i..2 * i + 2], 16).ok())
        .collect();
    om_formula::Key(bytes).id().filter(|&id| id != 0)
}

/// Compile each input line as `tools/fms-calc-harness -c` does and print
/// the same columns: line, error, UTF-16 span, context, formatted, Polish.
/// `@<context> ` and `%16 ` (`Self` allowed) prefixes are honoured.
pub fn compile_lines(
    file: &CorpusFile,
    context: &str,
    input: impl std::io::BufRead,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.is_empty() || line.starts_with('#') {
            writeln!(out, "{line}")?;
            continue;
        }
        let (mut key, mut options, mut body) = (context.to_owned(), 0, line.as_str());
        loop {
            match body.split_once(' ') {
                Some((k, rest)) if k.len() > 1 && k.starts_with('@') => {
                    key = k[1..].to_owned();
                    body = rest;
                }
                Some((o, rest)) if o.len() > 1 && o.starts_with('%') => {
                    options = o[1..].parse().unwrap_or(0);
                    body = rest;
                }
                _ => break,
            }
        }
        let ctx = Context {
            occurrence: context_key(&key),
            support_self: options & 16 != 0,
            ..Context::new(&file.names)
        };
        match compile(&unescape(body), &ctx) {
            Ok(c) => writeln!(
                out,
                "{line}\t0\t0:0\t{key}\t{}\t{}",
                hex(&formatted::encode(&c.tokens)),
                hex(&polish::encode(&c.expr))
            )?,
            Err(e) => writeln!(
                out,
                "{line}\t{}\t{}:{}\t{key}\t\t",
                e.code.number(),
                e.span.start,
                e.span.end
            )?,
        }
    }
    Ok(())
}
