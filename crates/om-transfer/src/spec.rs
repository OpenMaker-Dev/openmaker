//! The import and export specs Import Records, Export Records and Save
//! Records as Excel store (script-steps.md 11.3-11.5): `FMImportOptions`
//! (`84`), the per-field import flags (`f0`), `FMExportOptions` (`85`),
//! the translation profile (`86`) and Import Records' profile extension
//! (`87`). Each decodes from and encodes to the stored stream byte for
//! byte; keys keep the form they were read in (`00` or `02 01 00` for "no
//! key", script-steps.md 11.0).

use crate::charset::{ExportCharset, ImportCharset};

/// A stored `HBKey`: its bytes as read, `[u8 n][segments]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key(pub Vec<u8>);

impl Key {
    /// "No key" as FileMaker's XML importer writes it: the segment `00`.
    pub fn none() -> Key {
        Key(vec![0x02, 0x01, 0x00])
    }

    /// A one-segment key holding `id` (the segment read big-endian, the
    /// numbering of `om_scripts::runtime::FieldRef`).
    pub fn from_id(id: u64) -> Key {
        let mut segment = Vec::new();
        let mut v = id;
        loop {
            segment.insert(0, v as u8);
            v >>= 8;
            if v == 0 {
                break;
            }
        }
        let mut out = vec![segment.len() as u8 + 1, segment.len() as u8];
        out.extend(segment);
        Key(out)
    }

    /// The first segment read big-endian; `None` for no key (`00` or the
    /// segment `00`).
    pub fn id(&self) -> Option<u64> {
        let (&len, rest) = self.0.get(1..)?.split_first()?;
        let segment = rest.get(..usize::from(len))?;
        if segment.is_empty() || segment.len() > 8 {
            return None;
        }
        let v = segment.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
        (v != 0).then_some(v)
    }
}

/// A field spec in a stream (`DBFieldSpec::Write`): table occurrence key,
/// field key, 1-based repetition.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldSpec {
    pub table: Key,
    pub field: Key,
    pub repetition: u16,
}

impl FieldSpec {
    /// A field of an occurrence, keys in FileMaker's XML importer's form.
    pub fn new(occurrence: u64, field: u64) -> FieldSpec {
        FieldSpec {
            table: Key::from_id(occurrence),
            field: Key::from_id(field),
            repetition: 1,
        }
    }

    /// The "none" spec (empty keys).
    pub fn none() -> FieldSpec {
        FieldSpec {
            table: Key::none(),
            field: Key::none(),
            repetition: 1,
        }
    }

    /// The occurrence and field ids, when both are present.
    pub fn ids(&self) -> Option<(u64, u64)> {
        Some((self.table.id()?, self.field.id()?))
    }
}

/// A spec's stream did not have its layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecError;

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stored import/export spec does not have its layout")
    }
}

impl std::error::Error for SpecError {}

type Result<T> = std::result::Result<T, SpecError>;

/// A big-endian reader over a stored stream.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or(SpecError)?;
        let out = self.bytes.get(self.at..end).ok_or(SpecError)?;
        self.at += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn key(&mut self) -> Result<Key> {
        let n = usize::from(self.u8()?);
        let mut out = vec![n as u8];
        out.extend(self.take(n)?);
        Ok(Key(out))
    }

    fn field(&mut self) -> Result<FieldSpec> {
        Ok(FieldSpec {
            table: self.key()?,
            field: self.key()?,
            repetition: self.u16()?,
        })
    }

    /// A stream string (`SwapOStream::InternalSCSUCompress`, 11.0).
    fn string(&mut self) -> Result<String> {
        let first = self.u8()?;
        let n = match first {
            0..=0x7f => usize::from(first),
            0x80..=0xbf => (usize::from(first & 0x3f) << 8) | usize::from(self.u8()?),
            0xc2 => usize::from(self.u16()?),
            0xc4 => self.u32()? as usize,
            _ => return Err(SpecError),
        };
        om_scsu::decode_scsu(self.take(n)?).map_err(|_| SpecError)
    }

    fn rest(&mut self) -> &'a [u8] {
        let out = &self.bytes[self.at..];
        self.at = self.bytes.len();
        out
    }

    fn is_done(&self) -> bool {
        self.at == self.bytes.len()
    }

    fn done(self) -> Result<()> {
        if self.is_done() {
            Ok(())
        } else {
            Err(SpecError)
        }
    }
}

fn put_key(out: &mut Vec<u8>, key: &Key) {
    out.extend(&key.0);
}

fn put_field(out: &mut Vec<u8>, field: &FieldSpec) {
    put_key(out, &field.table);
    put_key(out, &field.field);
    out.extend(field.repetition.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, text: &str) {
    let bytes = om_scsu::encode_scsu(text);
    let n = bytes.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x4000 {
        out.extend((n as u16 | 0x8000).to_be_bytes());
    } else if n < 0x1_0000 {
        out.push(0xc2);
        out.extend((n as u16).to_be_bytes());
    } else {
        out.push(0xc4);
        out.extend((n as u32).to_be_bytes());
    }
    out.extend(bytes);
}

// ------------------------------------------------------------- export (85)

/// One field of the export order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportField {
    pub field: FieldSpec,
    /// For a summary field: the break field it is grouped by
    /// (`mSumByFieldSpec`); empty keys otherwise.
    pub sum_by: FieldSpec,
}

/// `FMExportOptions` (`85`, script-steps.md 11.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    pub fields: Vec<ExportField>,
    /// "Group by" fields with their selected byte (1).
    pub group_by: Vec<(FieldSpec, u8)>,
    /// "Apply current layout's data formatting to exported data".
    pub formatting: bool,
    pub charset: ExportCharset,
    /// `ExpUseNameAlias`, written by 26.x only (FileMaker's XML importer
    /// writes 1); `None` when the stream ends after the character set.
    pub use_name_alias: Option<u8>,
    /// "Use field names as column names in first row", written after
    /// `use_name_alias` by FileMaker Pro 26.1.1 only (script-steps.md
    /// 12.4); `None` when the stream ends before it.
    pub field_names: Option<u8>,
}

impl ExportOptions {
    pub fn decode(bytes: &[u8]) -> Result<ExportOptions> {
        let mut r = Reader::new(bytes);
        let n = r.u16()?;
        let mut fields = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            let field = r.field()?;
            let sum_by = r.field()?;
            fields.push(ExportField { field, sum_by });
        }
        let g = r.u16()?;
        let mut group_by = Vec::with_capacity(usize::from(g));
        for _ in 0..g {
            let field = r.field()?;
            group_by.push((field, r.u8()?));
        }
        let formatting = r.u8()? == 1;
        let charset = ExportCharset::from_stored(r.u8()?).ok_or(SpecError)?;
        let use_name_alias = if r.is_done() { None } else { Some(r.u8()?) };
        let field_names = if r.is_done() { None } else { Some(r.u8()?) };
        r.done()?;
        Ok(ExportOptions {
            fields,
            group_by,
            formatting,
            charset,
            use_name_alias,
            field_names,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend((self.fields.len() as u16).to_be_bytes());
        for f in &self.fields {
            put_field(&mut out, &f.field);
            put_field(&mut out, &f.sum_by);
        }
        out.extend((self.group_by.len() as u16).to_be_bytes());
        for (f, selected) in &self.group_by {
            put_field(&mut out, f);
            out.push(*selected);
        }
        out.push(u8::from(self.formatting));
        out.push(self.charset.stored());
        if self.use_name_alias.is_some() {
            out.extend(self.use_name_alias);
            out.extend(self.field_names);
        }
        out
    }

    /// Whether the order asks for a first row of field names (26.1.1's
    /// byte set to 1).
    pub fn has_field_names(&self) -> bool {
        self.field_names == Some(1)
    }
}

// ------------------------------------------------------------- import (84)

/// The import action (`TImportAction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportAction {
    /// Add new records.
    Add,
    /// Update existing records in the found set, in order (XML "Replace").
    UpdateFoundSet,
    /// Update matching records in the found set (XML "Update").
    UpdateMatching,
}

impl ImportAction {
    fn from_stored(v: u8) -> Option<ImportAction> {
        Some(match v {
            0 => ImportAction::Add,
            1 => ImportAction::UpdateFoundSet,
            2 => ImportAction::UpdateMatching,
            _ => return None,
        })
    }

    fn stored(self) -> u8 {
        match self {
            ImportAction::Add => 0,
            ImportAction::UpdateFoundSet => 1,
            ImportAction::UpdateMatching => 2,
        }
    }
}

/// `TImportFlags`.
pub mod import_flag {
    pub const SPLIT_REPETITIONS: u8 = 0x01;
    pub const AUTO_ENTER: u8 = 0x02;
    pub const MATCH_NAMES: u8 = 0x04;
    /// "Add remaining data as new records" (update actions).
    pub const ADD_REMAINING: u8 = 0x08;
    pub const CONTAINERS_AS_IS: u8 = 0x10;
    pub const KEEP_ORIGINAL: u8 = 0x20;
}

/// What a source column does (`FMImportFieldMapEntry::MappingOption`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapping {
    Import,
    DontImport,
    /// The match field of an update-matching import.
    Match,
    /// "Unimportable" (3).
    Unimportable,
}

impl Mapping {
    fn from_stored(v: u8) -> Option<Mapping> {
        Some(match v {
            0 => Mapping::Import,
            1 => Mapping::DontImport,
            2 => Mapping::Match,
            3 => Mapping::Unimportable,
            _ => return None,
        })
    }

    fn stored(self) -> u8 {
        match self {
            Mapping::Import => 0,
            Mapping::DontImport => 1,
            Mapping::Match => 2,
            Mapping::Unimportable => 3,
        }
    }
}

/// One source column of the import order: its target field (an empty key
/// for none) and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapEntry {
    pub field: Key,
    pub mapping: Mapping,
}

/// `FMImportOptions` (`84`, script-steps.md 11.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    /// The target table occurrence.
    pub table: Key,
    pub action: ImportAction,
    /// [`import_flag`] bits.
    pub flags: u8,
    pub charset: ImportCharset,
    /// One entry per source column, in column order.
    pub map: Vec<MapEntry>,
}

impl ImportOptions {
    pub fn decode(bytes: &[u8]) -> Result<ImportOptions> {
        let mut r = Reader::new(bytes);
        let table = r.key()?;
        let action = ImportAction::from_stored(r.u8()?).ok_or(SpecError)?;
        let flags = r.u8()?;
        let charset = ImportCharset::from_stored(r.u8()?).ok_or(SpecError)?;
        let n = r.u16()?;
        let mut map = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            let field = r.key()?;
            let mapping = Mapping::from_stored(r.u8()?).ok_or(SpecError)?;
            map.push(MapEntry { field, mapping });
        }
        r.done()?;
        Ok(ImportOptions {
            table,
            action,
            flags,
            charset,
            map,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.table.0.clone();
        out.extend([self.action.stored(), self.flags, self.charset.stored()]);
        out.extend((self.map.len() as u16).to_be_bytes());
        for e in &self.map {
            put_key(&mut out, &e.field);
            out.push(e.mapping.stored());
        }
        out
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

/// `FMImportOptions::WriteImportFlags` (`f0`): the name of a new table to
/// import into (empty otherwise) and per-field flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldFlags {
    pub new_table: String,
    pub fields: Vec<(Key, u8)>,
}

impl FieldFlags {
    pub fn decode(bytes: &[u8]) -> Result<FieldFlags> {
        let mut r = Reader::new(bytes);
        let new_table = r.string()?;
        let n = r.u16()?;
        let mut fields = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            let key = r.key()?;
            fields.push((key, r.u8()?));
        }
        r.done()?;
        Ok(FieldFlags { new_table, fields })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, &self.new_table);
        out.extend((self.fields.len() as u16).to_be_bytes());
        for (k, f) in &self.fields {
            put_key(&mut out, k);
            out.push(*f);
        }
        out
    }
}

// ------------------------------------------------------------ profile (86)

/// A file type (`FMTranslationData::FT*`, 11.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileType {
    /// `TABS`
    TabSeparated,
    /// `COMS`
    CommaSeparated,
    /// `MRGE`
    Merge,
    /// `HTML`
    HtmlTable,
    /// `DBF `
    DBase,
    /// `XMLE`: XML export.
    XmlExport,
    /// `XML `: XML import.
    XmlImport,
    /// `XLXE`: Excel workbook export (Export Records, Save Records as Excel).
    ExcelExport,
    /// `XLSX`: Excel workbook import.
    Excel2007,
    /// `XLS `: Excel 97-2004 import.
    Excel2003,
    /// `FMPR`: a FileMaker file.
    FileMaker,
    /// `BTCH`: a folder of files.
    Folder,
    /// `ODBC`
    Odbc,
    /// Any other (obsolete) four-character type, kept as stored.
    Other([u8; 4]),
}

impl FileType {
    /// The name FileMaker Pro's Type menu shows for the type (Export
    /// Records' file dialog; Claris, "Exporting data").
    pub fn name(self) -> &'static str {
        match self {
            FileType::TabSeparated => "Tab-Separated Values",
            FileType::CommaSeparated => "Comma-Separated Values",
            FileType::Merge => "Merge",
            FileType::HtmlTable => "HTML Table",
            FileType::XmlExport | FileType::XmlImport => "XML",
            FileType::ExcelExport | FileType::Excel2007 => "Excel Workbooks (.xlsx)",
            FileType::DBase => "DBF",
            _ => "Other",
        }
    }

    pub fn from_code(code: [u8; 4]) -> FileType {
        match &code {
            b"TABS" => FileType::TabSeparated,
            b"COMS" => FileType::CommaSeparated,
            b"MRGE" => FileType::Merge,
            b"HTML" => FileType::HtmlTable,
            b"DBF " => FileType::DBase,
            b"XMLE" => FileType::XmlExport,
            b"XML " => FileType::XmlImport,
            b"XLXE" => FileType::ExcelExport,
            b"XLSX" => FileType::Excel2007,
            b"XLS " => FileType::Excel2003,
            b"FMPR" => FileType::FileMaker,
            b"BTCH" => FileType::Folder,
            b"ODBC" => FileType::Odbc,
            _ => FileType::Other(code),
        }
    }

    pub fn code(self) -> [u8; 4] {
        match self {
            FileType::TabSeparated => *b"TABS",
            FileType::CommaSeparated => *b"COMS",
            FileType::Merge => *b"MRGE",
            FileType::HtmlTable => *b"HTML",
            FileType::DBase => *b"DBF ",
            FileType::XmlExport => *b"XMLE",
            FileType::XmlImport => *b"XML ",
            FileType::ExcelExport => *b"XLXE",
            FileType::Excel2007 => *b"XLSX",
            FileType::Excel2003 => *b"XLS ",
            FileType::FileMaker => *b"FMPR",
            FileType::Folder => *b"BTCH",
            FileType::Odbc => *b"ODBC",
            FileType::Other(c) => c,
        }
    }

    /// The extension FileMaker's save dialog proposes.
    pub fn extension(self) -> &'static str {
        match self {
            FileType::TabSeparated => "tab",
            FileType::CommaSeparated => "csv",
            FileType::Merge => "mer",
            FileType::HtmlTable => "htm",
            FileType::DBase => "dbf",
            FileType::XmlExport | FileType::XmlImport => "xml",
            FileType::ExcelExport | FileType::Excel2007 => "xlsx",
            FileType::Excel2003 => "xls",
            FileType::FileMaker => "fmp12",
            _ => "",
        }
    }
}

/// Where an XML import reads its document or style sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlSource {
    /// 1: a file (the step's path list) or the text of a calculation.
    File(String),
    /// 2: an HTTP URL.
    Url(String),
    /// 3: a field (XML source) or no style sheet (XSL).
    Field,
    /// An XSL style sheet from a path list (`UniversalPathListToStream`).
    Paths(Vec<[String; 3]>),
    /// 0 (no XSL in an export).
    None,
}

/// The class body after the base u16.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileBody {
    /// `TABS`, `COMS`, `MRGE`, `DBF `, `HTML`, `XLXE`: nothing more.
    Plain,
    /// `FMPR`: the source table key and, from older writers, more bytes.
    FileMaker { table: Key, rest: Vec<u8> },
    /// `XLS `, `XLSX`: the selected sheet, worksheet name and file name.
    Excel {
        sheet: u32,
        worksheet: String,
        file: String,
    },
    /// `BTCH`.
    Folder {
        folder: String,
        include_enclosed: u8,
        pictures: u8,
        by_reference: u8,
    },
    /// `XML ` import: the document's source and the style sheet's.
    XmlImport { xml: XmlSource, xsl: XmlSource },
    /// `XMLE`: the grammar (1 FMPXMLRESULT, 2 FMPDSORESULT) and style sheet.
    XmlExport { grammar: u16, xsl: XmlSource },
    /// Any body kept as stored (ODBC, obsolete types).
    Raw(Vec<u8>),
}

/// `FMTranslationProfile` (`86`, script-steps.md 11.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub file_type: FileType,
    /// The base u16: 1 when the profile has a field names row.
    pub field_names: u16,
    pub body: ProfileBody,
}

impl Profile {
    pub fn new(file_type: FileType) -> Profile {
        Profile {
            file_type,
            field_names: 0,
            body: ProfileBody::Plain,
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Profile> {
        let mut r = Reader::new(bytes);
        let code: [u8; 4] = r.take(4)?.try_into().unwrap();
        let file_type = FileType::from_code(code);
        let field_names = r.u16()?;
        let body = match file_type {
            FileType::TabSeparated
            | FileType::CommaSeparated
            | FileType::Merge
            | FileType::DBase
            | FileType::HtmlTable
            | FileType::ExcelExport => ProfileBody::Plain,
            FileType::FileMaker => ProfileBody::FileMaker {
                table: r.key()?,
                rest: r.rest().to_vec(),
            },
            FileType::Excel2003 | FileType::Excel2007 => ProfileBody::Excel {
                sheet: r.u32()?,
                worksheet: r.string()?,
                file: r.string()?,
            },
            FileType::Folder => ProfileBody::Folder {
                folder: r.string()?,
                include_enclosed: r.u8()?,
                pictures: r.u8()?,
                by_reference: r.u8()?,
            },
            FileType::XmlImport => {
                let xml = match r.u8()? {
                    1 => XmlSource::File(r.string()?),
                    2 => XmlSource::Url(r.string()?),
                    3 => XmlSource::Field,
                    _ => return Err(SpecError),
                };
                let xsl = match r.u8()? {
                    1 => XmlSource::Paths(path_list(&mut r)?),
                    2 => XmlSource::Url(r.string()?),
                    3 => XmlSource::Field,
                    _ => return Err(SpecError),
                };
                ProfileBody::XmlImport { xml, xsl }
            }
            FileType::XmlExport => {
                let grammar = r.u16()?;
                let xsl = match r.u16()? {
                    0 => XmlSource::None,
                    1 => XmlSource::Paths(path_list(&mut r)?),
                    2 => XmlSource::Url(r.string()?),
                    _ => return Err(SpecError),
                };
                ProfileBody::XmlExport { grammar, xsl }
            }
            FileType::Odbc | FileType::Other(_) => ProfileBody::Raw(r.rest().to_vec()),
        };
        r.done()?;
        Ok(Profile {
            file_type,
            field_names,
            body,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.file_type.code().to_vec();
        out.extend(self.field_names.to_be_bytes());
        match &self.body {
            ProfileBody::Plain => {}
            ProfileBody::FileMaker { table, rest } => {
                put_key(&mut out, table);
                out.extend(rest);
            }
            ProfileBody::Excel {
                sheet,
                worksheet,
                file,
            } => {
                out.extend(sheet.to_be_bytes());
                put_string(&mut out, worksheet);
                put_string(&mut out, file);
            }
            ProfileBody::Folder {
                folder,
                include_enclosed,
                pictures,
                by_reference,
            } => {
                put_string(&mut out, folder);
                out.extend([*include_enclosed, *pictures, *by_reference]);
            }
            ProfileBody::XmlImport { xml, xsl } => {
                put_source(&mut out, xml, false);
                put_source(&mut out, xsl, false);
            }
            ProfileBody::XmlExport { grammar, xsl } => {
                out.extend(grammar.to_be_bytes());
                put_source(&mut out, xsl, true);
            }
            ProfileBody::Raw(bytes) => out.extend(bytes),
        }
        out
    }
}

fn path_list(r: &mut Reader<'_>) -> Result<Vec<[String; 3]>> {
    let n = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..n {
        out.push([r.string()?, r.string()?, r.string()?]);
    }
    Ok(out)
}

fn put_source(out: &mut Vec<u8>, source: &XmlSource, wide: bool) {
    let kind = |out: &mut Vec<u8>, k: u8| {
        if wide {
            out.extend(u16::from(k).to_be_bytes());
        } else {
            out.push(k);
        }
    };
    match source {
        XmlSource::None => kind(out, 0),
        XmlSource::File(s) => {
            kind(out, 1);
            put_string(out, s);
        }
        XmlSource::Paths(paths) => {
            kind(out, 1);
            out.extend((paths.len() as u32).to_be_bytes());
            for p in paths {
                for s in p {
                    put_string(out, s);
                }
            }
        }
        XmlSource::Url(s) => {
            kind(out, 2);
            put_string(out, s);
        }
        XmlSource::Field => kind(out, 3),
    }
}

/// Import Records' profile extension (`87`, `KImportProfileEx`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileEx {
    /// The 0-based row holding field names; -1 for none. The rows up to and
    /// including it are not imported (oracle x02, i_names2).
    pub field_names_row: i16,
    /// A predefined profile id, -1 for none (FileMaker's XML importer
    /// writes 0).
    pub predefined: i32,
    /// The field delimiter text (a tab for tab files).
    pub delimiter: String,
}

impl ProfileEx {
    pub fn decode(bytes: &[u8]) -> Result<ProfileEx> {
        let mut r = Reader::new(bytes);
        let field_names_row = r.u16()? as i16;
        let predefined = r.u32()? as i32;
        let delimiter = r.string()?;
        r.done()?;
        Ok(ProfileEx {
            field_names_row,
            predefined,
            delimiter,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend((self.field_names_row as u16).to_be_bytes());
        out.extend((self.predefined as u32).to_be_bytes());
        put_string(&mut out, &self.delimiter);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn export_options_round_trip() {
        // Contacts::First Name, a summary field by Company, group by
        // Company, unformatted, UTF-16, with ExpUseNameAlias (step writer
        // probe g02, rendered by FileMaker's Save as XML).
        let bytes = unhex(
            "0002 0403d00002 020101 0001 020100 020100 0001
                   0403d00002 02010a 0001 0403d00002 020105 0001
             0001 0403d00002 020105 0001 01
             00 03 01",
        );
        let spec = ExportOptions::decode(&bytes).unwrap();
        assert_eq!(spec.fields.len(), 2);
        assert_eq!(spec.fields[0].field.ids(), Some((0xd00002, 1)));
        assert_eq!(spec.fields[0].sum_by.ids(), None);
        assert_eq!(spec.fields[1].sum_by.ids(), Some((0xd00002, 5)));
        assert_eq!(spec.group_by[0].0.ids(), Some((0xd00002, 5)));
        assert!(!spec.formatting);
        assert_eq!(spec.charset, ExportCharset::Utf16);
        assert_eq!(spec.use_name_alias, Some(1));
        assert_eq!((spec.field_names, spec.has_field_names()), (None, false));
        assert_eq!(spec.encode(), bytes);
        // FileMaker Pro 26.1.1 writes one more byte, field names (12.4;
        // synthetic: no 26.1.1 step has been read).
        let long = [&bytes[..], &[1]].concat();
        let spec = ExportOptions::decode(&long).unwrap();
        assert_eq!((spec.field_names, spec.has_field_names()), (Some(1), true));
        assert_eq!(spec.encode(), long);
        assert!(ExportOptions::decode(&[&long[..], &[0]].concat()).is_err());
        // Older writers end after the character set.
        let short = &bytes[..bytes.len() - 1];
        let spec = ExportOptions::decode(short).unwrap();
        assert_eq!(spec.use_name_alias, None);
        assert_eq!(spec.encode(), short);
        assert!(ExportOptions::decode(&bytes[..10]).is_err());
    }

    #[test]
    fn import_options_round_trip() {
        let bytes = unhex("0403d00002 02 0a 05 0002 020101 02 020102 00");
        let spec = ImportOptions::decode(&bytes).unwrap();
        assert_eq!(spec.table.id(), Some(0xd00002));
        assert_eq!(spec.action, ImportAction::UpdateMatching);
        assert!(spec.has(import_flag::AUTO_ENTER) && spec.has(import_flag::ADD_REMAINING));
        assert_eq!(spec.charset, ImportCharset::Utf8);
        assert_eq!(spec.map[0].mapping, Mapping::Match);
        assert_eq!(spec.map[1].field.id(), Some(2));
        assert_eq!(spec.encode(), bytes);
    }

    #[test]
    fn profiles_round_trip() {
        for hex in [
            "54414253 0001",
            "584d4c45 0001 0001 0000",
            "584c5845 0001",
            // XLSX: sheet 0, "Sheet1", "file:omprobe_s.xlsx" (probe g05)
            "584c5358 0001 00000000 06 09323f3f2e6b 13 3c33363f603537282835383f05297439362229",
        ] {
            let bytes = unhex(hex);
            let p = Profile::decode(&bytes).unwrap();
            assert_eq!(p.encode(), bytes, "{hex}");
        }
        let p = Profile::decode(&unhex("584c5358 0001 00000000 06 09323f3f2e6b 00")).unwrap();
        assert_eq!(
            p.body,
            ProfileBody::Excel {
                sheet: 0,
                worksheet: "Sheet1".into(),
                file: String::new()
            }
        );
        let ex = ProfileEx::decode(&unhex("0000 ffffffff 01 53")).unwrap();
        assert_eq!(ex.field_names_row, 0);
        assert_eq!(ex.predefined, -1);
        assert_eq!(ex.delimiter, "\t");
        assert_eq!(ex.encode(), unhex("0000 ffffffff 01 53"));
    }

    #[test]
    fn keys() {
        assert_eq!(Key::from_id(0xd00002).0, vec![4, 3, 0xd0, 0, 2]);
        assert_eq!(Key::from_id(5).0, vec![2, 1, 5]);
        assert_eq!(Key::none().id(), None);
        assert_eq!(Key(vec![0]).id(), None);
        assert_eq!(FieldSpec::new(0xd00002, 5).ids(), Some((0xd00002, 5)));
    }
}
