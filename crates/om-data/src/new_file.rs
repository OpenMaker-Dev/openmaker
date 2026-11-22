//! A new, blank file, as File > Create New... makes one: a file created
//! under the name the user gives, holding one table of that name with
//! FileMaker Pro's five default fields and one record, an occurrence and a
//! layout of it, OpenMaker's own theme, the accounts Admin (full access, no
//! password) and [Guest] (inactive), FileMaker's three built-in privilege
//! sets and its extended privileges (docs/re/file-format.md 8, Pro's blank
//! file compared key by key with this one; docs/new-files.md, what
//! OpenMaker writes and its limits).
//!
//! Everything is written from the specs, nothing copied from a file
//! FileMaker made: page 0 (`om_hbam::new_superblock`), the file status
//! `[2]`, the security catalog `[23]` with the checksums FileMaker checks
//! at open ([`crate::security_checksum`]), the theme ([`theme`]), and the
//! schema and layout through the same planners that edit an existing file
//! ([`crate::schema_draft`], [`crate::layout_catalog`]), so a file made
//! here and one edited later agree. The tree is then written whole by the
//! bulk rebuild (`om_hbam::writer::rebuild`).

mod theme;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use om_calc::Locale;
use om_hbam::{serial, OwnedNode};

use crate::field_catalog::listed_key;
use crate::field_options::{
    AutoCalculation, AutoEnter, AutoEnterValue, EntryValue, FieldOptions, Indexing, OptionFormula,
    Storage, StrictType, Validation,
};
use crate::layout_catalog::{self, NewLayout};
use crate::schema_draft::{SchemaCatalogs, SchemaDraft, Stamp};
use crate::security_checksum::{self as checksum, FileStamp};
use crate::{DataError, SchemaChange};

/// What the new file is made of besides its contents.
#[derive(Clone, Debug, PartialEq)]
pub struct NewFile {
    /// The file's name without `.fmp12`: the name it is created under, and
    /// its table's, occurrence's and layout's.
    pub name: String,
    /// The file's locale and formats `[2, 0d]`, FileMaker's for the
    /// system's language.
    pub locale: Locale,
    /// The user name the members' stamps carry (`Get ( UserName )`).
    pub user: String,
    /// When the file is made: the date-time key of the stamps and of
    /// Admin's password (schema-path.md 3.1), 8 bytes.
    pub time: Vec<u8>,
    /// `mInternalCreation` `[2, 254]`, which every security checksum but
    /// an extended privilege's hashes (file-format.md 8).
    pub internal_creation: u32,
}

/// Why no file was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

impl From<DataError> for Refusal {
    fn from(error: DataError) -> Self {
        Refusal(error.message)
    }
}

/// The file's bytes. `random` fills UUIDs and the password salts.
pub fn blank(file: &NewFile, random: &mut dyn FnMut(&mut [u8])) -> Result<Vec<u8>, Refusal> {
    let root = tree(file, random)?;
    let superblock = om_hbam::new_superblock(&file.name).map_err(|e| Refusal(e.to_string()))?;
    om_hbam::writer::rebuild(&root, &superblock).map_err(|e| Refusal(e.to_string()))
}

/// File > Create New... for a blank file: the operation its dialog and
/// `om-author`'s `create_file` command both run (AGENTS.md rule 3). It
/// needs no open file, so it is always available.
pub const CREATE_FILE: &str = "create_file";
/// Its File menu item, as FileMaker Pro 26 names it.
pub const CREATE_FILE_LABEL: &str = "Create New...";
/// The extension of the files it makes.
pub const EXTENSION: &str = "fmp12";

/// A blank file [`create`] made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    /// Where it is, with its extension.
    pub path: PathBuf,
    /// Its name, and its table's and layout's.
    pub name: String,
    /// The account that opens it, with no password.
    pub account: &'static str,
}

/// Why [`create`] made no file. Nothing was written, and no file was
/// replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateError {
    /// The name cannot be a new file's, and why.
    Name(String),
    /// A file or folder of that name is there already.
    Exists(PathBuf),
    /// The file could not be written, and why.
    Write(String),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateError::Name(why) | CreateError::Write(why) => f.write_str(why),
            CreateError::Exists(path) => write!(
                f,
                "“{}” already exists. Choose another name.",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
        }
    }
}

impl std::error::Error for CreateError {}

/// The path a chosen name stands for: `.fmp12` added unless it ends so, as
/// FileMaker's save panel adds it.
pub fn file_path(chosen: &Path) -> PathBuf {
    match chosen.extension() {
        Some(extension) if extension.eq_ignore_ascii_case(EXTENSION) => chosen.to_path_buf(),
        _ => {
            let mut name = chosen.as_os_str().to_owned();
            name.push(".");
            name.push(EXTENSION);
            PathBuf::from(name)
        }
    }
}

/// Make a blank file at `path` ([`file_path`] of it), named as the file,
/// in this system's locale and stamped by this user now. Never replaces a
/// file: the file is created new, and a failed write removes it.
pub fn create(path: &Path) -> Result<Created, CreateError> {
    let path = file_path(path);
    let name = path
        .file_stem()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| CreateError::Name("A new file needs a name.".into()))?
        .to_string();
    let now = jiff::Zoned::now();
    let text = format!(
        "{}/{}/{} {}:{:02}:{:02}",
        now.month(),
        now.day(),
        now.year(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let time = om_collate::timestamp_key(&text, om_collate::DateOrder::MonthDayYear)
        .ok_or_else(|| CreateError::Write("The system clock cannot be read.".into()))?;
    let file = NewFile {
        name: name.clone(),
        locale: system_locale(),
        user: crate::host::machine::user_name(),
        time,
        internal_creation: internal_creation(now.timestamp().as_second()),
    };
    let mut entropy = None;
    let bytes = blank(&file, &mut |out: &mut [u8]| {
        if let Err(e) = getrandom::getrandom(out) {
            entropy = Some(e);
        }
    })
    .map_err(|e| CreateError::Name(e.0))?;
    if let Some(e) = entropy {
        return Err(CreateError::Write(format!("No random numbers: {e}.")));
    }
    let mut out = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(CreateError::Exists(path));
        }
        Err(e) => {
            return Err(CreateError::Write(format!(
                "“{}” cannot be created: {e}.",
                path.display()
            )))
        }
    };
    if let Err(e) = out.write_all(&bytes).and_then(|()| out.sync_all()) {
        drop(out);
        let _ = std::fs::remove_file(&path);
        return Err(CreateError::Write(format!(
            "“{}” cannot be written: {e}.",
            path.display()
        )));
    }
    Ok(Created {
        path,
        name,
        account: ADMIN,
    })
}

/// The locale a new file takes: FileMaker's for the system's language and
/// region (`LC_ALL`, else `LC_TIME`, else `LANG`, as `en_US.UTF-8`), or
/// English (United States) for one OpenMaker has no record of.
pub fn system_locale() -> Locale {
    let tag = ["LC_ALL", "LC_TIME", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|t| !t.is_empty()))
        .unwrap_or_default();
    let tag = tag
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('_', "-");
    Locale::for_tag(&tag).unwrap_or_else(Locale::en_us)
}

/// `mInternalCreation` for a file made `unix_seconds` after 1970: the
/// seconds since 1904-01-01, cut to 32 bits (file-format.md 8).
pub fn internal_creation(unix_seconds: i64) -> u32 {
    const FROM_1904_TO_1970: i64 = 2_082_844_800;
    (unix_seconds + FROM_1904_TO_1970) as u32
}

/// The account a new file has, with full access and no password.
pub const ADMIN: &str = "Admin";
/// [Guest]'s stored name: a placeholder for the localized "[Guest]"
/// (local-files.md 2.1).
const GUEST: &str = "[\u{e002}]";

/// Top-level catalogs.
const STATUS: u8 = 0x02;
const LAYOUTS: u8 = 0x04;
const THEMES: u8 = 0x06;
const SECURITY: u8 = 0x17;
const FONTS: u8 = 0x19;
const VALUE_LISTS: u8 = 0x21;
/// Keys every catalog shares (schema-path.md 5.1, 8.1).
const INDEX: u8 = 0x01;
const MEMBERS: u8 = 0x05;
const LAST_ID: u8 = 0x00;
const NAMES: u8 = 0x01;
const ORDER: u8 = 0x03;
const COUNT: u8 = 0x04;
const ENTRIES: u8 = 0x07;
const NAME: u8 = 0x10;
const UNIQUE_ID: u8 = 0xd8;
const SOURCE_ID: u8 = 0xd9;
const MOD_USER: &[u8] = &[0xfc, 0x01];
const MOD_ACCOUNT: &[u8] = &[0xfc, 0x02];
const MOD_TIME: &[u8] = &[0xfc, 0x03];
/// The file status `[2]` (file-format.md 3.4, file-metadata.md 2, 3).
const FORMAT: u8 = 0x03;
const VERSION: u8 = 0x05;
const LOCALE: u8 = 0x0d;
const PREFERENCES: u8 = 0x18;
const INTERNAL_CREATION: u8 = 0xfe;
/// A field's next serial value and its tags (schema-path.md 5.5).
const NEXT_SERIAL: u8 = 0x0c;
const TAGS: u8 = 0xda;
/// A table's last issued record id, its indexes, records and record list
/// (query-program.md 7.1, write-path.md 5).
const LAST_RECORD: u8 = 0x00;
const INDEXES: u8 = 0x01;
const RECORDS: u8 = 0x05;
const RECORD_LIST: u8 = 0x0d;
const VALUE_INDEX: u8 = 0x00;
const VALUE_COUNTS: u8 = 0x10;
/// A layout's view flags (layout-model.md 4.2) as Pro's blank layout has
/// them: no view disabled, Form the default.
const VIEW_FLAGS: u8 = 0xf1;
const BLANK_LAYOUT_VIEWS: u64 = 0x0000_000f_10f3_0801;

/// The index language "Unicode" in FileMaker Pro's list: raw code units
/// (query-program.md 7.2), which Pro's blank file gives PrimaryKey.
const UNICODE_RAW: u32 = 2;
/// The file version every FileMaker 26 file has.
const FILE_VERSION: u16 = 1208;
/// The keys before the first table and occurrence FileMaker issues,
/// `80 01` and `d0 00 01` (file-format.md 8): an empty catalog's last
/// issued id.
const BEFORE_FIRST_TABLE: &[u8] = &[0x80, 0x00];
const BEFORE_FIRST_OCCURRENCE: &[u8] = &[0xd0, 0x00, 0x00];

/// The fields FileMaker Pro 26 gives a new file's table, its default
/// fields (document preference `0x8000`; file-format.md 8): their names,
/// types and options as Pro's blank files store them, with comments of
/// OpenMaker's own.
/// Each field's options are given the occurrence its calculation is
/// evaluated from.
type DefaultField = (
    &'static str,
    crate::FieldKind,
    &'static str,
    fn(u64) -> FieldOptions,
);
const DEFAULT_FIELDS: [DefaultField; 5] = [
    (
        "PrimaryKey",
        crate::FieldKind::Text,
        "The record's unique key",
        |occurrence| FieldOptions {
            auto_enter: AutoEnter {
                value: AutoEnterValue::Calculated(AutoCalculation {
                    // As Pro's blank stores its text.
                    formula: OptionFormula::new("Get( UUID )", occurrence),
                    replace_existing: true,
                    skip_when_empty: true,
                }),
                prohibit_modification: true,
            },
            validation: required(None, true),
            // Indexed in Unicode raw (language 2), as Pro's: its keys are
            // the UUID's code units (query-program.md 7.2).
            storage: Storage {
                indexing: Indexing::Minimal,
                language: UNICODE_RAW,
                ..Storage::default()
            },
            furigana: Default::default(),
        },
    ),
    (
        "CreationTimestamp",
        crate::FieldKind::Timestamp,
        "When the record was made",
        |_| entered(AutoEnterValue::Creation(EntryValue::Timestamp), true),
    ),
    (
        "CreatedBy",
        crate::FieldKind::Text,
        "The account that made the record",
        |_| entered(AutoEnterValue::Creation(EntryValue::AccountName), false),
    ),
    (
        "ModificationTimestamp",
        crate::FieldKind::Timestamp,
        "When the record last changed",
        |_| entered(AutoEnterValue::Modification(EntryValue::Timestamp), true),
    ),
    (
        "ModifiedBy",
        crate::FieldKind::Text,
        "The account that last changed the record",
        |_| entered(AutoEnterValue::Modification(EntryValue::AccountName), false),
    ),
];

/// Validation "Not empty", always enforced (no override), as every default
/// field has; `unique` too for the key.
fn required(strict_type: Option<StrictType>, unique: bool) -> Validation {
    Validation {
        allow_override: false,
        strict_type,
        not_empty: true,
        unique,
        ..Validation::default()
    }
}

/// A default field entered automatically and not modifiable; a timestamp
/// also takes a four-digit year.
fn entered(value: AutoEnterValue, timestamp: bool) -> FieldOptions {
    FieldOptions {
        auto_enter: AutoEnter {
            value,
            prohibit_modification: true,
        },
        validation: required(timestamp.then_some(StrictType::FourDigitYear), false),
        storage: Storage::default(),
        furigana: Default::default(),
    }
}

/// The key segment of id `id` (`80 01` for 0x8001).
fn serial_key(id: u64) -> Vec<u8> {
    crate::schema_draft::key_of(id)
}

/// The serial key after `key`.
fn next_after(key: &[u8]) -> Vec<u8> {
    serial::encode(serial::decode(key).unwrap_or(0) + 1)
}

/// The file's key tree.
pub fn tree(file: &NewFile, random: &mut dyn FnMut(&mut [u8])) -> Result<OwnedNode, Refusal> {
    let stamp = Stamp {
        user: file.user.clone(),
        account: ADMIN.into(),
        time: file.time.clone(),
    };
    let mut root = OwnedNode::list(Vec::new(), Vec::new());
    root.set_child(status(file));

    // The table, its occurrence and its field catalog, as Manage
    // Database's New Table makes them.
    let seeded = |last: &[u8]| {
        let mut catalog = OwnedNode::list(Vec::new(), Vec::new());
        catalog.put(
            &[&[INDEX], &[LAST_ID]],
            OwnedNode::leaf([LAST_ID], key_string(last)),
        );
        catalog
    };
    let mut tables = seeded(BEFORE_FIRST_TABLE);
    tables.segment = vec![0x10];
    let mut occurrences = seeded(BEFORE_FIRST_OCCURRENCE);
    occurrences.segment = vec![0x11];
    let mut draft = SchemaDraft::new(SchemaCatalogs {
        tables: Some(tables),
        occurrences: Some(occurrences),
        relationships: None,
        fields: Default::default(),
    });
    draft.apply(&SchemaChange::CreateTable {
        name: file.name.clone(),
        comment: String::new(),
    })?;
    let table_id = om_hbam::slot::be_uint(&next_after(BEFORE_FIRST_TABLE))
        .ok_or_else(|| Refusal("The new table has no id.".into()))?;
    let occurrence_id = om_hbam::slot::be_uint(&next_after(BEFORE_FIRST_OCCURRENCE))
        .ok_or_else(|| Refusal("The new occurrence has no id.".into()))?;
    for (name, data_type, comment, _) in DEFAULT_FIELDS {
        draft.apply(&SchemaChange::CreateField {
            table_id,
            name: name.into(),
            data_type,
            comment: comment.into(),
        })?;
    }
    for (field_id, (_, _, _, options)) in (1u64..).zip(DEFAULT_FIELDS) {
        draft.apply(&SchemaChange::SetFieldOptions {
            table_id,
            field_id,
            options: Box::new(options(occurrence_id)),
        })?;
    }
    let plan = draft.finish(&stamp, &mut || new_uuid(random));
    for (catalog, node) in plan.catalogs {
        let path = catalog.path();
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        root.put(&segments, node);
    }
    // The occurrences' index holds an empty routes list `08`, as Pro's
    // blank files have it (schema-path.md 5.3).
    root.ensure(&[&[0x03], &[0x11], &[INDEX], &[0x08]]);
    // The default fields carry FileMaker's tags (schema-path.md 5.5, 5.14):
    // a default field's, and the primary key's; Pro's have no next serial
    // value, which the Options planner writes as the dialog would.
    let table_key = serial_key(table_id);
    for (field_id, tags) in (1u64..).zip([
        "#^FMI* #_FMI_0 ",
        "#_FMI_0 ",
        "#_FMI_0 ",
        "#_FMI_0 ",
        "#_FMI_0 ",
    ]) {
        if let Some(member) =
            root.at_mut(&[&table_key, &[0x03], &[MEMBERS], &serial::encode(field_id)])
        {
            member.children.retain(|c| c.segment != [NEXT_SERIAL]);
            member.set_child(om_scripts::text_node(&[TAGS], tags));
        }
    }
    let occurrence = root
        .at(&[&[0x03], &[0x11], &[MEMBERS]])
        .and_then(|m| m.children.first())
        .and_then(|m| om_hbam::slot::be_uint(&m.segment))
        .ok_or_else(|| Refusal("The new table has no occurrence.".into()))?;

    // The theme the layout uses, then the layout.
    root.set_child(theme::catalog(&stamp, &mut || new_uuid(random))?);
    // An empty layout catalog: its index's name list, order, count and
    // entries.
    let mut empty_index: Vec<OwnedNode> = [NAMES, ORDER, ENTRIES]
        .map(|list| OwnedNode::list([list], Vec::new()))
        .into();
    empty_index.push(OwnedNode::leaf([COUNT], 0u32.to_be_bytes()));
    empty_index.sort_by(|a, b| a.segment.cmp(&b.segment));
    let empty = OwnedNode::list([LAYOUTS], vec![OwnedNode::list([INDEX], empty_index)]);
    let planned = layout_catalog::plan(
        &empty,
        &NewLayout {
            name: file.name.clone(),
            occurrence,
            theme: vec![1],
            width: layout_catalog::DEFAULT_WIDTH,
        },
    )
    .map_err(|e| Refusal(e.to_string()))?;
    // Form, List and Table views available, Form first, as Pro's blank
    // layout's flags (layout-model.md 4.2: no view bit set).
    let views = BLANK_LAYOUT_VIEWS.to_be_bytes();
    let mut member = planned.member;
    member.set_child(OwnedNode::leaf(vec![VIEW_FLAGS], views));
    member.set_child(OwnedNode::leaf(vec![UNIQUE_ID], new_uuid(random).to_vec()));
    member.set_child(OwnedNode::leaf(vec![SOURCE_ID], new_uuid(random).to_vec()));
    let mut index = planned.index;
    if let Some(entry) = index.at_mut(&[&[ENTRIES], &planned.key]) {
        entry.set_child(OwnedNode::leaf(vec![VIEW_FLAGS], views));
    }
    index.set_child(OwnedNode::leaf([0x20], [0x04]));
    index.set_child(OwnedNode::leaf(vec![UNIQUE_ID], new_uuid(random).to_vec()));
    root.put(&[&[LAYOUTS], &[INDEX]], stamped(index, &stamp));
    root.put(
        &[&[LAYOUTS], &[MEMBERS], &planned.key],
        stamped(member, &stamp),
    );

    root.set_child(empty_catalog(
        crate::script_catalog::SCRIPTS,
        0x04,
        true,
        &stamp,
        &mut || new_uuid(random),
    ));
    root.set_child(security(file, &stamp, random)?);
    root.set_child(fonts(&stamp, &mut || new_uuid(random))?);
    root.set_child(empty_catalog(VALUE_LISTS, 0x01, false, &stamp, &mut || {
        new_uuid(random)
    }));
    root.set_child(catalog_64(&stamp, &mut || new_uuid(random)));
    root.set_child(catalog_65(&stamp, &mut || new_uuid(random)));
    first_record(&mut root, file, table_id, random)?;
    Ok(root)
}

/// The table's first record, as Pro's blank file has one: its values are
/// what OpenMaker's record rules enter for a new record made by Admin now
/// (`RecordPlan::compute`: the key's `Get ( UUID )`, the timestamps and the
/// account), stored with the record list, the last issued id, and the
/// entries of the fields that are indexed (query-program.md 7.1).
fn first_record(
    root: &mut OwnedNode,
    file: &NewFile,
    table_id: u64,
    random: &mut dyn FnMut(&mut [u8]),
) -> Result<(), Refusal> {
    let refuse =
        |e: &dyn std::fmt::Display| Refusal(format!("The first record cannot be made: {e}"));
    // The rules read the schema from a file's key tree: the file so far.
    let page = om_hbam::new_superblock(&file.name).map_err(|e| refuse(&e))?;
    let bytes = om_hbam::writer::rebuild(root, &page).map_err(|e| refuse(&e))?;
    let built = om_hbam::HbamFile::from_bytes(bytes).map_err(|e| refuse(&e))?;
    let tree = built.key_tree().map_err(|e| refuse(&e))?;
    let catalog = om_hbam::Catalog::from_tree(&tree).map_err(|e| refuse(&e))?;
    let table = catalog
        .tables
        .iter()
        .find(|t| t.id == table_id)
        .ok_or_else(|| Refusal("The new table is missing.".into()))?;
    let schema = crate::rules::schema::calc_schema(&tree, &catalog, &file.name);
    let plan = crate::rules::RecordPlan::from_file_schema(table, &tree, &schema)?;
    let (date, time) = clock_text(&file.time, &file.locale)
        .ok_or_else(|| Refusal("The time given is not a date-time key.".into()))?;
    let mut uuid = [0u8; 16];
    random(&mut uuid);
    let environment = crate::rules::RecordEnvironment {
        account_name: ADMIN.into(),
        user_name: file.user.clone(),
        timestamp: format!("{date} {time}"),
        date,
        time,
        uuid: uuid_text(uuid),
        containers: None,
        interrupted: None,
        limits: om_calc::eval::CalcLimits::DEFAULT,
        lookups: Default::default(),
        serials: Default::default(),
        globals: None,
        // A file being made has no window or session to answer them.
        context: Default::default(),
        held: None,
        others_held: Default::default(),
        related_needs: None,
    };
    let values = plan.compute(
        crate::rules::WriteMode::Create,
        &Default::default(),
        &Default::default(),
        &environment,
        crate::rules::Entry::default(),
    )?;

    let table_key = serial_key(table_id);
    let record = serial::encode(1);
    let mut list = om_hbam::RidList::new();
    list.insert(1);
    root.put(
        &[&table_key, &[LAST_RECORD]],
        OwnedNode::leaf([LAST_RECORD], key_string(&record)),
    );
    root.put(&[&table_key, &[RECORD_LIST]], list.to_node([RECORD_LIST]));
    for (field, text) in &values {
        let Some(text) = text else { continue };
        let segment = om_hbam::slot::uint_segment(*field);
        root.put(
            &[&table_key, &[RECORDS], &record, &segment],
            om_scripts::text_node(&segment, text),
        );
        let Some(definition) = root
            .at(&[
                &table_key,
                &[0x03],
                &[MEMBERS],
                &serial::encode(*field),
                &[0x02],
            ])
            .and_then(|n| n.value.clone())
        else {
            continue;
        };
        let Some(options) = FieldOptions::decode(&definition, None, None) else {
            continue;
        };
        let Some(data_type) = table.field(*field).map(|f| f.data_type) else {
            continue;
        };
        if options.storage.indexing == Indexing::None {
            continue;
        }
        let keys =
            om_query::keys::TextKeys::of_language(options.storage.language).ok_or_else(|| {
                Refusal("A default field's index language is not one OpenMaker keys.".into())
            })?;
        let entries = om_query::keys::record_value_entries(
            data_type,
            std::slice::from_ref(text),
            om_collate::DateOrder::MonthDayYear,
            keys,
        );
        let mut counts = 1u64.to_be_bytes().to_vec();
        counts.extend((entries.len() as u64).to_be_bytes());
        root.put(
            &[&table_key, &[INDEXES], &segment, &[VALUE_COUNTS]],
            OwnedNode::leaf([VALUE_COUNTS], counts),
        );
        for key in entries {
            root.put(
                &[&table_key, &[INDEXES], &segment, &[VALUE_INDEX], &key],
                list.to_node(key.clone()),
            );
        }
    }
    Ok(())
}

/// `Get ( UUID )`'s text of 16 random bytes: a version 4 UUID in capitals.
fn uuid_text(bytes: [u8; 16]) -> String {
    let id = {
        let mut id = bytes;
        id[6] = (id[6] & 0x0f) | 0x40;
        id[8] = (id[8] & 0x3f) | 0x80;
        id
    };
    let hex: String = id.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A date-time key (schema-path.md 3.1) as the record rules' clock gives
/// it to auto-entered values: the date and the time of day in `locale`'s
/// storage form, seconds always shown (Pro's blank: "11/13/2026 12:37:53
/// PM"). `None` for bytes that are not such a key as om-collate writes it.
pub(crate) fn clock_text(key: &[u8], locale: &Locale) -> Option<(String, String)> {
    let (&[a, b, c], rest) = key.split_first_chunk::<3>()?;
    let date = u32::from_be_bytes([0, a, b, c]);
    if date & 0x80_0000 == 0 {
        return None;
    }
    let (year, month, day) = ((date & 0x7f_ffff) >> 10, (date >> 6) & 0xf, date & 0x3f);
    // The seconds since midnight as a number key: 80, the count of digits
    // (no leading zero), then two digits a byte, each a nibble holding the
    // digit plus one, an odd count's last nibble 0.
    let (&[0x80, count], digits) = rest.split_first_chunk::<2>()? else {
        return None;
    };
    let count = usize::from(count);
    if digits.len() != count.div_ceil(2) || (count % 2 == 1 && digits.last()? & 0x0f != 0) {
        return None;
    }
    let mut seconds = 0u32;
    for (i, nibble) in digits
        .iter()
        .flat_map(|b| [b >> 4, b & 0xf])
        .take(count)
        .enumerate()
    {
        let digit = nibble
            .checked_sub(1)
            .filter(|d| *d < 10 && (i > 0 || *d > 0))?;
        seconds = seconds.checked_mul(10)?.checked_add(u32::from(digit))?;
    }
    let (hour, minute, second) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hour > 23 {
        return None;
    }
    let date = om_calc::Date::from_ymd(i64::from(year), month, day)?.format(locale);
    let (shown, marker) = if locale.twelve_hour {
        let twelve = if hour % 12 == 0 { 12 } else { hour % 12 };
        (twelve, if hour < 12 { &locale.am } else { &locale.pm })
    } else {
        (hour, &locale.suffix_24h)
    };
    let sep = locale.time_separator;
    let hour_text = if locale.pad_hour {
        format!("{shown:02}")
    } else {
        shown.to_string()
    };
    let mut time = format!("{hour_text}{sep}{minute:02}{sep}{second:02}");
    if locale.twelve_hour && locale.am_pm_before {
        time.insert_str(0, marker);
    } else {
        time.push_str(marker);
    }
    Some((date, time))
}

/// A random (version 4) UUID, as FileMaker's are [O: the starters' `d8`].
fn new_uuid(random: &mut dyn FnMut(&mut [u8])) -> [u8; 16] {
    let mut id = [0u8; 16];
    random(&mut id);
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    id
}

/// A key string: a key's length, then the key (schema-path.md 5.1).
fn key_string(key: &[u8]) -> Vec<u8> {
    let mut out = vec![key.len() as u8];
    out.extend(key);
    out
}

/// `node` with the stamp of its creation: who and when, no count
/// (schema-path.md 8.1).
fn stamped(mut node: OwnedNode, stamp: &Stamp) -> OwnedNode {
    node.set_child(om_scripts::text_node(MOD_USER, &stamp.user));
    node.set_child(om_scripts::text_node(MOD_ACCOUNT, &stamp.account));
    // The time as a key string: its key is shorter than 8 bytes before
    // 02:46:40 (schema-path.md 3.1).
    node.set_child(OwnedNode::leaf(MOD_TIME.to_vec(), key_string(&stamp.time)));
    node
}

/// A catalog's index for `members` (key, name), in their display order
/// when `ordered`: the last issued id, the name list, the order, the count
/// and an empty entry per member, its UUID and stamp.
fn index(
    members: &[(Vec<u8>, &str)],
    ordered: bool,
    stamp: &Stamp,
    uuid: &mut dyn FnMut() -> [u8; 16],
) -> Result<OwnedNode, Refusal> {
    let mut index = OwnedNode::list(vec![INDEX], Vec::new());
    if let Some((last, _)) = members.last() {
        index.set_child(OwnedNode::leaf([LAST_ID], key_string(last)));
    }
    let names = members
        .iter()
        .map(|(key, name)| Ok(OwnedNode::leaf(listed(name)?, key_string(key))))
        .collect::<Result<Vec<_>, Refusal>>()?;
    let mut names_node = OwnedNode::list([NAMES], Vec::new());
    for name in names {
        names_node.set_child(name);
    }
    index.set_child(names_node);
    if ordered {
        let order = members
            .iter()
            .zip(1u32..)
            .map(|((key, _), at)| OwnedNode::leaf(at.to_be_bytes(), key_string(key)))
            .collect();
        index.set_child(OwnedNode::list([ORDER], order));
    }
    index.set_child(OwnedNode::leaf(
        [COUNT],
        (members.len() as u32).to_be_bytes(),
    ));
    let entries = members
        .iter()
        .map(|(key, _)| OwnedNode::leaf(key.clone(), Vec::new()))
        .collect();
    index.set_child(OwnedNode::list([ENTRIES], entries));
    index.set_child(OwnedNode::leaf([UNIQUE_ID], uuid().to_vec()));
    Ok(stamped(index, stamp))
}

/// A name list key (schema-path.md 5.6). [Guest]'s placeholder has the
/// key every FileMaker file stores for it, in every language [O: the 66
/// starters], which FileMaker's own weights for U+E002 give.
fn listed(name: &str) -> Result<Vec<u8>, Refusal> {
    listed_key(name).ok_or_else(|| Refusal(format!("“{name}” has no name list key.")))
}

/// The file status `[2]`: the format and file version, the locale, the
/// document preferences and `mInternalCreation`.
fn status(file: &NewFile) -> OwnedNode {
    let mut status = OwnedNode::list([STATUS], Vec::new());
    // The version that made the file: "26.0" in Pro 26's blank files
    // ("12.0" in older ones, as the starters).
    status.set_child(OwnedNode::leaf([FORMAT], om_scsu::encode_scsu("26.0")));
    status.set_child(OwnedNode::leaf([VERSION], FILE_VERSION.to_be_bytes()));
    status.set_child(OwnedNode::leaf([LOCALE], file.locale.to_record()));
    status.set_child(OwnedNode::leaf([PREFERENCES], preferences()));
    status.set_child(OwnedNode::leaf(
        [INTERNAL_CREATION],
        file.internal_creation.to_be_bytes(),
    ));
    status
}

/// The document preferences `[2, 18]` (file-metadata.md 3.1) of a new
/// file, as Pro's blank files have them: the default options `0x403` with
/// default fields `0x8000`, no file script triggers, layout 1 as the
/// "Switch to layout" choice (unchecked), the FileMaker icon.
fn preferences() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(0x8403u32.to_be_bytes());
    out.extend([1, 1]); // field view order and direction
    out.extend([0, 0]); // OnFirstWindowOpen, OnLastWindowClose
    out.extend([0x02, 0x01, 0x01]); // the default layout: layout 1
    out.extend(1u16.to_be_bytes()); // print style
    out.extend(0u16.to_be_bytes()); // relationships print style
    out.extend(1u16.to_be_bytes()); // thumbnails
    out.extend(512u32.to_be_bytes()); // thumbnail cache, MB
    out.extend([0, 0]); // OnWindowOpen, OnWindowClose
    out.extend([0, 0, 0, 0]); // custom layout size
    out.push(0); // OnFileAVPlayerChange
    out.extend(100u16.to_be_bytes()); // the FileMaker icon
    out.extend(1u16.to_be_bytes()); // scale factor
    out.push(0); // OnWindowTransaction
    out.push(0); // its field name, empty
    out
}

/// The security catalog `[23]`: accounts, privilege sets, extended
/// privileges and the file access catalog, each member with its checksum
/// (local-files.md 2, session-identity.md 2.7, 4).
fn security(
    file: &NewFile,
    stamp: &Stamp,
    random: &mut dyn FnMut(&mut [u8]),
) -> Result<OwnedNode, Refusal> {
    let access_uuid = new_uuid(random);
    let facts = FileStamp {
        internal_creation: file.internal_creation.to_be_bytes(),
        file_access_uuid: Some(access_uuid),
        version: FILE_VERSION,
    };
    let damaged = || Refusal("A security checksum cannot be computed.".into());
    let mut security = OwnedNode::list([SECURITY], Vec::new());

    // Accounts: [Guest] (inactive, Read-Only Access) and Admin.
    let mut accounts = Vec::new();
    for (key, name, password, status, set) in [
        (vec![1], GUEST, GUEST, inactive_status(), [1, 3]),
        (vec![2], ADMIN, "", active_status(&file.time), [1, 1]),
    ] {
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        member.set_child(OwnedNode::leaf([0x06], password_value(password, random)));
        member.set_child(OwnedNode::leaf([0x0a], status));
        member.set_child(OwnedNode::leaf([0x0b], set));
        member.set_child(om_scripts::text_node(&[NAME], name));
        let sum = checksum::account(&facts, &key, &member).ok_or_else(damaged)?;
        member.set_child(OwnedNode::leaf([0x04], sum));
        member.set_child(OwnedNode::leaf([UNIQUE_ID], new_uuid(random).to_vec()));
        accounts.push(stamped(member, stamp));
    }
    let mut accounts_index = index(
        &[(vec![1], GUEST), (vec![2], ADMIN)],
        true,
        stamp,
        &mut || new_uuid(random),
    )?;
    // File Options > Open: log in as Admin with no password (local-files.md 3.1).
    accounts_index.set_child(OwnedNode::leaf([0x06], Vec::new()));
    accounts_index.set_child(om_scripts::text_node(&[NAME], ADMIN));
    // "Show sign-in fields even when OAuth or AD FS is enabled" on
    // (local-files.md 3.1), as Pro's blank files have it.
    accounts_index.set_child(om_scripts::text_node(&[0x18], "4"));
    security.set_child(OwnedNode::list(
        [0x01],
        vec![accounts_index, OwnedNode::list([MEMBERS], accounts)],
    ));

    // The three built-in privilege sets, as every starter stores them
    // (session-identity.md 2.7).
    let mut sets = Vec::new();
    let mut sums = Vec::new();
    for (key, name, records, item, other) in [
        (
            1u8,
            "[Full Access]",
            [0x55, 0x03],
            [0xdf, 0xd7, 0xd7],
            [0x9f, 0xff],
        ),
        (
            2,
            "[Data Entry Only]",
            [0x55, 0x03],
            [0xde, 0xd6, 0xd6],
            [0x03, 0x04],
        ),
        (
            3,
            "[Read-Only Access]",
            [0x10, 0x03],
            [0xde, 0xd6, 0xd6],
            [0x03, 0x04],
        ),
    ] {
        let mut member = OwnedNode::list([key], Vec::new());
        member.set_child(OwnedNode::leaf([0x02], records));
        member.set_child(OwnedNode::list(
            [0x09],
            vec![
                OwnedNode::leaf([0x04], [item[0]]),
                OwnedNode::leaf([0x11], [item[1]]),
                OwnedNode::leaf([0x21], [item[2]]),
            ],
        ));
        member.set_child(OwnedNode::leaf([0x0a], other));
        member.set_child(om_scripts::text_node(&[NAME], name));
        let sum = checksum::privilege_set(&facts, &member).ok_or_else(damaged)?;
        member.set_child(OwnedNode::leaf([0x04], sum));
        member.set_child(OwnedNode::leaf([UNIQUE_ID], new_uuid(random).to_vec()));
        sums.push((sum, key));
        sets.push(stamped(member, stamp));
    }
    let mut sets_index = index(
        &[
            (vec![1], "[Full Access]"),
            (vec![2], "[Data Entry Only]"),
            (vec![3], "[Read-Only Access]"),
        ],
        true,
        stamp,
        &mut || new_uuid(random),
    )?;
    // The index of the sets' checksums (session-identity.md 4.1).
    let mut by_sum = OwnedNode::list([0x08], Vec::new());
    for (sum, key) in sums {
        by_sum.set_child(OwnedNode::list(
            sum.to_vec(),
            vec![OwnedNode::leaf([key], Vec::new())],
        ));
    }
    sets_index.set_child(by_sum);
    security.set_child(OwnedNode::list(
        [0x02],
        vec![sets_index, OwnedNode::list([MEMBERS], sets)],
    ));

    // The extended privileges; only fmreauthenticate10 names sets, all
    // three, with sharing state `01 01` (session-identity.md 2.7, 4.2).
    let mut extended = Vec::new();
    let mut names = Vec::new();
    for (key, keyword) in EXTENDED_PRIVILEGES.iter().enumerate() {
        let key = vec![key as u8 + 1];
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        if *keyword == "fmreauthenticate10" {
            member.set_child(OwnedNode::leaf([0x02], [1, 1]));
            member.set_child(OwnedNode::list(
                [0x05],
                (1..=3).map(|s| OwnedNode::leaf([s], Vec::new())).collect(),
            ));
        }
        member.set_child(om_scripts::text_node(&[NAME], keyword));
        let sum = checksum::extended_privilege(&member).ok_or_else(damaged)?;
        member.set_child(OwnedNode::leaf([0x04], sum));
        member.set_child(OwnedNode::leaf([UNIQUE_ID], new_uuid(random).to_vec()));
        extended.push(stamped(member, stamp));
        names.push((key, *keyword));
    }
    security.set_child(OwnedNode::list(
        [0x03],
        vec![
            index(&names, false, stamp, &mut || new_uuid(random))?,
            OwnedNode::list([MEMBERS], extended),
        ],
    ));

    // The file access catalog: no authorized files, none required.
    let requires = 0u32;
    let mut access = OwnedNode::list([INDEX], Vec::new());
    access.set_child(OwnedNode::leaf([COUNT], 0u32.to_be_bytes()));
    access.set_child(OwnedNode::leaf(
        [0x09],
        checksum::file_access_catalog(&facts, requires).ok_or_else(damaged)?,
    ));
    access.set_child(OwnedNode::leaf([NAME], access_uuid));
    access.set_child(OwnedNode::leaf([UNIQUE_ID], new_uuid(random).to_vec()));
    access.set_child(OwnedNode::leaf([0xf0], requires.to_be_bytes()));
    security.set_child(OwnedNode::list([0x04], vec![stamped(access, stamp)]));
    Ok(security)
}

/// The extended privileges of a FileMaker 26 file, in key order
/// (session-identity.md 2.6; Pro's blank files).
const EXTENDED_PRIVILEGES: [&str; 11] = [
    "fmwebdirect",
    "fmxdbc",
    "fmapp",
    "fmreauthenticate10",
    "fmxml",
    "fmphp",
    "fmextscriptaccess",
    "fmurlscript",
    "fmrest",
    "fmodata",
    // FileMaker 26 adds it (Pro's blank files; the older starters have the
    // ten above). Pro stores a description with it; OpenMaker leaves it out.
    "fmplugin",
];

/// A password value `06` (local-files.md 2.1): version 1, a fresh salt,
/// the hash, and the check bytes its second byte asks for
/// (`StoredPassword::encode`).
fn password_value(password: &str, random: &mut dyn FnMut(&mut [u8])) -> Vec<u8> {
    let mut salt = [0u8; om_crypto::account::SALT_LEN];
    random(&mut salt);
    om_crypto::account::StoredPassword::encode(password, salt)
}

/// An account's status `0a` (local-files.md 2.2): [Guest]'s, inactive at
/// 1/1/1024; Admin's, active with the password set now.
fn inactive_status() -> Vec<u8> {
    vec![0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x63, 0x53, 0x99]
}

fn active_status(time: &[u8]) -> Vec<u8> {
    key_string(time)
}

/// A catalog with no members, as Pro's blank files have the script
/// catalog `[17]` (`20` = `04`) and the value lists `[33]` (`20` = `01`):
/// an index holding its `20`, its UUID and stamp, and for scripts an empty
/// member list. With no members the scripts' open-time check passes
/// (scripts.md, "The script catalog's digest").
fn empty_catalog(
    key: u8,
    format: u8,
    members: bool,
    stamp: &Stamp,
    uuid: &mut dyn FnMut() -> [u8; 16],
) -> OwnedNode {
    let mut index = OwnedNode::list([INDEX], Vec::new());
    index.set_child(OwnedNode::leaf([0x20], [format]));
    index.set_child(OwnedNode::leaf([UNIQUE_ID], uuid().to_vec()));
    let mut children = vec![stamped(index, stamp)];
    if members {
        children.push(OwnedNode::list([MEMBERS], Vec::new()));
    }
    OwnedNode::list([key], children)
}

/// Catalog `[64]` as every FileMaker file has it (Pro's blank files and the
/// starters, file-format.md 8; what it holds is unknown): one member `01`
/// with only its UUID and stamp, and an index listing it under the empty
/// name, with `18` = `01 01` and `20` = `01`.
fn catalog_64(stamp: &Stamp, uuid: &mut dyn FnMut() -> [u8; 16]) -> OwnedNode {
    let mut index = OwnedNode::list([INDEX], Vec::new());
    index.set_child(OwnedNode::leaf([LAST_ID], key_string(&[1])));
    index.set_child(OwnedNode::list(
        [NAMES],
        vec![OwnedNode::leaf([0, 0, 0], key_string(&[1]))],
    ));
    index.set_child(OwnedNode::list(
        [ORDER],
        vec![OwnedNode::leaf(1u32.to_be_bytes(), key_string(&[1]))],
    ));
    index.set_child(OwnedNode::leaf([COUNT], 1u32.to_be_bytes()));
    index.set_child(OwnedNode::list(
        [ENTRIES],
        vec![OwnedNode::leaf([1], Vec::new())],
    ));
    index.set_child(OwnedNode::leaf([0x18], key_string(&[1])));
    index.set_child(OwnedNode::leaf([0x20], [0x01]));
    index.set_child(OwnedNode::leaf([UNIQUE_ID], uuid().to_vec()));
    let member = OwnedNode::list([1], vec![OwnedNode::leaf([UNIQUE_ID], uuid().to_vec())]);
    OwnedNode::list(
        [0x40],
        vec![
            stamped(index, stamp),
            OwnedNode::list([MEMBERS], vec![stamped(member, stamp)]),
        ],
    )
}

/// Catalog `[65]`, an index with only its UUID and stamp, as the starters
/// have it. Pro 26's blank files list FileMaker's standard menus in it;
/// OpenMaker writes none of them.
fn catalog_65(stamp: &Stamp, uuid: &mut dyn FnMut() -> [u8; 16]) -> OwnedNode {
    let index = OwnedNode::list([INDEX], vec![OwnedNode::leaf([UNIQUE_ID], uuid().to_vec())]);
    OwnedNode::list([0x41], vec![stamped(index, stamp)])
}

/// The font catalog `[25]`: font `00 00`, "arial,sans-serif", as every
/// starter has it (some have no other); Pro 26's blank files list their
/// theme's fonts instead, and OpenMaker's theme names none. Styles name
/// fonts by these keys.
fn fonts(stamp: &Stamp, uuid: &mut dyn FnMut() -> [u8; 16]) -> Result<OwnedNode, Refusal> {
    const DEFAULT_FONT: &str = "arial,sans-serif";
    let key = vec![0, 0];
    let mut member = OwnedNode::list(key.clone(), Vec::new());
    member.set_child(OwnedNode::leaf([0x02], [0x01, 0x00]));
    member.set_child(om_scripts::text_node(&[NAME], DEFAULT_FONT));
    member.set_child(OwnedNode::leaf([UNIQUE_ID], uuid().to_vec()));
    Ok(OwnedNode::list(
        [FONTS],
        vec![
            index(&[(key, DEFAULT_FONT)], false, stamp, uuid)?,
            OwnedNode::list([MEMBERS], vec![stamped(member, stamp)]),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::HbamFile;

    /// A fixed byte stream for UUIDs and salts.
    fn counter() -> impl FnMut(&mut [u8]) {
        let mut next = 0u8;
        move |out: &mut [u8]| {
            for b in out {
                next = next.wrapping_add(37);
                *b = next;
            }
        }
    }

    fn alpha() -> NewFile {
        NewFile {
            name: "Alpha".into(),
            locale: Locale::en_us(),
            user: "Tester".into(),
            time: om_collate::timestamp_key(
                "11/13/2026 12:37:53",
                om_collate::DateOrder::MonthDayYear,
            )
            .unwrap(),
            internal_creation: 0xe8b3_1c05,
        }
    }

    /// A new file's empty script catalog takes its first script (#274): the
    /// name list key is FileMaker's in every file, so no stored name needs
    /// to show it.
    #[test]
    fn a_new_file_takes_its_first_script() {
        use crate::field_catalog::listed_key;
        use crate::script_catalog::plan;
        use crate::{Script, ScriptCatalog};
        use std::collections::BTreeMap;
        let root = tree(&alpha(), &mut counter()).unwrap();
        let index = root
            .child(&[crate::script_catalog::SCRIPTS])
            .and_then(|catalog| catalog.child(&[1]))
            .expect("the script index")
            .clone();
        let requested = ScriptCatalog {
            scripts: vec![Script {
                id: 1,
                name: "First".into(),
                ..Script::default()
            }],
            ..ScriptCatalog::default()
        };
        let by = Stamp {
            user: "Tester".into(),
            account: ADMIN.into(),
            time: alpha().time,
        };
        let saved = plan(
            Some(&index),
            &ScriptCatalog::default(),
            &BTreeMap::new(),
            &requested,
            &by,
            &mut || [9; 16],
        )
        .unwrap()
        .unwrap();
        assert_eq!(saved.assigned[&1], 1);
        let entry = listed_key("First").unwrap();
        assert_eq!(
            saved
                .index
                .child(&[1])
                .and_then(|names| names.child(&entry))
                .and_then(|e| e.value.clone()),
            Some(vec![1, 1])
        );
        assert_eq!(
            saved.index.child(&[4]).and_then(|n| n.value.clone()),
            Some(1u32.to_be_bytes().to_vec())
        );
    }

    /// Every stamp holds its time as a key string, also a time whose key is
    /// shorter than 8 bytes (00:00:05).
    #[test]
    fn stamps_hold_short_times_with_their_length() {
        let mut file = alpha();
        file.time =
            om_collate::timestamp_key("11/13/2026 0:00:05", om_collate::DateOrder::MonthDayYear)
                .unwrap();
        assert!(file.time.len() < 8);
        let root = tree(&file, &mut counter()).unwrap();
        let mut stamps = 0;
        fn walk(node: &OwnedNode, stamps: &mut usize) {
            for child in &node.children {
                if child.segment == MOD_TIME {
                    let value = child.value.as_deref().unwrap();
                    assert_eq!(usize::from(value[0]), value.len() - 1);
                    *stamps += 1;
                }
                walk(child, stamps);
            }
        }
        walk(&root, &mut stamps);
        assert!(stamps > 20, "{stamps}");
    }

    /// The first record's times are in the file's own formats, and a time
    /// that is not a whole date-time key makes no file.
    #[test]
    fn the_first_record_takes_the_files_formats() {
        let mut file = alpha();
        file.locale = Locale::de_de();
        let bytes = blank(&file, &mut counter()).unwrap();
        let read = HbamFile::from_bytes(bytes).unwrap();
        let created = read
            .cell(0x8001, 1, 2)
            .unwrap()
            .and_then(|c| c.text())
            .unwrap()
            .unwrap();
        assert_eq!(created, "13.11.2026 12:37:53");
        for seconds in [0u32, 5, 59, 999, 1000, 45473, 86399] {
            let text = format!(
                "11/13/2026 {}:{:02}:{:02}",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            );
            let key =
                om_collate::timestamp_key(&text, om_collate::DateOrder::MonthDayYear).unwrap();
            let (_, time) = clock_text(&key, &Locale::en_us()).unwrap();
            assert!(
                time.contains(&format!("{:02}:{:02}", seconds / 60 % 60, seconds % 60)),
                "{time}"
            );
        }
        let mut short = alpha();
        short.time.truncate(5);
        assert!(blank(&short, &mut counter()).is_err());
    }

    /// `create` adds the extension, makes the file named as it, and never
    /// replaces one.
    #[test]
    fn create_makes_a_new_file_and_replaces_none() {
        let dir = std::env::temp_dir().join(format!("om-new-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let created = create(&dir.join("Beta Gamma")).unwrap();
        assert_eq!(created.path, dir.join("Beta Gamma.fmp12"));
        let file = HbamFile::open(&created.path).unwrap();
        assert_eq!(file.catalog().unwrap().tables[0].name, "Beta Gamma");
        let again = create(&dir.join("Beta Gamma.fmp12"));
        assert!(matches!(again, Err(CreateError::Exists(_))), "{again:?}");
        assert_eq!(std::fs::read(&created.path).unwrap(), file.as_bytes());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The file opens with its table, occurrence, layout and theme, reads
    /// back and rewrites byte for byte, and every security checksum
    /// recomputes to what it stores.
    #[test]
    fn a_blank_file_reads_back_and_rewrites_unchanged() {
        let bytes = blank(&alpha(), &mut counter()).unwrap();
        let file = HbamFile::from_bytes(bytes.clone()).unwrap();
        let catalog = file.catalog().unwrap();
        let names = |n: Vec<&str>| n.into_iter().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            catalog
                .tables
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>(),
            names(vec!["Alpha"])
        );
        assert_eq!(catalog.tables[0].id, 0x8001);
        assert_eq!(catalog.table_occurrences[0].name, "Alpha");
        assert_eq!(catalog.table_occurrences[0].id, 0xd0_0001);
        assert_eq!(catalog.layouts[0].name, "Alpha");
        // Pro's default fields, and its first record with what they enter.
        let fields: Vec<&str> = catalog.tables[0]
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(
            fields,
            [
                "PrimaryKey",
                "CreationTimestamp",
                "CreatedBy",
                "ModificationTimestamp",
                "ModifiedBy"
            ]
        );
        let records = file.records(0x8001).unwrap();
        assert_eq!(records.len(), 1);
        let cell = |field: u64| {
            file.cell(0x8001, 1, field)
                .unwrap()
                .and_then(|c| c.text())
                .and_then(Result::ok)
                .unwrap_or_default()
        };
        assert_eq!(cell(1).len(), 36, "{}", cell(1));
        // PrimaryKey is indexed in Unicode raw, as in Pro's blank file
        // (#777): its index holds the UUID's code units.
        let primary = catalog.tables[0].fields[0].decoded_options().unwrap();
        assert_eq!(primary.index_language(), UNICODE_RAW);
        let keys = file.key_tree().unwrap();
        let index = keys.at(&[0x8001, 1, 1, 0]).unwrap();
        let raw = om_query::keys::TextKeys::Raw.key(&cell(1)).unwrap();
        let held: Vec<&[u8]> = index.children.iter().map(|k| k.segment.0).collect();
        assert_eq!(held, [raw.as_slice()]);
        assert_eq!(cell(2), "11/13/2026 12:37:53 PM");
        assert_eq!(cell(3), ADMIN);
        assert_eq!(cell(4), cell(2));
        assert_eq!(cell(5), ADMIN);
        let tree = file.key_tree().unwrap();
        // The layout stores a Table View layout with its one empty column,
        // as Pro's blank does: without it, or with it holding no object,
        // FileMaker Pro quits when the layout is shown as a table (#806).
        let view = tree.at(&[4, 5, 1, 0x0d]).expect("a stored table view");
        let parts: Vec<u64> = view
            .get(3)
            .unwrap()
            .children
            .iter()
            .filter_map(|p| p.id())
            .collect();
        assert_eq!(parts, [0, 1, 2]);
        let store = view.get(7).and_then(|n| n.assembled()).unwrap();
        let store = om_layout::object::ObjectStore::decode(&store).unwrap();
        assert_eq!(store.root, [2]);
        let column = &store.objects[0];
        let shows = column
            .field_obj
            .as_ref()
            .and_then(|f| f.field_spec.as_ref());
        assert_eq!(shows.map(|s| (s.table_key, s.field_key)), Some((0, 0)));
        let flags = om_layout::encode::EMPTY_COLUMN_FLAG | om_layout::encode::TABLE_VIEW_FLAG;
        assert_eq!(column.flags & flags, flags);
        // Its row is the one Pro writes for the theme's 16 pt edit box (#853).
        let bounds = column.bounds.as_ref().unwrap();
        assert_eq!((bounds.top, bounds.bottom), (24.0, 45.0));
        // The edit box's text is 16 pt on one line, with its spacing in
        // margins and no insets, which FileMaker Pro would draw inside but
        // not count (#853).
        let styles = tree.at(&[6, 5, 1, 0x16]).and_then(|n| n.assembled());
        let styles = om_layout::ThemeStyles::decode(&styles.unwrap()).unwrap();
        let edit_box = |component: i32, id: i32| {
            let property = styles
                .type_styles
                .iter()
                .filter(|set| set.object_types.contains(&61))
                .flat_map(|set| set.styles.iter().flat_map(|s| s.components.iter()))
                .filter(|c| (c.component, c.state) == (component, 1))
                .flat_map(|c| c.properties.iter())
                .find(|p| p.id == id)?;
            property.length.as_ref().map(|l| (l.value, l.unit))
        };
        assert_eq!(
            [69, 72].map(|id| edit_box(1, id)),
            [Some((16.0, 6)), Some((1.0, 9))]
        );
        let margins = [105, 106, 107, 108].map(|id| edit_box(3, id));
        assert_eq!(margins, [2.0, 4.0, 2.0, 4.0].map(|v| Some((v, 6))));
        let insets = [117, 118, 119, 120].map(|id| edit_box(3, id));
        assert_eq!(insets, [Some((0.0, 6)); 4]);
        assert!(view.at(&[0x0a]).is_some_and(|i| i.children.len() == 1));
        assert!(view.at(&[0x15]).is_some_and(|t| t.children.len() == 1));
        let root = om_hbam::OwnedNode::list(
            Vec::new(),
            tree.root
                .children
                .iter()
                .map(|n| n.to_owned_node())
                .collect(),
        );
        // Rewritten from page 0 as it was before the first write (SCN 0),
        // the file comes back byte for byte.
        let mut page = file.superblock().raw.to_vec();
        page[0x802..0x80a].fill(0);
        let again = om_hbam::writer::rebuild(&root, &page).unwrap();
        assert!(again == bytes, "the rewrite differs");

        let security = root.child(&[SECURITY]).unwrap();
        let facts = FileStamp {
            internal_creation: alpha().internal_creation.to_be_bytes(),
            file_access_uuid: security
                .at(&[&[4], &[1], &[NAME]])
                .and_then(|n| n.value.as_deref())
                .and_then(|v| v.try_into().ok()),
            version: FILE_VERSION,
        };
        let stored = |m: &OwnedNode| m.child(&[4]).and_then(|n| n.value.clone()).unwrap();
        for m in &security.at(&[&[1], &[MEMBERS]]).unwrap().children {
            assert_eq!(
                checksum::account(&facts, &m.segment, m).unwrap().to_vec(),
                stored(m)
            );
        }
        for m in &security.at(&[&[2], &[MEMBERS]]).unwrap().children {
            assert_eq!(
                checksum::privilege_set(&facts, m).unwrap().to_vec(),
                stored(m)
            );
        }
        for m in &security.at(&[&[3], &[MEMBERS]]).unwrap().children {
            assert_eq!(checksum::extended_privilege(m).unwrap().to_vec(), stored(m));
        }
        // Admin's password is the empty one, [Guest]'s its name.
        let password = |key: u8| {
            let value = security.at(&[&[1], &[MEMBERS], &[key], &[6]]).unwrap();
            om_crypto::account::StoredPassword::parse(value.value.as_deref().unwrap()).unwrap()
        };
        assert!(password(2).verify(""));
        assert!(password(1).verify(GUEST));
    }
}
