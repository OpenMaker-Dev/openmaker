//! Name resolution: formulas store fields, table occurrences, custom
//! functions and plug-in functions by id, and show them by their current
//! names (calc-encoding.md 4.2). Callers plug in a file's catalogs.

use std::collections::HashMap;

use crate::operand::PluginFunctionId;

/// A file's names as the tokenizer, compiler and printer need them. Every
/// lookup by name compares case-insensitively, as FileMaker does. The
/// defaults know no names.
pub trait Names {
    /// The name of table occurrence `occurrence`.
    fn occurrence_name(&self, _occurrence: u64) -> Option<&str> {
        None
    }
    fn find_occurrence(&self, _name: &str) -> Option<u64> {
        None
    }
    /// The name of `field` of the base table shown by `occurrence`.
    fn field_name(&self, _occurrence: u64, _field: u64) -> Option<&str> {
        None
    }
    fn find_field(&self, _occurrence: u64, _name: &str) -> Option<u64> {
        None
    }
    /// Whether `field` is a summary field, which `GetSummary` requires
    /// (error 1216 otherwise); `None` when unknown, which skips the check.
    fn is_summary_field(&self, _occurrence: u64, _field: u64) -> Option<bool> {
        None
    }
    /// A custom function (`16/5/<id>`); its argument counts are its
    /// parameter count.
    fn custom_function(&self, _id: u64) -> Option<Function<'_>> {
        None
    }
    fn find_custom_function(&self, _name: &str) -> Option<u64> {
        None
    }
    /// An installed plug-in's function. FileMaker's built-in Design and
    /// mobile functions come from the catalog and need no entry here.
    fn plugin_function(&self, _id: PluginFunctionId) -> Option<Function<'_>> {
        None
    }
    fn find_plugin_function(&self, _name: &str) -> Option<PluginFunctionId> {
        None
    }
}

/// A function the file defines or a plug-in provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Function<'a> {
    pub name: &'a str,
    pub min_args: u16,
    /// `None` when unlimited.
    pub max_args: Option<u16>,
}

/// No names: formulas that reference nothing a file defines.
pub struct NoNames;

impl Names for NoNames {}

fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// An in-memory [`Names`] filled from a file's catalogs. The first of two
/// entries with the same name wins a lookup by name.
#[derive(Clone, Debug, Default)]
pub struct NameTable {
    occurrences: HashMap<u64, (String, u64)>,
    occurrence_names: HashMap<String, u64>,
    fields: HashMap<(u64, u64), String>,
    /// Field kinds where known: true for summary fields.
    summary: HashMap<(u64, u64), bool>,
    field_names: HashMap<(u64, String), u64>,
    custom_functions: HashMap<u64, (String, u16)>,
    custom_function_names: HashMap<String, u64>,
    plugins: HashMap<PluginFunctionId, (String, u16, Option<u16>)>,
    plugin_names: HashMap<String, PluginFunctionId>,
}

impl NameTable {
    /// A table occurrence showing base table `table`.
    pub fn add_occurrence(&mut self, id: u64, name: &str, table: u64) {
        self.occurrences.insert(id, (name.to_owned(), table));
        self.occurrence_names.entry(fold(name)).or_insert(id);
    }

    /// Another file's occurrence `name` of base table `table`: fields
    /// resolve in it and it shows its name, but no name finds it.
    pub fn add_foreign_occurrence(&mut self, id: u64, name: &str, table: u64) {
        self.occurrences.insert(id, (name.to_owned(), table));
    }

    /// A field of base table `table`.
    pub fn add_field(&mut self, table: u64, id: u64, name: &str) {
        self.fields.insert((table, id), name.to_owned());
        self.field_names.entry((table, fold(name))).or_insert(id);
    }

    /// Record whether a field of base table `table` is a summary field.
    pub fn set_summary_field(&mut self, table: u64, id: u64, summary: bool) {
        self.summary.insert((table, id), summary);
    }

    pub fn add_custom_function(&mut self, id: u64, name: &str, parameters: u16) {
        self.custom_functions
            .insert(id, (name.to_owned(), parameters));
        self.custom_function_names.entry(fold(name)).or_insert(id);
    }

    pub fn add_plugin_function(
        &mut self,
        id: PluginFunctionId,
        name: &str,
        min_args: u16,
        max_args: Option<u16>,
    ) {
        self.plugins
            .insert(id, (name.to_owned(), min_args, max_args));
        self.plugin_names.entry(fold(name)).or_insert(id);
    }

    fn table(&self, occurrence: u64) -> Option<u64> {
        self.occurrences.get(&occurrence).map(|(_, t)| *t)
    }

    /// The custom and plug-in functions alone, without occurrences and
    /// fields: for compiling against a schema being edited.
    pub fn functions_only(&self) -> NameTable {
        NameTable {
            custom_functions: self.custom_functions.clone(),
            custom_function_names: self.custom_function_names.clone(),
            plugins: self.plugins.clone(),
            plugin_names: self.plugin_names.clone(),
            ..NameTable::default()
        }
    }
}

impl Names for NameTable {
    fn occurrence_name(&self, occurrence: u64) -> Option<&str> {
        self.occurrences.get(&occurrence).map(|(n, _)| n.as_str())
    }
    fn find_occurrence(&self, name: &str) -> Option<u64> {
        self.occurrence_names.get(&fold(name)).copied()
    }
    fn field_name(&self, occurrence: u64, field: u64) -> Option<&str> {
        let table = self.table(occurrence)?;
        self.fields.get(&(table, field)).map(String::as_str)
    }
    fn find_field(&self, occurrence: u64, name: &str) -> Option<u64> {
        let table = self.table(occurrence)?;
        self.field_names.get(&(table, fold(name))).copied()
    }
    fn is_summary_field(&self, occurrence: u64, field: u64) -> Option<bool> {
        let table = self.table(occurrence)?;
        self.summary.get(&(table, field)).copied()
    }
    fn custom_function(&self, id: u64) -> Option<Function<'_>> {
        self.custom_functions.get(&id).map(|(name, n)| Function {
            name,
            min_args: *n,
            max_args: Some(*n),
        })
    }
    fn find_custom_function(&self, name: &str) -> Option<u64> {
        self.custom_function_names.get(&fold(name)).copied()
    }
    fn plugin_function(&self, id: PluginFunctionId) -> Option<Function<'_>> {
        self.plugins.get(&id).map(|(name, min, max)| Function {
            name,
            min_args: *min,
            max_args: *max,
        })
    }
    fn find_plugin_function(&self, name: &str) -> Option<PluginFunctionId> {
        self.plugin_names.get(&fold(name)).copied()
    }
}
