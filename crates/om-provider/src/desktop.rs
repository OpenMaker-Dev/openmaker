//! Client global-field values shared by desktop files and their related
//! readers. Connections remain independently owned. A relationship keeps
//! its file's values alive after that file's desktop window closes.
//!
//! Values are kept by file as external.rs tells files apart: by host, port
//! and name, or by path on this computer, so one name on two hosts is two
//! files (#43). A file whose identity is unknown is kept by its name.
use crate::{calc::SessionGlobals, NativeProvider};
use om_data::file_refs::FileIdentity;
use om_data::host::{CalcSchema, FileSchemaContext};
use std::collections::{BTreeMap, BTreeSet};

/// Which file a session value is of.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DesktopFile {
    Identity(FileIdentity),
    // A calculation file name with one extension already removed.
    // Reapplying hosted-name suffix rules would merge Peer and Peer.fmp12.
    Name(String),
}

impl DesktopFile {
    fn of(identity: Option<&FileIdentity>, name: &str) -> Self {
        identity.map_or_else(
            || Self::Name(name.to_lowercase()),
            |identity| Self::Identity(identity.clone()),
        )
    }

    /// A related file imported into a calculation schema.
    fn imported(file: &FileSchemaContext) -> Self {
        Self::of(file.identity.as_ref(), &file.file_name)
    }
}

#[derive(Default)]
pub(crate) struct DesktopGlobals {
    generation: u64,
    values: BTreeMap<DesktopFile, SessionGlobals>,
    holders: BTreeMap<DesktopFile, BTreeSet<DesktopFile>>,
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
    /// This provider's own file.
    fn desktop_file(&self) -> DesktopFile {
        DesktopFile::of(self.external.root_identity(), &self.calc.file_name)
    }

    /// The file whose values `table` of `schema` holds, and its id there.
    fn desktop_table(&self, schema: &CalcSchema, table: u64) -> (DesktopFile, u64) {
        match schema.table_context(table) {
            Some(file) => (DesktopFile::imported(file), file.file_id(table)),
            None => (self.desktop_file(), table),
        }
    }

    pub(crate) fn register_desktop_file(&mut self, schema: Option<&CalcSchema>) {
        let file = self.desktop_file();
        let mut held = BTreeSet::from([file.clone()]);
        if let Some(schema) = schema {
            held.extend(schema.imported_files().iter().map(DesktopFile::imported));
        }
        let mut desktop = self.desktop.lock().expect("desktop globals");
        desktop.holders.insert(file, held);
        desktop.prune();
    }

    pub(crate) fn close_desktop_file(&mut self) {
        if self.session.is_some() {
            let file = self.desktop_file();
            let mut desktop = self.desktop.lock().expect("desktop globals");
            desktop.holders.remove(&file);
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
        let (file, table) = self.desktop_table(schema, table);
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
            let (file, native) = self.desktop_table(schema, table.id);
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
