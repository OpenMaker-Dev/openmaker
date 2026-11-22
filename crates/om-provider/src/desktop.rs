//! Client global-field values shared by desktop files and their related
//! readers. Connections remain independently owned. A relationship keeps
//! its file's values alive after that file's desktop window closes.
use crate::{calc::SessionGlobals, NativeProvider};
use om_data::host::CalcSchema;
use std::collections::{BTreeMap, BTreeSet};

// These are calculation file names with one extension already removed.
// Reapplying hosted-name suffix rules would merge Peer and Peer.fmp12.
fn file_key(name: &str) -> String {
    name.to_lowercase()
}

#[derive(Default)]
pub(crate) struct DesktopGlobals {
    generation: u64,
    values: BTreeMap<String, SessionGlobals>,
    holders: BTreeMap<String, BTreeSet<String>>,
}

impl DesktopGlobals {
    fn prune(&mut self) {
        let before = self.values.len();
        self.values
            .retain(|name, _| self.holders.values().any(|held| held.contains(name)));
        if self.values.len() != before {
            self.generation = self.generation.wrapping_add(1);
        }
    }
}

impl NativeProvider {
    pub(crate) fn register_desktop_file(&mut self, schema: Option<&CalcSchema>) {
        let name = file_key(&self.calc.file_name);
        let mut held = BTreeSet::from([name.clone()]);
        if let Some(schema) = schema {
            held.extend(
                schema
                    .imported_files()
                    .iter()
                    .map(|f| file_key(&f.file_name)),
            );
        }
        let mut desktop = self.desktop.lock().expect("desktop globals");
        desktop.holders.insert(name, held);
        desktop.prune();
    }

    pub(crate) fn close_desktop_file(&mut self) {
        if self.session.is_some() {
            let mut desktop = self.desktop.lock().expect("desktop globals");
            desktop.holders.remove(&file_key(&self.calc.file_name));
            desktop.prune();
        }
    }

    pub(crate) fn sync_desktop_globals(&mut self) -> bool {
        let generation = self.desktop.lock().expect("desktop globals").generation;
        if generation == self.desktop_generation {
            return false;
        }
        self.desktop_generation = generation;
        self.invalidate_calc_data();
        true
    }

    pub(crate) fn set_desktop_globals<'a>(
        &mut self,
        schema: &CalcSchema,
        table: u64,
        changes: impl IntoIterator<Item = ((u64, u32), &'a str)>,
    ) {
        let (file, table) = match schema.table_context(table) {
            Some(file) => (file_key(&file.file_name), file.file_id(table)),
            None => (file_key(&self.calc.file_name), table),
        };
        let mut desktop = self.desktop.lock().expect("desktop globals");
        desktop.values.entry(file).or_default().extend(
            changes
                .into_iter()
                .map(|((field, repetition), text)| ((table, field, repetition), text.to_owned())),
        );
        desktop.generation = desktop.generation.wrapping_add(1);
        self.desktop_generation = desktop.generation;
    }

    pub(crate) fn desktop_globals(&mut self, schema: &CalcSchema) -> SessionGlobals {
        self.sync_desktop_globals();
        let desktop = self.desktop.lock().expect("desktop globals");
        let mut values = self.calc.globals.clone();
        for table in schema.tables() {
            let (file, native) = match schema.table_context(table.id) {
                Some(file) => (file_key(&file.file_name), file.file_id(table.id)),
                None => (file_key(&self.calc.file_name), table.id),
            };
            if let Some(globals) = desktop.values.get(&file) {
                for ((_, field, repetition), text) in
                    globals.range((native, 0, 0)..=(native, u64::MAX, u32::MAX))
                {
                    values.insert((table.id, *field, *repetition), text.clone());
                }
            }
        }
        values
    }
}
