//! An in-memory file for the oracle tests: the probe database of
//! `tools/fms-transfer-oracle` (the Contacts table with the probe fields,
//! and Addresses related by Contacts::PrimaryKey = Addresses::ForeignKey),
//! holding the records the batteries' Setup scripts make, as FileMaker
//! stores them.
#![allow(dead_code)]

use std::collections::BTreeMap;

use om_calc::{Locale, Number};
use om_transfer::export::{ExportContext, ExportRecord, ExportSource, FieldInfo};
use om_transfer::import::{FieldWrite, ImportTarget, TargetField};
use om_transfer::text::ExportValue;
use om_transfer::{FieldKind, FieldType, RecordId, Result, TransferError};

pub const CONTACTS: u64 = 0xd00002;
pub const ADDRESSES: u64 = 0xd00003;

/// Contacts fields (ids as the probe schema numbers them).
pub mod f {
    pub const FIRST: u64 = 1;
    pub const LAST: u64 = 2;
    pub const PHOTO: u64 = 3;
    pub const TITLE: u64 = 4;
    pub const COMPANY: u64 = 5;
    pub const JOB: u64 = 6;
    pub const WEBSITE: u64 = 7;
    pub const PRIMARY_KEY: u64 = 9;
    pub const CREATED_BY: u64 = 10;
    pub const N: u64 = 14;
    pub const D: u64 = 15;
    pub const T: u64 = 16;
    pub const TS: u64 = 17;
    pub const R: u64 = 18;
    pub const NR: u64 = 19;
    pub const CN: u64 = 20;
    pub const CU: u64 = 21;
    pub const S: u64 = 22;
    pub const SC: u64 = 23;
    pub const G: u64 = 24;
    pub const AE: u64 = 25;
    pub const AEO: u64 = 26;
    pub const VNE: u64 = 27;
    pub const VU: u64 = 28;
    pub const VNO: u64 = 29;
    pub const LOG: u64 = 30;
    pub const K: u64 = 31;
}

/// Addresses fields.
pub mod a {
    pub const CITY: u64 = 3;
    pub const TYPE: u64 = 7;
    pub const FOREIGN_KEY: u64 = 10;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Summary {
    Total(u64),
    Count(u64),
}

#[derive(Clone, Debug)]
pub struct Field {
    pub occurrence: u64,
    pub id: u64,
    pub name: &'static str,
    pub ty: FieldType,
    pub kind: FieldKind,
    pub repetitions: u32,
    pub summary: Option<Summary>,
    /// Auto-entered on creation (with auto-enter on).
    pub auto: Option<&'static str>,
    pub not_empty_always: bool,
    pub unique_always: bool,
}

fn field(occurrence: u64, id: u64, name: &'static str, ty: FieldType) -> Field {
    Field {
        occurrence,
        id,
        name,
        ty,
        kind: FieldKind::Normal,
        repetitions: 1,
        summary: None,
        auto: None,
        not_empty_always: false,
        unique_always: false,
    }
}

pub fn fields() -> Vec<Field> {
    use FieldType::*;
    let mut out = vec![
        field(CONTACTS, f::FIRST, "First Name", Text),
        field(CONTACTS, f::LAST, "Last Name", Text),
        field(CONTACTS, f::PHOTO, "Photo", Container),
        field(CONTACTS, f::TITLE, "Title", Text),
        field(CONTACTS, f::COMPANY, "Company", Text),
        field(CONTACTS, f::JOB, "Job Title", Text),
        field(CONTACTS, f::WEBSITE, "Website", Text),
        field(CONTACTS, f::PRIMARY_KEY, "PrimaryKey", Text),
        Field {
            auto: Some("Admin"),
            ..field(CONTACTS, f::CREATED_BY, "CreatedBy", Text)
        },
        field(CONTACTS, f::N, "N", Number),
        field(CONTACTS, f::D, "D", Date),
        field(CONTACTS, f::T, "T", Time),
        field(CONTACTS, f::TS, "TS", Timestamp),
        Field {
            repetitions: 3,
            ..field(CONTACTS, f::R, "R", Text)
        },
        Field {
            repetitions: 3,
            ..field(CONTACTS, f::NR, "NR", Number)
        },
        Field {
            kind: FieldKind::Calculation,
            ..field(CONTACTS, f::CN, "CN", Number)
        },
        Field {
            kind: FieldKind::Calculation,
            ..field(CONTACTS, f::CU, "CU", Text)
        },
        Field {
            kind: FieldKind::Summary,
            summary: Some(Summary::Total(f::N)),
            ..field(CONTACTS, f::S, "S", Number)
        },
        Field {
            kind: FieldKind::Summary,
            summary: Some(Summary::Count(f::FIRST)),
            ..field(CONTACTS, f::SC, "SC", Number)
        },
        Field {
            kind: FieldKind::Global,
            ..field(CONTACTS, f::G, "G", Text)
        },
        Field {
            auto: Some("auto"),
            ..field(CONTACTS, f::AE, "AE", Text)
        },
        Field {
            auto: Some("over"),
            ..field(CONTACTS, f::AEO, "AEO", Text)
        },
        Field {
            not_empty_always: true,
            ..field(CONTACTS, f::VNE, "VNE", Text)
        },
        Field {
            unique_always: true,
            ..field(CONTACTS, f::VU, "VU", Text)
        },
        field(CONTACTS, f::VNO, "VNO", Text),
        field(CONTACTS, f::LOG, "LOG", Text),
        field(CONTACTS, f::K, "K", Text),
    ];
    out.extend([
        field(ADDRESSES, a::CITY, "City", FieldType::Text),
        field(ADDRESSES, a::TYPE, "Type", FieldType::Text),
        field(ADDRESSES, a::FOREIGN_KEY, "ForeignKey", FieldType::Text),
    ]);
    out
}

/// A record's cells: field -> repetitions.
pub type Cells = BTreeMap<u64, Vec<String>>;

pub struct Record {
    pub id: RecordId,
    pub mod_count: u64,
    pub cells: Cells,
}

/// The file.
pub struct Mem {
    pub fields: Vec<Field>,
    pub contacts: Vec<Record>,
    pub addresses: Vec<Record>,
    pub globals: Cells,
    /// The found set of Contacts (ids), in order.
    pub found: Vec<RecordId>,
    pub sort: Vec<(u64, u64)>,
    pub next_id: RecordId,
    pub locale: Locale,
    /// Interrupt after this many checks of `interrupted` (an aborted
    /// script); `None` never.
    pub interrupt_after: Option<usize>,
    pub checks: usize,
}

impl Mem {
    pub fn new() -> Mem {
        Mem {
            fields: fields(),
            contacts: Vec::new(),
            addresses: Vec::new(),
            globals: Cells::new(),
            found: Vec::new(),
            sort: Vec::new(),
            next_id: 1,
            locale: Locale::en_us(),
            interrupt_after: None,
            checks: 0,
        }
    }

    /// The import's and export's check of an abort.
    fn check(&mut self) -> bool {
        self.checks += 1;
        self.interrupt_after.is_some_and(|n| self.checks > n)
    }

    pub fn field(&self, occurrence: u64, id: u64) -> Option<&Field> {
        self.fields
            .iter()
            .find(|f| f.occurrence == occurrence && f.id == id)
    }

    /// Add a Contacts record from (field, repetitions) text.
    pub fn add(&mut self, cells: &[(u64, &[&str])]) -> RecordId {
        let id = self.next_id;
        self.next_id += 1;
        let mut c = Cells::new();
        for (field, reps) in cells {
            c.insert(*field, reps.iter().map(|s| s.to_string()).collect());
        }
        // Auto-entered at creation by the Setup scripts' New Record.
        c.entry(f::AE).or_insert_with(|| vec!["auto".into()]);
        c.entry(f::AEO).or_insert_with(|| vec!["over".into()]);
        c.entry(f::CREATED_BY)
            .or_insert_with(|| vec!["Admin".into()]);
        self.contacts.push(Record {
            id,
            mod_count: 0,
            cells: c,
        });
        self.found.push(id);
        id
    }

    pub fn add_address(&mut self, key: &str, city: &str, kind: &str) {
        let id = 1000 + self.addresses.len() as RecordId;
        let mut c = Cells::new();
        c.insert(a::FOREIGN_KEY, vec![key.into()]);
        c.insert(a::CITY, vec![city.into()]);
        c.insert(a::TYPE, vec![kind.into()]);
        self.addresses.push(Record {
            id,
            mod_count: 0,
            cells: c,
        });
    }

    pub fn record(&self, id: RecordId) -> &Record {
        self.contacts.iter().find(|r| r.id == id).expect("record")
    }

    /// Record `id` of Contacts or of Addresses.
    fn in_table(&self, occurrence: u64, id: RecordId) -> &Record {
        if occurrence == ADDRESSES {
            self.addresses.iter().find(|r| r.id == id).expect("address")
        } else {
            self.record(id)
        }
    }

    fn text(&self, record: &Record, field: u64) -> String {
        record
            .cells
            .get(&field)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    }

    /// Sort the found set by fields of Contacts (text order, empty first).
    pub fn sort_by(&mut self, fields: &[u64]) {
        let mut found = self.found.clone();
        found.sort_by(|x, y| {
            for field in fields {
                let a = self.text(self.record(*x), *field);
                let b = self.text(self.record(*y), *field);
                let o = om_calc_order(&a, &b);
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
        self.found = found;
        self.sort = fields.iter().map(|f| (CONTACTS, *f)).collect();
    }
}

/// The probes' text order: empty first, then case-insensitive, then as is.
fn om_calc_order(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, false) => return std::cmp::Ordering::Less,
        (false, true) => return std::cmp::Ordering::Greater,
        _ => {}
    }
    a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b))
}

impl Default for Mem {
    fn default() -> Self {
        Mem::new()
    }
}

fn number(text: &str) -> Option<Number> {
    (!text.is_empty()).then(|| om_transfer::text::parse_number(text))
}

impl ExportSource for Mem {
    fn interrupted(&mut self) -> bool {
        self.check()
    }

    fn context(&mut self) -> Result<ExportContext> {
        Ok(ExportContext {
            file_name: "OM_Probe_ImportExport.fmp12".into(),
            layout_name: String::new(),
            total_records: self.contacts.len() as u64,
            product_build: "08-26-2026".into(),
            product_name: "FileMaker".into(),
            product_version: "Server 26.0.3".into(),
            date_format: "M/d/yyyy".into(),
            time_format: "h:mm:ss a".into(),
            locale: self.locale.clone(),
            created: "2026-10-29T14:39:28Z".into(),
        })
    }

    fn field(&mut self, occurrence: u64, id: u64) -> Result<FieldInfo> {
        let f = Mem::field(self, occurrence, id)
            .ok_or_else(|| TransferError::new(102, "missing field"))?;
        Ok(FieldInfo {
            name: f.name.into(),
            occurrence: if occurrence == CONTACTS {
                "Contacts".into()
            } else {
                "Addresses".into()
            },
            related: occurrence != CONTACTS,
            ty: f.ty,
            kind: f.kind,
            repetitions: f.repetitions,
            empty_ok: true,
        })
    }

    fn records(&mut self) -> Result<Vec<ExportRecord>> {
        Ok(self
            .found
            .iter()
            .map(|id| ExportRecord {
                id: *id,
                mod_count: self.record(*id).mod_count,
            })
            .collect())
    }

    fn sort_fields(&mut self) -> Result<Vec<(u64, u64)>> {
        Ok(self.sort.clone())
    }

    fn values(
        &mut self,
        record: RecordId,
        occurrence: u64,
        field: u64,
    ) -> Result<Vec<Vec<ExportValue>>> {
        let rec = self.record(record);
        if occurrence == ADDRESSES {
            let key = self.text(rec, f::PRIMARY_KEY);
            if key.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(self
                .addresses
                .iter()
                .filter(|r| self.text(r, a::FOREIGN_KEY) == key)
                .map(|r| vec![ExportValue::Stored(self.text(r, field))])
                .collect());
        }
        if field == f::G {
            let g = self.globals.get(&f::G).cloned().unwrap_or_default();
            return Ok(vec![g.into_iter().map(ExportValue::Stored).collect()]);
        }
        let reps = rec.cells.get(&field).cloned().unwrap_or_default();
        Ok(vec![reps.into_iter().map(ExportValue::Stored).collect()])
    }

    fn summary(
        &mut self,
        occurrence: u64,
        field: u64,
        records: &[RecordId],
        _by: Option<(u64, u64)>,
    ) -> Result<ExportValue> {
        let f = Mem::field(self, occurrence, field)
            .unwrap()
            .summary
            .unwrap();
        Ok(match f {
            Summary::Total(of) => {
                let mut total: Option<Number> = None;
                for r in records {
                    if let Some(n) = number(&self.text(self.record(*r), of)) {
                        total = Some(match total {
                            Some(t) => t.add(&n, 16),
                            None => n,
                        });
                    }
                }
                ExportValue::Stored(
                    total
                        .map(|t| om_transfer::text::number_text(&t))
                        .unwrap_or_default(),
                )
            }
            Summary::Count(of) => {
                let n = records
                    .iter()
                    .filter(|r| !self.text(self.record(**r), of).is_empty())
                    .count();
                ExportValue::Stored(n.to_string())
            }
        })
    }

    /// The probe's totals and counts are the found set's on every record.
    fn record_summaries(
        &mut self,
        occurrence: u64,
        field: u64,
        records: &[RecordId],
    ) -> Result<Vec<ExportValue>> {
        let found = self.found.clone();
        let value = self.summary(occurrence, field, &found, None)?;
        Ok(vec![value; records.len()])
    }
}

impl ImportTarget for Mem {
    fn interrupted(&mut self) -> bool {
        self.check()
    }

    fn fields(&mut self, occurrence: u64) -> Result<Vec<TargetField>> {
        Ok(self
            .fields
            .iter()
            .filter(|f| f.occurrence == occurrence)
            .map(|f| TargetField {
                id: f.id,
                name: f.name.into(),
                ty: f.ty,
                kind: f.kind,
                repetitions: f.repetitions,
            })
            .collect())
    }

    /// Contacts' found set (the window's); another table's records in
    /// creation order, as FileMaker Server updates another table than the
    /// window's (oracle x04).
    fn found_set(&mut self, occurrence: u64) -> Result<Vec<RecordId>> {
        Ok(if occurrence == ADDRESSES {
            self.addresses.iter().map(|r| r.id).collect()
        } else {
            self.found.clone()
        })
    }

    fn value(&mut self, occurrence: u64, record: RecordId, field: u64) -> Result<String> {
        Ok(self.text(self.in_table(occurrence, record), field))
    }

    fn create(
        &mut self,
        occurrence: u64,
        writes: &[FieldWrite],
        auto_enter: bool,
    ) -> Result<RecordId> {
        let mut cells = Cells::new();
        apply(&mut cells, writes);
        if auto_enter {
            for f in self.fields.iter().filter(|f| f.occurrence == occurrence) {
                if let Some(v) = f.auto {
                    if !writes.iter().any(|w| w.field == f.id) {
                        cells.insert(f.id, vec![v.into()]);
                    }
                }
            }
        }
        self.validate(occurrence, &cells, None)?;
        let id = self.next_id;
        self.next_id += 1;
        let record = Record {
            id,
            mod_count: 0,
            cells,
        };
        if occurrence == ADDRESSES {
            self.next_id -= 1;
            let id = 1000 + self.addresses.len() as RecordId;
            self.addresses.push(Record { id, ..record });
            return Ok(id);
        } else {
            self.contacts.push(record);
        }
        Ok(id)
    }

    fn update(
        &mut self,
        occurrence: u64,
        record: RecordId,
        writes: &[FieldWrite],
        _auto_enter: bool,
    ) -> Result<()> {
        let mut cells = self.in_table(occurrence, record).cells.clone();
        apply(&mut cells, writes);
        self.validate(occurrence, &cells, Some(record))?;
        let table = if occurrence == ADDRESSES {
            &mut self.addresses
        } else {
            &mut self.contacts
        };
        let r = table.iter_mut().find(|r| r.id == record).unwrap();
        r.cells = cells;
        r.mod_count += 1;
        Ok(())
    }

    fn set_found_set(&mut self, occurrence: u64, records: &[RecordId]) -> Result<()> {
        if occurrence == CONTACTS {
            self.found = records.to_vec();
        }
        Ok(())
    }
}

fn apply(cells: &mut Cells, writes: &[FieldWrite]) {
    for w in writes {
        let reps = cells.entry(w.field).or_default();
        let i = w.repetition as usize - 1;
        while reps.len() <= i {
            reps.push(String::new());
        }
        reps[i] = w.text.clone();
    }
}

impl Mem {
    fn validate(&self, occurrence: u64, cells: &Cells, me: Option<RecordId>) -> Result<()> {
        for f in self.fields.iter().filter(|f| f.occurrence == occurrence) {
            let v = cells
                .get(&f.id)
                .and_then(|r| r.first())
                .cloned()
                .unwrap_or_default();
            if f.not_empty_always && v.is_empty() {
                return Err(TransferError::new(
                    509,
                    format!("{} requires a value", f.name),
                ));
            }
            if f.unique_always
                && !v.is_empty()
                && self
                    .contacts
                    .iter()
                    .any(|r| Some(r.id) != me && self.text(r, f.id) == v)
            {
                return Err(TransferError::new(
                    504,
                    format!("{} must be unique", f.name),
                ));
            }
        }
        Ok(())
    }
}

/// The records battery x01's Setup makes, as stored.
pub fn x01() -> Mem {
    let mut m = Mem::new();
    m.globals.insert(f::G, vec!["glob".into()]);
    m.add(&[
        (f::K, &["1"]),
        (f::VNE, &["v1"]),
        (f::VU, &["u1"]),
        (f::PRIMARY_KEY, &["C1"]),
        (f::FIRST, &["Ada"]),
        (f::LAST, &["Lovelace"]),
        (f::COMPANY, &["Analytical"]),
        (f::TITLE, &["He said \"hi\", ok"]),
        (f::JOB, &["a\tb\rc"]),
        (f::WEBSITE, &["naïve – €100 😀 日本"]),
        (f::N, &["1234.5"]),
        (f::D, &["12/10/1815"]),
        (f::T, &["13:05:09"]),
        (f::TS, &["01/02/2020 03:04:05 PM"]),
        (f::R, &["r1", "", "r3"]),
        (f::NR, &["1", "2.50", "-3"]),
        (f::CN, &["2469"]),
        (f::CU, &["Ada!"]),
    ]);
    m.add_address("C1", "London", "");
    m.add(&[
        (f::K, &["2"]),
        (f::VNE, &["v2"]),
        (f::VU, &["u2"]),
        (f::PRIMARY_KEY, &["C2"]),
        (f::FIRST, &["Bob"]),
        (f::COMPANY, &["Analytical"]),
        (f::TITLE, &["a,b"]),
        (f::JOB, &["x\u{0b}y"]),
        (f::WEBSITE, &["Ünïcödé"]),
        (f::N, &["-0.001"]),
        (f::T, &["00:00:00"]),
        (f::NR, &["", "5"]),
        (f::CN, &["-.002"]),
        (f::CU, &["Bob!"]),
    ]);
    m.add(&[
        (f::K, &["3"]),
        (f::VNE, &["v3"]),
        (f::VU, &["u3"]),
        (f::PRIMARY_KEY, &["C3"]),
        (f::FIRST, &["Ünïcödé Ωμέγα"]),
        (f::LAST, &["日本語テキスト"]),
        (f::COMPANY, &["Beta"]),
        (f::TITLE, &["trailing space "]),
        (f::JOB, &["line1\rline2\r"]),
        (f::N, &["007"]),
        (f::D, &["02/29/2024"]),
        (f::T, &["26:01:00"]),
        (f::TS, &["12/31/1999 11:59:59.5 PM"]),
        (f::CN, &["14"]),
        (f::CU, &["Ünïcödé Ωμέγα!"]),
    ]);
    m.add(&[(f::K, &["4"]), (f::VNE, &["v4"]), (f::VU, &["u4"])]);
    m.add(&[
        (f::K, &["5"]),
        (f::VNE, &["v5"]),
        (f::VU, &["u5"]),
        (f::PRIMARY_KEY, &["C5"]),
        (f::FIRST, &["Eve"]),
        (f::LAST, &["Tab\tEnd"]),
        (f::COMPANY, &["Beta"]),
        (f::TITLE, &["<b>&amp;</b> 'q'"]),
        (f::N, &["12345678901234567890.125"]),
        (f::D, &["01/01/0001"]),
        (f::T, &["-01:00:00"]),
        (f::NR, &["", "", ".1"]),
        (f::CN, &["24691357802469135780.25"]),
        (f::CU, &["Eve!"]),
    ]);
    m
}

/// Battery x03's records: x01's, the imported Addresses and two records
/// imported with values as typed.
pub fn x03() -> Mem {
    let mut m = x01();
    m.add_address("C1", "Paris", "Home");
    m.add_address("C1", "Rome", "");
    m.add_address("C5", "Oslo", "Work");
    for (k, first, n, d, t, ts, nr) in [
        (
            "6",
            "Typed",
            "abc",
            "2/29/2024",
            "1:02",
            "1/2/2020 15:04",
            ["1", "x"],
        ),
        (
            "7",
            "Short",
            "1e3",
            "01/01/2024",
            "1:02:03.25",
            "1/2/2020",
            ["", ""],
        ),
    ] {
        let cn = if k == "6" { "0" } else { "2000" };
        let cu = format!("{first}!");
        m.add(&[
            (f::K, &[k]),
            (f::FIRST, &[first]),
            (f::N, &[n]),
            (f::D, &[d]),
            (f::T, &[t]),
            (f::TS, &[ts]),
            (f::VNE, &["v"]),
            (f::VNO, &["o"]),
            (f::COMPANY, &["Gamma"]),
            (f::NR, &nr),
            (f::CN, &[cn]),
            (f::CU, &[cu.as_str()]),
        ]);
    }
    m
}
