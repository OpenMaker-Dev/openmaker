//! A file's names for script step text (`om_scripts::display`): fields and
//! table occurrences, layouts, scripts, base tables and value lists from its
//! calculation schema.

use crate::host::CalcSchema;
use om_formula::Names;
use om_scripts::display::{Lookup, StepNames};

impl StepNames for CalcSchema {
    fn calc_names(&self) -> &dyn Names {
        self.names()
    }

    fn decimal_comma(&self) -> bool {
        self.locale.decimal_separator == ','
    }

    fn layout_name(&self, id: u64) -> Option<&str> {
        self.layouts()
            .iter()
            .find(|l| u64::from(l.id) == id)
            .map(|l| l.name.as_str())
    }

    fn layout_occurrence_name(&self, id: u64) -> Option<&str> {
        let layout = self.layouts().iter().find(|l| u64::from(l.id) == id)?;
        self.occurrence(layout.occurrence?).map(|o| o.name.as_str())
    }

    fn script_name(&self, id: u64) -> Option<&str> {
        self.scripts()
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.as_str())
    }

    fn table_name(&self, id: u64) -> Option<&str> {
        self.table(id).map(|t| t.name.as_str())
    }

    // Layouts and scripts may share a name in FileMaker; the lookups count
    // every match so a shared name is refused rather than the first taken.

    fn find_layout(&self, name: &str, occurrence: Option<&str>) -> Lookup {
        let shows = |id: Option<u64>| match occurrence {
            None => true,
            Some(wanted) => id
                .and_then(|o| self.occurrence(o))
                .is_some_and(|o| o.name.to_lowercase() == wanted.to_lowercase()),
        };
        Lookup::by_name(
            self.layouts()
                .iter()
                .filter(|l| shows(l.occurrence))
                .map(|l| (u64::from(l.id), l.name.as_str())),
            name,
        )
    }

    fn find_script(&self, name: &str) -> Lookup {
        Lookup::by_name(
            self.scripts()
                .iter()
                .filter(|s| s.kind == om_scripts::ScriptKind::Script)
                .map(|s| (s.id, s.name.as_str())),
            name,
        )
    }

    fn find_table(&self, name: &str) -> Lookup {
        Lookup::by_name(self.tables().map(|t| (t.id, t.name.as_str())), name)
    }

    fn find_value_list(&self, name: &str) -> Lookup {
        Lookup::by_name(
            self.value_lists().iter().map(|l| (l.id, l.name.as_str())),
            name,
        )
    }

    fn find_occurrence(&self, name: &str) -> Lookup {
        match Lookup::by_name(self.occurrences().map(|o| (o.id, o.name.as_str())), name) {
            // External occurrences are only in the calculation names.
            Lookup::Missing => self.names().find_occurrence(name).into(),
            found => found,
        }
    }

    fn find_field(&self, occurrence: u64, name: &str) -> Lookup {
        match self.table_of(occurrence) {
            Some(table) => {
                Lookup::by_name(table.fields.values().map(|f| (f.id, f.name.as_str())), name)
            }
            None => self.names().find_field(occurrence, name).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FieldDef;
    use crate::meta::ScriptEntry;
    use om_calc::{Locale, ValueType};
    use om_scripts::ScriptKind;

    /// Two layouts named Detail of different occurrences, two named List
    /// of the same one, two scripts named Twin and a field whose name
    /// differs from another's only in case.
    fn shared() -> CalcSchema {
        let mut schema = CalcSchema::new("f", Locale::en_us());
        schema.add_table(0x8002, "People");
        schema.add_occurrence(0xd00002, "People", 0x8002);
        schema.add_occurrence(0xd00003, "Archive", 0x8002);
        schema.add_field(0x8002, FieldDef::data(1, "Name", ValueType::Text));
        schema.add_field(0x8002, FieldDef::data(2, "name", ValueType::Text));
        schema.add_field(0x8002, FieldDef::data(3, "City", ValueType::Text));
        schema.add_layout(3, "Detail", Some(0xd00002));
        schema.add_layout(4, "Detail", Some(0xd00003));
        schema.add_layout(5, "List", Some(0xd00002));
        schema.add_layout(6, "List", Some(0xd00002));
        let script = |id, name: &str| ScriptEntry {
            id,
            name: name.into(),
            kind: ScriptKind::Script,
        };
        schema.set_scripts(vec![
            script(1, "Twin"),
            script(2, "Twin"),
            script(3, "Once"),
        ]);
        schema
    }

    #[test]
    fn shared_names_are_ambiguous_unless_the_occurrence_tells() {
        let schema = shared();
        assert_eq!(schema.find_layout("Detail", None), Lookup::Ambiguous);
        assert_eq!(schema.find_layout("Detail", Some("People")), Lookup::One(3));
        assert_eq!(
            schema.find_layout("detail", Some("archive")),
            Lookup::One(4)
        );
        assert_eq!(
            schema.find_layout("List", Some("People")),
            Lookup::Ambiguous
        );
        assert_eq!(
            schema.find_layout("Detail", Some("Nowhere")),
            Lookup::Missing
        );
        assert_eq!(schema.find_script("Twin"), Lookup::Ambiguous);
        assert_eq!(schema.find_script("once"), Lookup::One(3));
        // An exact name wins over one equal ignoring case.
        assert_eq!(schema.find_field(0xd00002, "Name"), Lookup::One(1));
        assert_eq!(schema.find_field(0xd00002, "NAME"), Lookup::Ambiguous);
        assert_eq!(schema.find_field(0xd00003, "city"), Lookup::One(3));
        assert_eq!(schema.find_occurrence("archive"), Lookup::One(0xd00003));
        assert_eq!(schema.find_table("Nowhere"), Lookup::Missing);
    }
}
