//! The functions a formula editor offers: every function of the catalog
//! with its signature, category and description, the Get functions, and a
//! file's custom functions.
//!
//! Names, signatures, categories and descriptions come from the catalogs:
//! `calc-functions.csv` (DBEngine string resources 130-156),
//! `get-selectors.csv` (resource 131) and the category names of resource
//! 135 in `enumerations.csv`.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::Insertion;
use crate::host::CalcSchema;

const FUNCTIONS: &str = include_str!("../../../../docs/re/catalogs/calc-functions.csv");
const SELECTORS: &str = include_str!("../../../../docs/re/catalogs/get-selectors.csv");
const ENUMERATIONS: &str = include_str!("../../../../docs/re/catalogs/enumerations.csv");

/// Resource 135's index of the Get category and of the file's custom
/// functions.
pub const GET: usize = 18;
pub const CUSTOM: usize = 21;

/// One function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// What the functions pane shows and inserts: `Abs ( number )`.
    pub signature: String,
    /// The name it sorts and is searched by: `Abs`, `Get ( AccountName )`.
    pub name: String,
    /// `name` lowercased, as the list sorts and searches.
    pub key: String,
    /// Its category: an index of resource 135 (2 is Text).
    pub category: usize,
    pub description: String,
}

impl Entry {
    pub fn is_get(&self) -> bool {
        self.category == GET
    }

    /// What inserting it writes: the signature, with the first parameter
    /// selected (Get functions take none to fill in).
    pub fn insertion(&self) -> Insertion {
        Insertion::function(&self.signature, !self.is_get())
    }

    /// A Get function's selector: `AccountName` of `Get ( AccountName )`.
    pub fn selector(&self) -> Option<&str> {
        self.is_get().then(|| {
            self.name
                .trim_start_matches("Get ( ")
                .trim_end_matches(" )")
        })
    }
}

/// Split one CSV line (`"` quotes fields; `""` is a quote inside one).
fn columns(line: &str) -> Vec<String> {
    let (mut out, mut field, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    out.push(field);
    out
}

/// The rows of a catalog, by its header's column names.
fn rows(text: &str) -> Vec<HashMap<String, String>> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = columns(lines.next().unwrap_or_default());
    lines
        .map(|line| header.iter().cloned().zip(columns(line)).collect())
        .collect()
}

/// Resource 135: `Name`, `Type`, then the categories (`Text` is 2, `Get`
/// 18, `Custom` 21, `External` 22).
pub fn categories() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = Vec::new();
        for row in rows(ENUMERATIONS) {
            if row.get("resource_id").map(String::as_str) != Some("135") {
                continue;
            }
            let (Some(index), Some(value)) = (
                row.get("index").and_then(|i| i.parse::<usize>().ok()),
                row.get("value"),
            ) else {
                continue;
            };
            if names.len() <= index {
                names.resize(index + 1, String::new());
            }
            names[index].clone_from(value);
        }
        names
    })
}

/// FileMaker's functions and Get functions, alphabetically. The generic
/// `Get ( flag )` is listed as its selectors.
pub fn catalog() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let category = |row: &HashMap<String, String>| {
            row.get("category_id")
                .and_then(|c| c.parse::<usize>().ok())
                .and_then(|c| c.checked_sub(1))
        };
        let mut entries: Vec<Entry> = Vec::new();
        for row in rows(FUNCTIONS) {
            let (Some(name), Some(signature), Some(category)) =
                (row.get("name"), row.get("signature"), category(&row))
            else {
                continue;
            };
            if row.get("kind").map(String::as_str) != Some("function")
                || signature.is_empty()
                || category == GET
            {
                continue;
            }
            entries.push(Entry {
                signature: signature.clone(),
                name: name.clone(),
                key: name.to_lowercase(),
                category,
                description: row.get("description").cloned().unwrap_or_default(),
            });
        }
        for row in rows(SELECTORS) {
            let (Some(signature), Some(category)) = (row.get("signature"), category(&row)) else {
                continue;
            };
            if signature.is_empty() {
                continue;
            }
            entries.push(Entry {
                signature: signature.clone(),
                name: signature.clone(),
                key: signature.to_lowercase(),
                category,
                description: row.get("description").cloned().unwrap_or_default(),
            });
        }
        sort(&mut entries);
        entries
    })
}

fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| (&a.key, &a.signature).cmp(&(&b.key, &b.signature)));
}

/// The file's custom functions: `Name ( parameter ; ... )`, or the name
/// alone when it takes none.
pub fn custom_functions(schema: Option<&CalcSchema>) -> Vec<Entry> {
    let mut entries: Vec<Entry> = schema
        .into_iter()
        .flat_map(|s| s.custom_functions())
        .map(|f| Entry {
            signature: if f.parameters.is_empty() {
                f.name.clone()
            } else {
                format!("{} ( {} )", f.name, f.parameters.join(" ; "))
            },
            name: f.name.clone(),
            key: f.name.to_lowercase(),
            category: CUSTOM,
            description: String::new(),
        })
        .collect();
    sort(&mut entries);
    entries
}

/// Everything a file offers: FileMaker's functions and the file's custom
/// functions, alphabetically.
pub fn listed(schema: Option<&CalcSchema>) -> Vec<Entry> {
    let mut entries = catalog().to_vec();
    entries.extend(custom_functions(schema));
    sort(&mut entries);
    entries
}
