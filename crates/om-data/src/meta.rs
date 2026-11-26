//! What a file stores about itself and its schema beyond tables and fields
//! (spec `docs/re/file-metadata.md`), decoded once for every host: the
//! desktop client's calculation host, OpenMaker Server's script host and
//! the tools.
//!
//! | structure | key | here |
//! |---|---|---|
//! | file locale | `[2, 0d]` | [`locale`] ([`Locale::from_record`]) |
//! | file options, default layout, file triggers | `[2, 18]` | [`FileOptions`] |
//! | page setup | `[2, 87]` | [`page_format`], as stored |
//! | relationships | `[3, fb, 5, R]` | [`Relationship`] |
//! | summary definitions | `[T, 3, 5, F, 09]` | [`SummaryDef`] |
//! | value lists | `[21, 5, V]` | [`ValueList`] |
//! | layout and object triggers | `[4, 5, L, 30]`... | [`om_layout::triggers`] |
//! | catalog ids and display order | `[cat, 1, 03, n]` | [`display_order`], [`om_hbam::serial::id_of_raw`] |
//! | a table occurrence's saved found set | `[TO, 07, 01]` | [`SavedFoundSet`] |
//!
//! Catalog member ids stay the big-endian reading of their key segment, the
//! numbering every catalog in this workspace uses; the ids FileMaker shows
//! are their serial decode ([`om_hbam::serial::id_of_raw`],
//! file-metadata.md 1). Script ids are already the serial decode
//! (`Script::id`).

use om_calc::Locale;
use om_hbam::{KeyTree, Node};
use om_scripts::specs::{SortSpec, SpecField};

pub use om_layout::triggers::{ScriptTrigger, StoredFormula};

/// The file locale record `[2, 0d]` (file-metadata.md 2).
pub const LOCALE: [u64; 2] = [2, 0x0d];
/// The document preferences `[2, 18]` (file-metadata.md 3).
pub const DOCUMENT_PREFERENCES: [u64; 2] = [2, 0x18];
/// The page setup `[2, 87]` (file-format.md 3.4): the same tagged chunks as
/// Print Setup's stored page format (script-steps.md 11.6), a `Comm` chunk
/// and the platform's records.
pub const PAGE_SETUP: [u64; 2] = [2, 0x87];
/// Relationships `[3, fb, 5]`.
pub const RELATIONSHIPS: [u64; 3] = [3, 0xfb, 5];
/// Value lists `[21, 5]` (`KValueLists`).
pub const VALUE_LISTS: [u64; 2] = [0x21, 5];
/// The script catalog `[17]` (decimal 17, `0x11`).
pub const SCRIPTS: u64 = 17;
/// The layout catalog `[4]`.
pub const LAYOUTS: u64 = 4;

/// The file's locale, when `tree` holds a record that decodes.
pub fn locale(tree: &KeyTree<'_>) -> Option<Locale> {
    Locale::from_record(&tree.at(&LOCALE)?.assembled()?)
}

/// The most of a stored page setup [`page_format`] reads: Print Setup's
/// limit for a stored page format (script-steps.md 11.6).
pub const MAX_PAGE_FORMAT: usize = 1 << 20;

/// The file's stored page setup ([`PAGE_SETUP`]), reassembled but not
/// decoded: a host reads what it supports of it. `None` for a file that
/// stores none (no starter solution does), and for one larger than
/// [`MAX_PAGE_FORMAT`], which is refused by its declared and actual sizes
/// before anything is copied.
pub fn page_format(tree: &KeyTree<'_>) -> Option<Vec<u8>> {
    let node = tree.at(&PAGE_SETUP)?;
    let declared = (node.children.first())
        .and_then(|header| header.value?.first_chunk::<4>().copied())
        .map(|total| u32::from_be_bytes(total) as usize);
    // The pieces after the chunk header (child 0) are the value.
    let actual = node.value.map_or_else(
        || {
            node.children
                .iter()
                .skip(1)
                .filter_map(|n| n.value)
                .map(<[u8]>::len)
                .sum()
        },
        <[u8]>::len,
    );
    if actual > MAX_PAGE_FORMAT || declared.is_some_and(|total| total > MAX_PAGE_FORMAT) {
        return None;
    }
    node.assembled().map(|bytes| bytes.into_owned())
}

/// What a file stores at [`PAGE_SETUP`], told apart as a writer needs it:
/// none, a value [`page_format`] reads, or one it does not (too large, or
/// not reassembled).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredPageFormat {
    None,
    Readable(Vec<u8>),
    Unreadable,
}

/// [`StoredPageFormat`] of `tree`.
pub fn stored_page_format(tree: &KeyTree<'_>) -> StoredPageFormat {
    if tree.at(&PAGE_SETUP).is_none() {
        return StoredPageFormat::None;
    }
    page_format(tree).map_or(StoredPageFormat::Unreadable, StoredPageFormat::Readable)
}

/// Whether a page setup `value` may be replaced by another: its tagged
/// chunks run to its end, and each is one FileMaker Pro regenerates from
/// `Comm` at its next Page Setup OK, `Comm`, `M_PM` or `MMod`
/// (file-format.md 3.8). Another chunk, a Windows record say, is of
/// unmeasured use and is not dropped.
pub fn replaceable_page_format(value: &[u8]) -> bool {
    let mut rest = value;
    while !rest.is_empty() {
        let Some((tag, after)) = rest.split_first_chunk::<4>() else {
            return false;
        };
        let Some((len, after)) = after.split_first_chunk::<4>() else {
            return false;
        };
        let Some(after) = after.get(u32::from_be_bytes(*len) as usize..) else {
            return false;
        };
        if !matches!(tag, b"Comm" | b"M_PM" | b"MMod") {
            return false;
        }
        rest = after;
    }
    true
}

/// `value` as the node of a file's page setup ([`PAGE_SETUP`]), stored
/// chunked as FileMaker Pro stores it whatever its length: child `00` the
/// u32 total, then 1000-byte pieces keyed 1, 2, ... (file-format.md 3.8,
/// 4.3). Pro read a 37-byte `[2, 87]` stored this way; one stored as a
/// plain leaf it was never shown. `None` past [`MAX_PAGE_FORMAT`], which
/// [`page_format`] would not read back.
pub fn page_format_node(value: &[u8]) -> Option<om_hbam::OwnedNode> {
    if value.len() > MAX_PAGE_FORMAT {
        return None;
    }
    let total = u32::try_from(value.len()).ok()?;
    let mut children = vec![om_hbam::OwnedNode::leaf([0], total.to_be_bytes())];
    for (i, piece) in value.chunks(1000).enumerate() {
        children.push(om_hbam::OwnedNode::leaf(
            om_hbam::serial::encode(i as u64 + 1),
            piece,
        ));
    }
    Some(om_hbam::OwnedNode::list([0x87], children))
}

/// A big-endian reader over a stored stream (file-metadata.md 0).
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    /// A key `[u8 n][segments]`: its first segment read big-endian (the
    /// catalogs' numbering); `None` inside for an empty key (`00`, or the
    /// segment `00`).
    fn key(&mut self) -> Option<Option<u64>> {
        let n = usize::from(self.u8()?);
        let key = self.take(n)?;
        let Some((&len, rest)) = key.split_first() else {
            return Some(None);
        };
        let segment = rest.get(..usize::from(len))?;
        Some(om_hbam::slot::be_uint(segment).filter(|v| *v != 0))
    }

    /// A key naming a script: its segment's serial decode (`Script::id`).
    fn script_key(&mut self) -> Option<Option<u64>> {
        let n = usize::from(self.u8()?);
        let key = self.take(n)?;
        let Some((&len, rest)) = key.split_first() else {
            return Some(None);
        };
        let segment = rest.get(..usize::from(len))?;
        Some(om_hbam::serial::decode(segment).filter(|v| *v != 0))
    }

    /// A field spec: occurrence key, field key, u16 repetition.
    fn field(&mut self) -> Option<Option<SpecField>> {
        let occurrence = self.key()?;
        let field = self.key()?;
        let repetition = self.u16()?;
        Some(field.map(|field| SpecField {
            occurrence,
            field,
            repetition,
        }))
    }

    fn string(&mut self) -> Option<String> {
        let (text, used) = om_scsu::decode_counted(self.bytes.get(self.at..)?).ok()?;
        self.at += used;
        Some(text)
    }

    fn sort_spec(&mut self) -> Option<SortSpec> {
        let (spec, used) = SortSpec::read(self.bytes.get(self.at..)?)?;
        self.at += used;
        Some(spec)
    }

    /// A value list's "re-sort values based on" sort spec: entries whose
    /// field specs are empty, the language in the first one's locale.
    fn sort_locale(&mut self) -> Option<u32> {
        self.u16()?;
        let count = self.u16()?;
        let mut locale = 0;
        for i in 0..count {
            self.field()?;
            self.field()?;
            self.key()?;
            self.u8()?;
            let l = self.u32()?;
            if i == 0 {
                locale = l;
            }
        }
        Some(locale)
    }
}

// ----- file options and file triggers (file-metadata.md 3) ------------------

/// File script trigger event ids (script-runtime.md 7.1).
pub mod file_event {
    pub const FIRST_WINDOW_OPEN: u16 = 201;
    pub const LAST_WINDOW_CLOSE: u16 = 202;
    pub const WINDOW_OPEN: u16 = 204;
    pub const WINDOW_CLOSE: u16 = 205;
    pub const AV_PLAYER_CHANGE: u16 = 206;
    pub const WINDOW_TRANSACTION: u16 = 209;
}

/// Option bits of the document preferences (file-metadata.md 3.2).
pub mod file_option {
    pub const ASIAN_LINE_BREAKING: u32 = 0x0001;
    pub const ROMAN_LINE_BREAKING: u32 = 0x0002;
    pub const SMART_QUOTES: u32 = 0x0008;
    /// Open: switch to the default layout.
    pub const SWITCH_TO_LAYOUT: u32 = 0x0040;
    /// Data entry: ask whenever the settings are different.
    pub const ASK_FORMATS: u32 = 0x0200;
    /// Data entry: always use the current system settings.
    pub const SYSTEM_FORMATS: u32 = 0x0400;
    pub const HIDE_TOOLBARS: u32 = 0x4000;
    pub const USE_DEFAULT_FIELDS: u32 = 0x8000;
}

/// The document preferences `[2, 18]` (`DocumentPrefs::Read`): the option
/// bits, the default layout and the file script triggers. Members an older
/// record lacks keep FileMaker's defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOptions {
    pub options: u32,
    /// Manage Database's field list order and direction.
    pub field_view_order: u8,
    pub field_view_ascending: bool,
    /// File Options "Switch to layout" (kept when the option is off; see
    /// [`FileOptions::default_layout`]).
    pub layout: Option<u64>,
    pub first_window_open: Option<u64>,
    pub last_window_close: Option<u64>,
    pub window_open: Option<u64>,
    pub window_close: Option<u64>,
    pub av_player_change: Option<u64>,
    pub window_transaction: Option<u64>,
    /// OnWindowTransaction's field name (XML `scriptParameterFieldName`).
    pub window_transaction_field: String,
    pub print_type: u16,
    pub relations_print_style: u16,
    pub thumbnail_options: u16,
    pub thumbnail_cache_mb: u32,
    pub custom_layout_size: (u16, u16),
    /// 100 FileMaker's icon, 999 a custom icon.
    pub icon_type: u16,
    pub scale_factor: u16,
}

impl Default for FileOptions {
    fn default() -> Self {
        FileOptions {
            options: 0x403,
            field_view_order: 0,
            field_view_ascending: false,
            layout: None,
            first_window_open: None,
            last_window_close: None,
            window_open: None,
            window_close: None,
            av_player_change: None,
            window_transaction: None,
            window_transaction_field: String::new(),
            print_type: 1,
            relations_print_style: 0,
            thumbnail_options: 1,
            thumbnail_cache_mb: 512,
            custom_layout_size: (0, 0),
            icon_type: 100,
            scale_factor: 1,
        }
    }
}

impl FileOptions {
    /// Decode the record; `None` when even its first members are missing
    /// or a member is malformed.
    pub fn decode(bytes: &[u8]) -> Option<FileOptions> {
        let mut r = Reader::new(bytes);
        let mut o = FileOptions {
            options: r.u32()?,
            field_view_order: r.u8()?,
            field_view_ascending: r.u8()? != 0,
            first_window_open: r.script_key()?,
            last_window_close: r.script_key()?,
            layout: r.key()?,
            ..FileOptions::default()
        };
        // Every member from here on is optional: an older record ends.
        macro_rules! next {
            ($e:expr) => {
                if r.done() {
                    return Some(o);
                } else {
                    $e?
                }
            };
        }
        o.print_type = next!(r.u16());
        o.relations_print_style = next!(r.u16());
        o.thumbnail_options = next!(r.u16());
        o.thumbnail_cache_mb = next!(r.u32());
        o.window_open = next!(r.script_key());
        o.window_close = r.script_key()?;
        o.custom_layout_size = (next!(r.u16()), r.u16()?);
        o.av_player_change = next!(r.script_key());
        o.icon_type = next!(r.u16());
        o.scale_factor = next!(r.u16());
        o.window_transaction = next!(r.script_key());
        o.window_transaction_field = next!(r.string());
        Some(o)
    }

    /// The layout the file opens on: the default layout when "Switch to
    /// layout" is on.
    pub fn default_layout(&self) -> Option<u64> {
        self.layout
            .filter(|_| self.options & file_option::SWITCH_TO_LAYOUT != 0)
    }

    /// The script of a file trigger event ([`file_event`]).
    pub fn trigger(&self, event: u16) -> Option<u64> {
        match event {
            file_event::FIRST_WINDOW_OPEN => self.first_window_open,
            file_event::LAST_WINDOW_CLOSE => self.last_window_close,
            file_event::WINDOW_OPEN => self.window_open,
            file_event::WINDOW_CLOSE => self.window_close,
            file_event::AV_PLAYER_CHANGE => self.av_player_change,
            file_event::WINDOW_TRANSACTION => self.window_transaction,
            _ => None,
        }
    }
}

/// The file's document preferences, when `tree` holds a record that
/// decodes.
pub fn file_options(tree: &KeyTree<'_>) -> Option<FileOptions> {
    FileOptions::decode(&tree.at(&DOCUMENT_PREFERENCES)?.assembled()?)
}

// ----- relationships (file-metadata.md 4) -----------------------------------

/// A join predicate's operator (`DBJoinPred::Type`), each meaning **left
/// field op right field**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Cartesian,
}

impl JoinOp {
    /// Codes 0-6; 7 and 8 are in-memory only and fail the read.
    pub fn from_code(code: u8) -> Option<JoinOp> {
        Some(match code {
            0 => JoinOp::Equal,
            1 => JoinOp::NotEqual,
            2 => JoinOp::Less,
            3 => JoinOp::LessEqual,
            4 => JoinOp::Greater,
            5 => JoinOp::GreaterEqual,
            6 => JoinOp::Cartesian,
            _ => return None,
        })
    }

    pub fn code(self) -> u8 {
        match self {
            JoinOp::Equal => 0,
            JoinOp::NotEqual => 1,
            JoinOp::Less => 2,
            JoinOp::LessEqual => 3,
            JoinOp::Greater => 4,
            JoinOp::GreaterEqual => 5,
            JoinOp::Cartesian => 6,
        }
    }

    /// The operator seen from the other side (`a < b` is `b > a`).
    pub fn reversed(self) -> JoinOp {
        match self {
            JoinOp::Less => JoinOp::Greater,
            JoinOp::LessEqual => JoinOp::GreaterEqual,
            JoinOp::Greater => JoinOp::Less,
            JoinOp::GreaterEqual => JoinOp::LessEqual,
            other => other,
        }
    }

    /// The operator as `RelationInfo` and the relationship graph write it.
    pub fn symbol(self) -> &'static str {
        match self {
            JoinOp::Equal => "=",
            JoinOp::NotEqual => "≠",
            JoinOp::Less => "<",
            JoinOp::LessEqual => "≤",
            JoinOp::Greater => ">",
            JoinOp::GreaterEqual => "≥",
            JoinOp::Cartesian => "×",
        }
    }
}

/// `left_field op right_field`: field ids of the left and right
/// occurrences' base tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinPredicate {
    pub op: JoinOp,
    pub left: u64,
    pub right: u64,
}

/// `Definition::Options` (file-metadata.md 4.1); "X to Y" is the direction
/// of travel, the action happening to Y's records.
pub mod relation_option {
    /// Delete the right side's related records (right side's checkbox).
    pub const CASCADE_DELETE_LEFT_TO_RIGHT: u32 = 0x01;
    pub const CASCADE_DELETE_RIGHT_TO_LEFT: u32 = 0x02;
    /// Allow creating right-side records through the relationship.
    pub const CREATE_LEFT_TO_RIGHT: u32 = 0x10;
    pub const CREATE_RIGHT_TO_LEFT: u32 = 0x20;
    /// Sort the right side's records by the left-to-right order.
    pub const SORT_LEFT_TO_RIGHT: u32 = 0x40;
    /// Sort the left side's records by the right-to-left order.
    pub const SORT_RIGHT_TO_LEFT: u32 = 0x80;
}

/// A relationship `[3, fb, 5, R]`: the definition `02` and the predicates
/// `03, n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relationship {
    pub id: u64,
    /// The left and right table occurrences.
    pub left: u64,
    pub right: u64,
    /// The order of the right occurrence's records seen from the left, and
    /// of the left's seen from the right. Kept when the sort bit is off;
    /// only the bit decides ([`Relationship::order_of`]).
    pub left_to_right: SortSpec,
    pub right_to_left: SortSpec,
    pub options: u32,
    /// In definition order; all must hold.
    pub predicates: Vec<JoinPredicate>,
}

impl Relationship {
    /// The sort order of `to`'s records when travelling to it (`to` is the
    /// right or the left occurrence), when its sort bit is set.
    pub fn order_of(&self, to: u64) -> Option<&SortSpec> {
        if to == self.right && self.options & relation_option::SORT_LEFT_TO_RIGHT != 0 {
            Some(&self.left_to_right)
        } else if to == self.left && self.options & relation_option::SORT_RIGHT_TO_LEFT != 0 {
            Some(&self.right_to_left)
        } else {
            None
        }
    }

    /// Decode the definition `02` (`DBRelation::Definition::Read`) with the
    /// predicates; `None` when either is malformed.
    pub fn decode(id: u64, definition: &[u8], predicates: &[&[u8]]) -> Option<Relationship> {
        let mut r = Reader::new(definition);
        let left = r.key()??;
        let right = r.key()??;
        let left_to_right = r.sort_spec()?;
        let right_to_left = r.sort_spec()?;
        let options = r.u32()?;
        let predicates = predicates
            .iter()
            .map(|bytes| {
                let mut r = Reader::new(bytes);
                let op = JoinOp::from_code(r.u8()?)?;
                Some(JoinPredicate {
                    op,
                    left: r.key()?.unwrap_or(0),
                    right: r.key()?.unwrap_or(0),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Relationship {
            id,
            left,
            right,
            left_to_right,
            right_to_left,
            options,
            predicates,
        })
    }

    /// A relationship node `[3, fb, 5, R]`.
    pub fn from_node(entry: &Node<'_>) -> Option<Relationship> {
        let definition = entry.get(2)?.assembled()?;
        let mut predicates: Vec<(u64, std::borrow::Cow<'_, [u8]>)> = entry
            .get(3)
            .map(|n| {
                n.children
                    .iter()
                    .map(|p| Some((p.id()?, p.assembled()?)))
                    .collect::<Option<Vec<_>>>()
            })
            .unwrap_or(Some(Vec::new()))?;
        predicates.sort_by_key(|(n, _)| *n);
        let bytes: Vec<&[u8]> = predicates.iter().map(|(_, b)| b.as_ref()).collect();
        Relationship::decode(entry.id()?, &definition, &bytes)
    }
}

/// Every relationship of the file that decodes.
pub fn relationships(tree: &KeyTree<'_>) -> Vec<Relationship> {
    tree.at(&RELATIONSHIPS)
        .into_iter()
        .flat_map(|n| n.children.iter())
        .filter(|n| n.id().is_some_and(|id| id != 0))
        .filter_map(Relationship::from_node)
        .collect()
}

// ----- summary fields (file-metadata.md 5) ----------------------------------

/// `SummaryDef::SummaryType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryKind {
    /// Total of; with [`summary_flag::RUNNING`] a running total.
    Total,
    /// Average of; weighted by the additional field with
    /// [`summary_flag::OPTION`].
    Average,
    /// Count of; running with [`summary_flag::RUNNING`].
    Count,
    Minimum,
    Maximum,
    /// Standard deviation (sample); by population with
    /// [`summary_flag::OPTION`].
    StdDeviation,
    /// Fraction of total; subtotaled by the additional field with
    /// [`summary_flag::OPTION`].
    Fraction,
    /// List of.
    List,
    /// A kind FileMaker never writes (8, `kSumCountValidNumOnly`) or
    /// another unknown code.
    Other(u8),
}

impl SummaryKind {
    /// `SummaryDef::SummaryType`'s code.
    pub fn code(self) -> u8 {
        match self {
            SummaryKind::Total => 1,
            SummaryKind::Average => 2,
            SummaryKind::Count => 3,
            SummaryKind::Minimum => 4,
            SummaryKind::Maximum => 5,
            SummaryKind::StdDeviation => 6,
            SummaryKind::Fraction => 7,
            SummaryKind::List => 9,
            SummaryKind::Other(code) => code,
        }
    }

    pub fn from_code(code: u8) -> SummaryKind {
        match code {
            1 => SummaryKind::Total,
            2 => SummaryKind::Average,
            3 => SummaryKind::Count,
            4 => SummaryKind::Minimum,
            5 => SummaryKind::Maximum,
            6 => SummaryKind::StdDeviation,
            7 => SummaryKind::Fraction,
            9 => SummaryKind::List,
            other => SummaryKind::Other(other),
        }
    }
}

/// `mFlags` bits of a summary definition.
pub mod summary_flag {
    /// The kind's option: weighted, by population, subtotaled.
    pub const OPTION: u8 = 0x01;
    /// Summarize repetitions individually.
    pub const INDIVIDUAL_REPETITIONS: u8 = 0x02;
    /// Running total or count.
    pub const RUNNING: u8 = 0x04;
    /// Restart the running summary for each sorted group.
    pub const RESTART: u8 = 0x10;
}

/// A field spec of a summary definition: `HBKey<5>` occurrence, field and
/// repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SummaryField {
    /// `None` for this table (the summarized field's empty key).
    pub occurrence: Option<u64>,
    pub field: u64,
    pub repetition: u16,
}

/// The 30-byte summary definition `09` (`DBField::SummaryDef`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SummaryDef {
    pub kind: SummaryKind,
    pub flags: u8,
    /// The summarized field of the same table; `None` for "nothing".
    pub field: Option<SummaryField>,
    /// The additional field: the weight, or the break field of a
    /// subtotaled fraction or a restarting running summary.
    pub additional: Option<SummaryField>,
}

impl SummaryDef {
    /// A summary of `field` of this table, without options.
    pub fn new(kind: SummaryKind, field: u64) -> SummaryDef {
        SummaryDef {
            kind,
            flags: 0,
            field: Some(SummaryField {
                occurrence: None,
                field,
                repetition: 1,
            }),
            additional: None,
        }
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// Decode the definition; a shorter value is zero-padded and a missing
    /// one reads as Total of nothing, as `GetSummaryDef` reads it.
    pub fn decode(bytes: &[u8]) -> SummaryDef {
        let mut raw = [0u8; 30];
        let n = bytes.len().min(30);
        raw[..n].copy_from_slice(&bytes[..n]);
        // An `HBKey<5>` in 6 bytes: the length, then the key's segments;
        // bytes past the length are not part of it.
        let key = |at: usize| -> Option<u64> {
            let len = usize::from(raw[at]).min(5);
            let key = &raw[at + 1..at + 1 + len];
            let (&seglen, segment) = key.split_first()?;
            om_hbam::slot::be_uint(segment.get(..usize::from(seglen))?).filter(|v| *v != 0)
        };
        let spec = |at: usize| -> Option<SummaryField> {
            Some(SummaryField {
                occurrence: key(at),
                field: key(at + 6)?,
                repetition: u16::from_be_bytes([raw[at + 12], raw[at + 13]]),
            })
        };
        SummaryDef {
            kind: if raw[0] == 0 {
                SummaryKind::Total
            } else {
                SummaryKind::from_code(raw[0])
            },
            flags: raw[1],
            field: spec(2),
            additional: spec(16),
        }
    }

    /// The 30 bytes FileMaker stores for this definition. A key takes its
    /// length byte and key string; the unused bytes after it, which
    /// FileMaker leaves as they were in memory (schema-path.md 5.11), are
    /// written as zeros. "This table" and "no field" are the key `01 00`.
    pub fn encode(&self) -> [u8; 30] {
        let mut out = [0u8; 30];
        out[0] = self.kind.code();
        out[1] = self.flags;
        let key = |out: &mut [u8], id: Option<u64>| {
            let bytes = id.unwrap_or(0).to_be_bytes();
            let skip = bytes.iter().take(7).take_while(|b| **b == 0).count();
            let segment = &bytes[skip..];
            out[0] = u8::try_from(segment.len() + 1).expect("at most 9");
            out[1] = u8::try_from(segment.len()).expect("at most 8");
            out[2..2 + segment.len()].copy_from_slice(segment);
        };
        for (at, spec) in [(2, self.field), (16, self.additional)] {
            key(&mut out[at..at + 6], spec.and_then(|s| s.occurrence));
            key(&mut out[at + 6..at + 12], spec.map(|s| s.field));
            let repetition = spec.map_or(1, |s| s.repetition);
            out[at + 12..at + 14].copy_from_slice(&repetition.to_be_bytes());
        }
        out
    }
}

/// The first segment of a key string `[u8 n][u8 len][segment]...` read
/// big-endian, as the catalogs number their members: a field's value list
/// reference `[T, 3, 5, F, 07]` starts with one (`02 01 03` is list 3).
pub fn key_string(bytes: &[u8]) -> Option<u64> {
    Reader::new(bytes).key().flatten()
}

// ----- value lists (file-metadata.md 6) -------------------------------------

/// `DBValueList::Options`.
pub mod value_list_option {
    /// Include only related values starting from the start occurrence.
    pub const SHOW_RELATED: u16 = 0x1;
    /// Sort by the second field.
    pub const SORT_BY_SECOND: u16 = 0x2;
    /// Show values only from the second field.
    pub const SECOND_ONLY: u16 = 0x4;
}

/// Where a value list's values come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueSource {
    /// Custom values, in order (`-` alone is a divider).
    Custom(Vec<String>),
    /// Values from a field, optionally with a second field and related
    /// values only.
    Field {
        primary: SpecField,
        secondary: Option<SpecField>,
        /// "Include only related values starting from" this occurrence.
        start: Option<u64>,
    },
    /// Another file's value list (type 2).
    External {
        file: Option<u64>,
        list: Option<u64>,
    },
    /// A value-list calculation (type 3, debugger-gated in 26.0.3).
    Calculation(Option<StoredFormula>),
}

/// A value list `[21, 5, V]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueList {
    pub id: u64,
    pub name: String,
    pub source: ValueSource,
    pub options: u16,
    /// "Re-sort values based on" this `Locale::Type` (0 none).
    pub sort_locale: u32,
    /// The separator FileMaker stores (a space in every file).
    pub separator: String,
}

/// Value list member keys.
const VALUE_LIST_DEFINITION: u64 = 2;
const VALUE_LIST_VALUES: u64 = 3;
const VALUE_LIST_CALCULATION: u64 = 5;
const NAME: u64 = 0x10;

impl ValueList {
    /// Decode a value list node `[21, 5, V]`. A list without a definition is
    /// a custom list.
    pub fn from_node(entry: &Node<'_>) -> Option<ValueList> {
        let id = entry.id()?;
        let name = entry.get(NAME)?.text()?.ok()?;
        let custom = || -> Option<Vec<String>> {
            let text = match entry.get(VALUE_LIST_VALUES) {
                Some(node) => node.text()?.ok()?,
                None => String::new(),
            };
            Some(
                text.split('\r')
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )
        };
        let Some(definition) = entry.get(VALUE_LIST_DEFINITION) else {
            return Some(ValueList {
                id,
                name,
                source: ValueSource::Custom(custom()?),
                options: 0,
                sort_locale: 0,
                separator: " ".into(),
            });
        };
        let bytes = definition.assembled()?;
        let mut r = Reader::new(&bytes);
        let kind = r.u8()?;
        let options = r.u16()?;
        let primary = r.field()?;
        let secondary = r.field()?;
        let start = r.key()?;
        let file = r.key()?;
        let list = r.key()?;
        let separator = r.string()?;
        let sort_locale = if r.done() { 0 } else { r.sort_locale()? };
        let source = match kind {
            0 => ValueSource::Custom(custom()?),
            1 => ValueSource::Field {
                primary: primary?,
                secondary,
                start: start.filter(|_| options & value_list_option::SHOW_RELATED != 0),
            },
            2 => ValueSource::External { file, list },
            3 => ValueSource::Calculation(entry.get(VALUE_LIST_CALCULATION).and_then(|n| {
                let polish = n.get(6)?.assembled()?;
                Some(StoredFormula {
                    context: n.get(4).and_then(|c| c.assembled()).map(|c| c.into_owned()),
                    polish: polish.into_owned(),
                })
            })),
            _ => return None,
        };
        Some(ValueList {
            id,
            name,
            source,
            options,
            sort_locale,
            separator,
        })
    }
}

/// The file's value lists in display order.
pub fn value_lists(tree: &KeyTree<'_>) -> Vec<ValueList> {
    let Some(members) = tree.at(&VALUE_LISTS) else {
        return Vec::new();
    };
    let lists: Vec<ValueList> = members
        .children
        .iter()
        .filter(|n| n.id().is_some_and(|id| id != 0))
        .filter_map(ValueList::from_node)
        .collect();
    in_display_order(tree, 0x21, lists, |l| l.id)
}

// ----- saved found sets (file-metadata.md 11) -------------------------------

/// What FileMaker Pro saved of a table occurrence's found set, at
/// `[TO, 07, 01]` under the occurrence's key: the window reopens on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedFoundSet {
    /// Every record of the table (`ff`); `records` is then empty.
    pub all: bool,
    /// The found records (`05`, a RID list), in record id order.
    pub records: om_hbam::RidList,
    /// The current record's id (`03`); `None` for none.
    pub current: Option<u64>,
    /// Sorted (`02`) by `sort`.
    pub sorted: bool,
    /// The last sort (`0a`, `DBSortSpec::Write`), kept when the found set
    /// is no longer sorted.
    pub sort: Option<SortSpec>,
}

/// The path of a table occurrence's saved found set under the occurrence's
/// key.
pub const SAVED_FOUND_SET: [u8; 2] = [0x07, 0x01];

impl SavedFoundSet {
    /// Decode the node at `[TO, 07, 01]`. `None` when a member is
    /// malformed, or when it names neither every record nor a RID list.
    pub fn from_node(node: &Node<'_>) -> Option<SavedFoundSet> {
        let all = node.child(&[0xff]).is_some();
        let records = match node.child(&[0x05]) {
            Some(list) => om_hbam::RidList::from_node(list).ok()?,
            None if all => om_hbam::RidList::new(),
            None => return None,
        };
        // A key: `[u8 n][n bytes]`, the record id's serial key; id 0 is
        // none.
        let current = match node.child(&[0x03]).and_then(|n| n.value) {
            Some([n, key @ ..]) if usize::from(*n) == key.len() => {
                Some(om_hbam::serial::decode(key)?).filter(|id| *id != 0)
            }
            Some(_) => return None,
            None => None,
        };
        let sorted = match node.child(&[0x02]).and_then(|n| n.value) {
            Some([flag]) => *flag != 0,
            Some(_) => return None,
            None => false,
        };
        let sort = match node.child(&[0x0a]).and_then(|n| n.value) {
            Some(bytes) => Some(SortSpec::decode(bytes)?),
            None => None,
        };
        Some(SavedFoundSet {
            all,
            records,
            current,
            sorted: sorted && sort.as_ref().is_some_and(|s| !s.fields.is_empty()),
            sort,
        })
    }
}

/// A list with no value of its own whose members are leaves: a RID list's
/// shape, or a sort key's records.
fn is_list_of_leaves(node: &om_hbam::OwnedNode) -> bool {
    node.value.is_none() && node.children.iter().all(|c| c.children.is_empty())
}

/// The sorted order `08` in the forms FileMaker Pro wrote
/// (file-metadata.md 11): `00` the u64 count; `01` the first field's keys,
/// each over its records or naming a bucket (`01 n`); buckets `n` of the
/// next field's keys over their records, with `ff` the bucket's key. A
/// key's records are a leaf per record, or a RID list's bitmap words as
/// `05` holds them.
fn is_sorted_order(node: &om_hbam::OwnedNode) -> bool {
    let records = |key: &om_hbam::OwnedNode| {
        is_list_of_leaves(key)
            && (key
                .children
                .iter()
                .all(|r| r.value.as_deref().unwrap_or_default().is_empty())
                || om_hbam::RidList::from_owned(key).is_ok())
    };
    node.value.is_none()
        && node
            .children
            .iter()
            .all(|member| match member.segment.as_slice() {
                [0x00] => {
                    member.children.is_empty()
                        && member.value.as_ref().is_some_and(|v| v.len() == 8)
                }
                [0x01] => {
                    member.value.is_none()
                        && member.children.iter().all(|key| {
                            records(key)
                                || (key.children.is_empty()
                                    && key
                                        .value
                                        .as_deref()
                                        .is_some_and(|v| v.len() == 2 && v[0] == 1))
                        })
                }
                [n] if *n >= 2 => {
                    member.value.is_none()
                        && member.children.iter().all(|key| {
                            if key.segment == [0xff] {
                                key.children.is_empty()
                            } else {
                                records(key)
                            }
                        })
                }
                _ => false,
            })
}

/// What a window of a local file keeps of a table when it closes
/// ([`saved_found_set_written`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundSetWrite {
    /// The found records; `None` for every record of the table.
    pub records: Option<om_hbam::RidList>,
    /// The current record's id; `None` for none.
    pub current: Option<u64>,
    /// The table's data version `[3, 16, 5, T, fb]` (schema-path.md 5.8).
    pub data_version: u32,
}

/// The node `[TO, 07]` becomes when a window closes on `write` with the
/// found set of table `table_key` (file-metadata.md 11), from the node the
/// file holds there (`None`: none yet). It sets `03`, `05` (or `ff` with an
/// empty `05`) and `fe, <table>`; with a kept sort `0a` the set reopens
/// unsorted (`02` = 0, the sorted order `08` taken out), `f0` and `fd` kept
/// beside it; every other member stays as it is. `None` when the node holds
/// a member or a shape no file FileMaker wrote has: nothing is written then.
pub fn saved_found_set_written(
    existing: Option<&om_hbam::OwnedNode>,
    table_key: &[u8],
    write: &FoundSetWrite,
) -> Option<om_hbam::OwnedNode> {
    use om_hbam::OwnedNode;
    let leaf_of = |node: &OwnedNode| node.children.is_empty().then(|| node.value.clone());
    let (mut header, mut state, mut find) = (None, None, None);
    for member in existing.map_or(&[][..], |n| &n.children) {
        match member.segment.as_slice() {
            [0x00] => header = Some(member),
            [0x01] => state = Some(member),
            [0x02] => find = Some(member),
            _ => return None,
        }
    }
    // `[TO, 07, 00]` is `01 02` in every file; `[TO, 07]` and its state are
    // lists.
    if header.is_some_and(|h| leaf_of(h) != Some(Some(vec![1, 2])))
        || existing.is_some_and(|n| n.value.is_some())
        || state.is_some_and(|n| n.value.is_some())
    {
        return None;
    }
    let (mut sort, mut f0, mut fd, mut sorted_flag) = (None, None, None, false);
    for member in state.map_or(&[][..], |n| &n.children) {
        match member.segment.as_slice() {
            [0x02] => match leaf_of(member)?.as_deref() {
                Some([_]) => sorted_flag = true,
                _ => return None,
            },
            // A record's key; every record found.
            [0x03] => match leaf_of(member)?.as_deref() {
                Some([n, key @ ..]) if usize::from(*n) == key.len() && !key.is_empty() => {}
                _ => return None,
            },
            [0xff] => {
                if !leaf_of(member)?.unwrap_or_default().is_empty() {
                    return None;
                }
            }
            [0x05] => {
                if !is_list_of_leaves(member) {
                    return None;
                }
                om_hbam::RidList::from_owned(member).ok()?;
            }
            // The sorted order the set no longer has, in a form FileMaker
            // Pro's files have.
            [0x08] => {
                if !is_sorted_order(member) {
                    return None;
                }
            }
            [0x0a] => {
                SortSpec::decode(leaf_of(member)?.as_deref()?)?;
                sort = Some(member.clone());
            }
            [0xf0] => {
                if leaf_of(member)?.as_ref().map(Vec::len) != Some(4) {
                    return None;
                }
                f0 = Some(member.clone());
            }
            // A key alone (no value, no children) FileMaker writes beside
            // a kept sort, sorted or not (file-metadata.md 11); its meaning
            // is unknown. Only that form is known.
            [0xfd] => {
                if !(member.push && member.value.is_none() && member.children.is_empty()) {
                    return None;
                }
                fd = Some(member.clone());
            }
            [0xfe] => {
                let [version] = member.children.as_slice() else {
                    return None;
                };
                if member.value.is_some()
                    || version.segment != table_key
                    || leaf_of(version)?.map(|v| v.len()) != Some(4)
                {
                    return None;
                }
            }
            _ => return None,
        }
    }
    // Every file has `02`, `f0` and `fd` beside a kept sort only.
    if (sorted_flag || f0.is_some() || fd.is_some()) && sort.is_none() {
        return None;
    }
    let current = write.current.unwrap_or(0);
    let mut key = vec![0];
    key.extend(om_hbam::serial::encode(current));
    key[0] = (key.len() - 1) as u8;
    let mut members = Vec::new();
    if sort.is_some() {
        members.push(OwnedNode::leaf([0x02], vec![0]));
    }
    members.push(OwnedNode::leaf([0x03], key));
    members.push(match &write.records {
        Some(records) => records.to_node(vec![0x05]),
        None => OwnedNode::list([0x05], Vec::new()),
    });
    members.extend(sort);
    members.extend(f0);
    members.extend(fd);
    members.push(OwnedNode::list(
        [0xfe],
        vec![OwnedNode::leaf(
            table_key,
            write.data_version.to_be_bytes().to_vec(),
        )],
    ));
    if write.records.is_none() {
        members.push(OwnedNode::leaf([0xff], Vec::new()));
    }
    // A last find FileMaker Pro wrote is kept; without one, the empty one
    // every file has.
    let find = find.cloned().unwrap_or_else(|| {
        OwnedNode::list(
            [0x02],
            vec![
                OwnedNode::leaf([0x03], vec![1, 0]),
                OwnedNode::list([0x05], Vec::new()),
            ],
        )
    });
    Some(OwnedNode::list(
        [0x07],
        vec![
            OwnedNode::leaf([0x00], vec![1, 2]),
            OwnedNode::list([0x01], members),
            find,
        ],
    ))
}

// ----- catalogs: display order and names (file-metadata.md 1) ---------------

/// The member ids of catalog `catalog` in display order (`[cat, 1, 03, n]`,
/// schema-path.md 5.1), as the big-endian reading of their keys; `None`
/// when the tree has no display order. Each entry is one key segment with
/// its length byte (`01 1d`, `02 80 02`, `03 d0 00 02`).
pub fn display_order(tree: &KeyTree<'_>, catalog: &[u64]) -> Option<Vec<u64>> {
    let path: Vec<u64> = catalog.iter().copied().chain([1, 3]).collect();
    let order = tree.at(&path)?;
    let mut entries: Vec<(u64, u64)> = order
        .children
        .iter()
        .filter_map(|n| {
            let position = n.id()?;
            let value = n.assembled()?;
            let (&len, segment) = value.split_first()?;
            let id = om_hbam::slot::be_uint(segment.get(..usize::from(len))?)?;
            Some((position, id))
        })
        .collect();
    entries.sort_by_key(|(position, _)| *position);
    Some(entries.into_iter().map(|(_, id)| id).collect())
}

/// `items` ordered by the catalog's display order; members it does not
/// list follow in id order.
fn in_display_order<T>(
    tree: &KeyTree<'_>,
    catalog: u64,
    mut items: Vec<T>,
    id: impl Fn(&T) -> u64,
) -> Vec<T> {
    if let Some(order) = display_order(tree, &[catalog]) {
        items.sort_by_key(|item| {
            let id = id(item);
            (
                order.iter().position(|o| *o == id).unwrap_or(usize::MAX),
                id,
            )
        });
    }
    items
}

/// A script catalog member: a script, a folder or a separator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptEntry {
    /// The id FileMaker shows (`Script::id`).
    pub id: u64,
    pub name: String,
    pub kind: om_scripts::ScriptKind,
}

/// The script catalog's members in display order, without decoding their
/// steps. When only the catalog's index `[17, 1]` was read (a hosted file
/// opened on demand, as FileMaker Pro 26 opens one), its member cache
/// `[17, 1, 07, S]` lists them instead ([`indexed_scripts`]).
pub fn scripts(tree: &KeyTree<'_>) -> Vec<ScriptEntry> {
    let Some(members) = tree.at(&[SCRIPTS, 5]) else {
        return indexed_scripts(tree);
    };
    let entries: Vec<(u64, ScriptEntry)> = members
        .children
        .iter()
        .filter(|n| n.id().is_some_and(|id| id != 0))
        .filter_map(|n| {
            let id = om_hbam::serial::decode(n.segment.0)?;
            let name = n
                .get(NAME)
                .and_then(Node::text)
                .and_then(Result::ok)
                .unwrap_or_else(|| format!("Script {id}"));
            // Folders keep an empty step list, so the flags decide first.
            let flags = n.get(0xf0).and_then(Node::uint_value).unwrap_or(0);
            let kind = if flags & 1 != 0 {
                om_scripts::ScriptKind::Folder
            } else if flags & 2 != 0 {
                om_scripts::ScriptKind::FolderEnd
            } else if n.get(4).is_some() {
                om_scripts::ScriptKind::Script
            } else {
                om_scripts::ScriptKind::Separator
            };
            Some((n.id()?, ScriptEntry { id, name, kind }))
        })
        .collect();
    in_display_order(tree, SCRIPTS, entries, |(raw, _)| *raw)
        .into_iter()
        .map(|(_, entry)| entry)
        .collect()
}

/// The script catalog's index entries `[17, 1, 07, S]` in display order:
/// each member's name `10` and flags `f0` (scripts.md, "The catalog index").
/// A folder and a folder's end are told by their flags; every other entry
/// is listed as a script, since a separator differs from a script only by
/// its member having no step list `04`, which the index does not hold [I:
/// how the index marks a separator is not established].
pub fn indexed_scripts(tree: &KeyTree<'_>) -> Vec<ScriptEntry> {
    let Some(entries) = tree.at(&[SCRIPTS, 1, 7]) else {
        return Vec::new();
    };
    let entries: Vec<(u64, ScriptEntry)> = entries
        .children
        .iter()
        .filter(|n| n.id().is_some_and(|id| id != 0))
        .filter_map(|n| {
            let id = om_hbam::serial::decode(n.segment.0)?;
            let name = n
                .get(NAME)
                .and_then(Node::text)
                .and_then(Result::ok)
                .unwrap_or_else(|| format!("Script {id}"));
            let flags = n.get(0xf0).and_then(Node::uint_value).unwrap_or(0);
            let kind = if flags & 1 != 0 {
                om_scripts::ScriptKind::Folder
            } else if flags & 2 != 0 {
                om_scripts::ScriptKind::FolderEnd
            } else {
                om_scripts::ScriptKind::Script
            };
            Some((n.id()?, ScriptEntry { id, name, kind }))
        })
        .collect();
    in_display_order(tree, SCRIPTS, entries, |(raw, _)| *raw)
        .into_iter()
        .map(|(_, entry)| entry)
        .collect()
}

/// Layout catalog flags, preferring the member over the index copy.
pub fn layout_flags(tree: &KeyTree<'_>, id: u64) -> u64 {
    tree.at(&[LAYOUTS, 5, id, 0xf0])
        .and_then(Node::uint_value)
        .or_else(|| {
            tree.at(&[LAYOUTS, 1, 7, id, 0xf0])
                .and_then(Node::uint_value)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::{OwnedNode, SlotReader};

    fn hex(text: &str) -> Vec<u8> {
        let text: String = text.split_whitespace().collect();
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    fn tree(nodes: Vec<OwnedNode>) -> Vec<u8> {
        OwnedNode::list(Vec::new(), nodes).to_wire().unwrap()
    }

    /// A stored page setup `[2, 87]` is read whole, chunked or not, and one
    /// past 1 MiB is refused by its declared or actual size before it is
    /// reassembled: the file's bytes size nothing a host keeps.
    #[test]
    fn a_stored_page_setup_past_one_mebibyte_is_not_read() {
        let stored = |value: OwnedNode| {
            let wire = tree(vec![OwnedNode::list([2], vec![value])]);
            page_format(&KeyTree::from_slots(SlotReader::new(&wire)).unwrap())
        };
        let chunked = |total: u32, pieces: &[usize]| {
            let mut children = vec![OwnedNode::leaf([0], total.to_be_bytes().to_vec())];
            children.extend(
                (pieces.iter().enumerate())
                    .map(|(i, len)| OwnedNode::leaf([i as u8 + 1], vec![7; *len])),
            );
            OwnedNode::list([0x87], children)
        };
        assert_eq!(
            stored(OwnedNode::leaf([0x87], b"Comm".to_vec())),
            Some(b"Comm".to_vec())
        );
        assert_eq!(stored(chunked(6, &[4, 2])), Some(vec![7; 6]));
        // Pieces of at most 64,000 bytes (a wire value's limit) adding up to
        // more than 1 MiB, and declaring their true total.
        let pieces = [64_000; 17];
        assert!(pieces.iter().sum::<usize>() > MAX_PAGE_FORMAT);
        assert_eq!(stored(chunked(64_000 * 17, &pieces)), None);
        // Exactly 1 MiB is read: the header is not counted.
        let mut pieces = vec![64_000; 16];
        pieces.push(MAX_PAGE_FORMAT - 16 * 64_000);
        let whole = stored(chunked(MAX_PAGE_FORMAT as u32, &pieces));
        assert_eq!(whole.map(|bytes| bytes.len()), Some(MAX_PAGE_FORMAT));
        // The declared total alone refuses, whatever pieces follow.
        assert_eq!(stored(chunked(u32::MAX, &[4])), None);
    }

    /// A window's close writes the bytes FileMaker Pro 26.0.3 wrote for the
    /// same state (file-metadata.md 11): over a starter's entry, found set
    /// {2} on record 2 at data version 5 is the two-window copy's entry, and
    /// {1, 3, 4} on record 4 the find-new copy's; over an entry with a kept
    /// sort the set reopens unsorted, FileMaker's `fd` kept beside it; a
    /// member or shape no file has writes nothing.
    #[test]
    fn a_closing_window_writes_the_found_set_as_filemaker_pro_does() {
        use om_hbam::RidList;
        let leaf = |key: u8, value: &str| OwnedNode::leaf([key], hex(value));
        let list = |key: u8, children: Vec<OwnedNode>| OwnedNode::list([key], children);
        let fe = |version: &str| list(0xfe, vec![OwnedNode::leaf([0x80, 0x02], hex(version))]);
        let empty_find = || list(0x02, vec![leaf(0x03, "0100"), list(0x05, Vec::new())]);
        let entry = |state: Vec<OwnedNode>| {
            list(
                0x07,
                vec![leaf(0x00, "0102"), list(0x01, state), empty_find()],
            )
        };
        let shipped = entry(vec![
            leaf(0x03, "0100"),
            list(0x05, Vec::new()),
            fe("00000001"),
            OwnedNode::leaf([0xff], Vec::new()),
        ]);
        let ids = |ids: &[u64]| {
            let mut list = RidList::new();
            for id in ids {
                list.insert(*id);
            }
            Some(list)
        };
        let write = |records, current, data_version| FoundSetWrite {
            records,
            current,
            data_version,
        };
        let written = |existing: &OwnedNode, w: &FoundSetWrite| {
            saved_found_set_written(Some(existing), &[0x80, 0x02], w)
        };
        assert_eq!(
            written(&shipped, &write(ids(&[2]), Some(2), 5)),
            Some(entry(vec![
                leaf(0x03, "0102"),
                list(0x05, vec![OwnedNode::leaf([0x02], Vec::new())]),
                fe("00000005"),
            ]))
        );
        assert_eq!(
            written(&shipped, &write(ids(&[1, 3, 4]), Some(4), 5)),
            Some(entry(vec![
                leaf(0x03, "0104"),
                list(0x05, vec![leaf(0x00, "0000001a")]),
                fe("00000005"),
            ]))
        );
        // No entry yet: the shape every file has.
        assert_eq!(
            saved_found_set_written(None, &[0x80, 0x02], &write(None, None, 1)),
            Some(shipped.clone())
        );
        // A kept sort: unsorted, `0a` and `f0` as they were, no `08`.
        let spec = "00000001 0403d00002 020104 0001 020100 020100 0001 020102 00 00000000";
        let sorted = entry(vec![
            leaf(0x02, "01"),
            leaf(0x03, "0103"),
            list(0x05, Vec::new()),
            list(0x08, vec![leaf(0x00, "0000000000000003")]),
            leaf(0x0a, spec),
            leaf(0xf0, "00000000"),
            fe("00000004"),
            OwnedNode::leaf([0xff], Vec::new()),
        ]);
        assert_eq!(
            written(&sorted, &write(ids(&[1, 3]), Some(3), 4)),
            Some(entry(vec![
                leaf(0x02, "00"),
                leaf(0x03, "0103"),
                list(0x05, vec![leaf(0x00, "0000000a")]),
                leaf(0x0a, spec),
                leaf(0xf0, "00000000"),
                fe("00000004"),
            ]))
        );
        // A sorted order whose key holds its records as a RID list's bitmap
        // word, as FileMaker Pro 26.0.3 wrote it in #642's Tasks copy
        // (records 1-3 under one key): written unsorted like the others.
        let rid_sorted = entry(vec![
            leaf(0x02, "01"),
            leaf(0x03, "0103"),
            list(0x05, Vec::new()),
            list(
                0x08,
                vec![
                    leaf(0x00, "0000000000000003"),
                    list(0x01, vec![list(0x07, vec![leaf(0x00, "0000000e")])]),
                ],
            ),
            leaf(0x0a, spec),
            leaf(0xf0, "00000000"),
            fe("00000007"),
            OwnedNode::leaf([0xff], Vec::new()),
        ]);
        assert_eq!(
            written(&rid_sorted, &write(ids(&[1, 3]), Some(3), 7)),
            Some(entry(vec![
                leaf(0x02, "00"),
                leaf(0x03, "0103"),
                list(0x05, vec![leaf(0x00, "0000000a")]),
                leaf(0x0a, spec),
                leaf(0xf0, "00000000"),
                fe("00000007"),
            ]))
        );
        // FileMaker's `fd` beside the sort (file-metadata.md 11) is kept
        // as it was.
        let mut with_fd = sorted.clone();
        with_fd.children[1]
            .children
            .insert(6, list(0xfd, Vec::new()));
        assert_eq!(
            written(&with_fd, &write(ids(&[1, 3]), Some(3), 4)),
            Some(entry(vec![
                leaf(0x02, "00"),
                leaf(0x03, "0103"),
                list(0x05, vec![leaf(0x00, "0000000a")]),
                leaf(0x0a, spec),
                leaf(0xf0, "00000000"),
                list(0xfd, Vec::new()),
                fe("00000004"),
            ]))
        );
        // Shapes no file has: nothing is written.
        let unknown = entry(vec![leaf(0x03, "0100"), leaf(0x04, "00")]);
        let lone_f0 = entry(vec![leaf(0x03, "0100"), leaf(0xf0, "00000000")]);
        let lone_fd = entry(vec![leaf(0x03, "0100"), list(0xfd, Vec::new())]);
        // `fd` as a leaf, which no file has.
        let mut fd_leaf = sorted.clone();
        fd_leaf.children[1]
            .children
            .insert(6, OwnedNode::leaf([0xfd], Vec::new()));
        let other_table = entry(vec![
            leaf(0x03, "0100"),
            list(0xfe, vec![OwnedNode::leaf([0x80, 0x03], hex("00000001"))]),
        ]);
        let deep_rid = entry(vec![
            leaf(0x03, "0100"),
            list(0x05, vec![list(0x00, vec![leaf(0x01, "00")])]),
        ]);
        let odd_order = entry(vec![
            leaf(0x03, "0100"),
            list(0x05, Vec::new()),
            list(0x08, vec![leaf(0x00, "0000000000000001"), leaf(0x01, "00")]),
        ]);
        for node in [
            unknown,
            lone_f0,
            lone_fd,
            fd_leaf,
            other_table,
            deep_rid,
            odd_order,
        ] {
            assert!(written(&node, &write(None, None, 1)).is_none(), "{node:?}");
        }
    }

    /// The saved found sets FileMaker Pro 26.0.3 wrote into a Tasks
    /// starter at close (file-metadata.md 11), as `[TO, 07, 01]` holds
    /// them; the sorted order `08` and the unknown `f0` and `fe` are left
    /// out, as the reader leaves them.
    #[test]
    fn saved_found_sets_decode_as_filemaker_pro_wrote_them() {
        let decode = |children: Vec<OwnedNode>| {
            let wire = tree(vec![OwnedNode::list([1], children)]);
            let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
            SavedFoundSet::from_node(tree.node(&[&[1]]).unwrap())
        };
        let leaf = |key: u8, value: &str| OwnedNode::leaf([key], hex(value));
        // Tasks after a Quick Find: records 1 and 3 (a bitmap word),
        // record 3 current, unsorted, its last sort by a value list.
        let tasks = decode(vec![
            leaf(0x02, "00"),
            leaf(0x03, "0103"),
            OwnedNode::list([0x05], vec![leaf(0x00, "0000000a")]),
            leaf(
                0x0a,
                "00000001 0403d00002 020104 0001 020100 020100 0001 020102 00 00000000",
            ),
        ])
        .unwrap();
        assert!(!tasks.all && !tasks.sorted);
        assert_eq!(tasks.records.iter().collect::<Vec<_>>(), [1, 3]);
        assert_eq!(tasks.current, Some(3));
        assert_eq!(tasks.sort.unwrap().fields[0].value_list, Some(2));
        // Assignees after a find, sorted ascending by field 0a: record 2,
        // a singleton key.
        let assignees = decode(vec![
            leaf(0x02, "01"),
            leaf(0x03, "0102"),
            OwnedNode::list([0x05], vec![OwnedNode::leaf([0x02], Vec::new())]),
            leaf(
                0x0a,
                "00000001 0403d00004 02010a 0001 020100 020100 0001 00 00 00000000",
            ),
        ])
        .unwrap();
        assert!(assignees.sorted && !assignees.all);
        assert_eq!(assignees.records.iter().collect::<Vec<_>>(), [2]);
        let field = assignees.sort.unwrap().fields[0].field;
        assert_eq!((field.occurrence, field.field), (Some(0xd0_0004), 0x0a));
        // A starter as shipped: every record, none current.
        let shipped = decode(vec![
            leaf(0x03, "0100"),
            OwnedNode::list([0x05], Vec::new()),
            OwnedNode::leaf([0xff], Vec::new()),
        ])
        .unwrap();
        assert!(shipped.all && shipped.current.is_none() && shipped.sort.is_none());
        // Neither every record nor a list, or a malformed member: nothing.
        assert!(decode(vec![leaf(0x03, "0102")]).is_none());
        assert!(decode(vec![
            leaf(0x03, "0201"),
            OwnedNode::leaf([0xff], Vec::new())
        ])
        .is_none());
    }

    #[test]
    fn scripts_tell_folders_and_folder_ends_from_separators_in_display_order() {
        let flags = |v: u64| OwnedNode::leaf([0xf0], v.to_be_bytes().to_vec());
        let name = |n: &str| OwnedNode::leaf([0x10], om_scsu::encode_scsu(n));
        // A folder keeps an empty step list (key 4); its end is `--` with
        // flag 2; a separator has neither.
        let members = vec![
            OwnedNode::list(
                [1],
                vec![OwnedNode::leaf([4], Vec::new()), name("F"), flags(9)],
            ),
            OwnedNode::list(
                [2],
                vec![OwnedNode::leaf([4], vec![0; 28]), name("S"), flags(8)],
            ),
            OwnedNode::list([3], vec![name("-"), flags(0x18)]),
            OwnedNode::list([4], vec![name("--"), flags(2)]),
        ];
        // The display order entries are one key segment with its length.
        let order = OwnedNode::list(
            [3],
            [4u8, 2, 1, 3]
                .iter()
                .enumerate()
                .map(|(i, id)| OwnedNode::leaf(vec![0, 0, 0, i as u8 + 1], vec![1, *id]))
                .collect(),
        );
        let wire = tree(vec![OwnedNode::list(
            [SCRIPTS as u8],
            vec![
                OwnedNode::list([1], vec![order]),
                OwnedNode::list([5], members),
            ],
        )]);
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let kinds: Vec<(u64, om_scripts::ScriptKind)> =
            scripts(&tree).iter().map(|s| (s.id, s.kind)).collect();
        use om_scripts::ScriptKind::*;
        assert_eq!(
            kinds,
            [(4, FolderEnd), (2, Script), (1, Folder), (3, Separator)]
        );
        assert_eq!(display_order(&tree, &[SCRIPTS]), Some(vec![4, 2, 1, 3]));
    }

    #[test]
    fn document_preferences_hold_the_file_triggers() {
        // Audit MBS file (file-metadata.md 3.1): OnFirstWindowOpen script 12,
        // default layout 54, OnWindowClose script 85.
        let audit = hex(
            "00000443 04 01 02010c 00 020136 0001 0000 0001 00000200 00 020155 \
             0000 0000 00 0064 0001 00 00",
        );
        let o = FileOptions::decode(&audit).unwrap();
        assert_eq!(o.options, 0x443);
        assert_eq!(o.first_window_open, Some(12));
        assert_eq!(o.last_window_close, None);
        assert_eq!(o.layout, Some(0x36));
        assert_eq!(o.default_layout(), Some(0x36));
        assert_eq!(o.window_open, None);
        assert_eq!(o.window_close, Some(85));
        assert_eq!(o.trigger(file_event::WINDOW_CLOSE), Some(85));
        assert_eq!(o.window_transaction, None);
        assert_eq!(o.thumbnail_cache_mb, 512);
        // A template's record ends after the custom icon members.
        let template = hex(
            "00000443 01 01 00 00 020103 0001 0000 0001 00000200 00 00 0000 0000 00 03e7 0001 0000",
        );
        let t = FileOptions::decode(&template).unwrap();
        assert_eq!((t.layout, t.icon_type), (Some(3), 999));
        // A script id of 128 or more, and the transaction field name.
        let mut high = audit.clone();
        high.truncate(high.len() - 2);
        high.extend([0x03, 0x02, 0x80, 0x0d]);
        high.extend(om_scsu::encode_counted("AuditJSON"));
        let h = FileOptions::decode(&high).unwrap();
        assert_eq!(h.window_transaction, Some(141));
        assert_eq!(h.window_transaction_field, "AuditJSON");
        // An old record that ends after the default layout keeps defaults.
        let old = FileOptions::decode(&audit[..13]).unwrap();
        assert_eq!((old.print_type, old.scale_factor), (1, 1));
        assert!(FileOptions::decode(&audit[..9]).is_none());
        let off = FileOptions {
            options: 0x403,
            ..o
        };
        assert_eq!(off.default_layout(), None);
    }

    #[test]
    fn relationships_read_both_orders_and_the_options() {
        // fmSyntaxColorizer relationship 19 (file-metadata.md 4.1).
        let definition = hex("0403d0001e 0403d0001b 0000 0000 0000 0002 \
             0403d0001e 02010e 0001 020100 020100 0001 020108 00 00000000 \
             0403d0001e 020102 0001 020100 020100 0001 00 00 00000000 000000a2");
        let predicate = hex("00 020105 020103");
        let r = Relationship::decode(19, &definition, &[&predicate]).unwrap();
        assert_eq!((r.left, r.right), (0xd0_001e, 0xd0_001b));
        assert!(r.left_to_right.fields.is_empty());
        assert_eq!(r.right_to_left.fields.len(), 2);
        assert_eq!(r.right_to_left.fields[0].value_list, Some(8));
        assert_eq!(r.right_to_left.fields[1].field.field, 2);
        assert_eq!(r.options, 0xa2);
        // Sorted from the right (0x80), not from the left.
        assert!(r.order_of(r.left).is_some());
        assert!(r.order_of(r.right).is_none());
        assert_eq!(
            r.predicates,
            [JoinPredicate {
                op: JoinOp::Equal,
                left: 5,
                right: 3
            }]
        );
        assert!(Relationship::decode(19, &definition[..20], &[]).is_none());
        // Operator 7 fails the read.
        assert!(Relationship::decode(19, &definition, &[&hex("07 020105 020103")]).is_none());
        for code in 0..=6 {
            assert_eq!(JoinOp::from_code(code).unwrap().code(), code);
        }
    }

    #[test]
    fn summary_definitions_read_every_kind_and_option() {
        // Tasks template "Number of Topics": count of field 1, stale bytes
        // after the keys.
        let count = SummaryDef::decode(&[
            0x03, 0x00, 0x02, 0x01, 0x00, 0x00, 0xfe, 0x7f, 0x02, 0x01, 0x01, 0xbe, 0xbf,
        ]);
        assert_eq!(count.kind, SummaryKind::Count);
        assert_eq!(count.field.unwrap().field, 1);
        assert_eq!(count.field.unwrap().occurrence, None);
        assert_eq!(count.additional, None);
        // Weighted average of field 5 by field 7 of occurrence d0 00 02.
        let mut weighted = [0u8; 30];
        weighted[..2].copy_from_slice(&[2, 0x01]);
        weighted[2..5].copy_from_slice(&[2, 1, 0]);
        weighted[8..11].copy_from_slice(&[2, 1, 5]);
        weighted[14..16].copy_from_slice(&[0, 1]);
        weighted[16..21].copy_from_slice(&[4, 3, 0xd0, 0, 2]);
        weighted[22..25].copy_from_slice(&[2, 1, 7]);
        weighted[28..30].copy_from_slice(&[0, 1]);
        let w = SummaryDef::decode(&weighted);
        assert_eq!(w.kind, SummaryKind::Average);
        assert!(w.has(summary_flag::OPTION));
        assert_eq!(
            w.additional,
            Some(SummaryField {
                occurrence: Some(0xd0_0002),
                field: 7,
                repetition: 1
            })
        );
        assert_eq!(SummaryDef::decode(&[]).kind, SummaryKind::Total);
        assert_eq!(SummaryDef::decode(&[]).field, None);
        for (code, kind) in [
            (4, SummaryKind::Minimum),
            (5, SummaryKind::Maximum),
            (6, SummaryKind::StdDeviation),
            (7, SummaryKind::Fraction),
            (9, SummaryKind::List),
            (8, SummaryKind::Other(8)),
        ] {
            assert_eq!(SummaryDef::decode(&[code, 0x14]).kind, kind);
        }
        let running = SummaryDef::decode(&[1, 0x14]);
        assert!(running.has(summary_flag::RUNNING) && running.has(summary_flag::RESTART));
    }

    #[test]
    fn value_lists_read_custom_field_and_related_sources() {
        let name = |text: &str| OwnedNode::leaf([0x10], om_scsu::encode_scsu(text));
        // Custom values without a definition, a 12-byte chunked text header.
        let text = om_scsu::encode_scsu("Alpha\r-\rBeta\rGamma");
        let custom = OwnedNode::list(
            [1],
            vec![
                OwnedNode::list(
                    [3],
                    vec![
                        OwnedNode::leaf(
                            [0],
                            [text.len() as u32, 18, 4]
                                .iter()
                                .flat_map(|w| w.to_be_bytes())
                                .collect::<Vec<u8>>(),
                        ),
                        OwnedNode::leaf([1], text.clone()),
                    ],
                ),
                name("Status"),
            ],
        );
        // From field 2 of d0 00 03, second field 4, related from d0 00 05,
        // re-sorted by German.
        let mut definition =
            hex("01 0007 0403d00003 020102 0001 0403d00003 020104 0001 0403d00005 00 00 01");
        definition.push(0x7a);
        definition.extend(hex(
            "0000 0001 020100 020100 0001 020100 020100 0001 00 00 00000019",
        ));
        let field = OwnedNode::list([2], vec![OwnedNode::leaf([2], definition), name("Tasks")]);
        let wire = tree(vec![OwnedNode::list(
            [0x21],
            vec![
                OwnedNode::list(
                    [1],
                    vec![OwnedNode::list(
                        [3],
                        // Display order entries: one key segment with its
                        // length, as files store them (`01 02`).
                        vec![
                            OwnedNode::leaf([0, 0, 0, 1], [1, 2]),
                            OwnedNode::leaf([0, 0, 0, 2], [1, 1]),
                        ],
                    )],
                ),
                OwnedNode::list([5], vec![custom, field]),
            ],
        )]);
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let lists = value_lists(&tree);
        assert_eq!(lists.iter().map(|l| l.id).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(
            lists[1].source,
            ValueSource::Custom(vec![
                "Alpha".into(),
                "-".into(),
                "Beta".into(),
                "Gamma".into()
            ])
        );
        let ValueSource::Field {
            primary,
            secondary,
            start,
        } = &lists[0].source
        else {
            panic!("{:?}", lists[0].source);
        };
        assert_eq!((primary.occurrence, primary.field), (Some(0xd0_0003), 2));
        assert_eq!(secondary.unwrap().field, 4);
        assert_eq!(*start, Some(0xd0_0005));
        assert_eq!(lists[0].options, 7);
        assert_eq!(lists[0].sort_locale, 25);
        assert_eq!(lists[0].separator, " ");
    }
}
