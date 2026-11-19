//! The calculation value, `Draco::DBData` (spec `docs/re/calc-values.md` 1),
//! and its conversions (4.4, 6.3, 6.5).

use crate::datetime::{Date, Time, TimeStyle, Timestamp};
use crate::error::CalcError;
use crate::eval::CalcHost;
use crate::locale::Locale;
use crate::number::{Number, DEFAULT_PRECISION};
use crate::text::Text;

/// The type of a value (`DBDataType`; booleans are numbers, JSON is text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
    Container,
}

/// A stream type, a four-character code (`QuadChar`, `containers.md` 2.3):
/// `FILE`, `JPEG`, `PNGf`, `SIZE`, `DPI_`, ...
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamType(pub [u8; 4]);

impl StreamType {
    /// Any file, stored as is: what decoding functions produce.
    pub const FILE: StreamType = StreamType(*b"FILE");
    /// Native compressed file data, stored as a gzip member.
    pub const ZLIB: StreamType = StreamType(*b"ZLIB");

    /// The metadata types (`QuadChar::IsSupportingType`, `containers.md`
    /// 2.3) and the main-stream marker; every other type holds data.
    pub fn is_supporting(self) -> bool {
        matches!(
            &self.0,
            b"SIZE"
                | b"DPI_"
                | b"ROT8"
                | b"FRM#"
                | b"FORK"
                | b"FNAM"
                | b"SIGN"
                | b"REMO"
                | b"ENCR"
                | b"GLPH"
                | b"MAIN"
        )
    }
}

impl std::fmt::Debug for StreamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(&self.0))
    }
}

/// Container data as calculations see it (`DBBinaryData`, `containers.md`
/// 2): its reference text and the streams the calculation has in hand.
///
/// The reference text is what `GetAsText`, `&` and the text functions see:
/// the file name of embedded data (`a.png`), the path list of a reference.
/// Storage keeps the name in the `FNAM` stream; here it is the text, so
/// `FNAM` is never among `streams`. Streams are the data (`FILE`, `JPEG`,
/// ...) and its metadata (`SIZE`, `DPI_`, ...), the main data stream first.
/// A container the host knows only by reference, whose data it fetches on
/// demand, has no streams: one a host read from a container field names
/// its library member ([`Container::library`]) and has the file name as
/// its text, and the host loads its streams when a function reads them
/// ([`crate::eval::CalcHost::container_data`]).
///
/// Two containers are equal when their names and streams are (`=` on
/// containers compares the data and the name, spec 6.2); two library
/// members when their MD5s are, which cover both (containers.md 2.2).
#[derive(Clone, Debug, Default)]
pub struct Container {
    pub text: Text,
    pub streams: Vec<(StreamType, Vec<u8>)>,
    /// Logical bytes of a stored compressed main stream. `streams` keeps
    /// the original representation for storage MD5, internal size and
    /// copies. Hosts decode this once when loading a ZLIB body; clones
    /// share the expanded bytes. Absent for uncompressed streams.
    pub decompressed: Option<std::sync::Arc<Vec<u8>>>,
    /// For data a host read from a container field's storage: whether it
    /// is intact, `VerifyContainer`'s answer (its streams match their
    /// stored MD5, containers.md 2.2). `None` for data a calculation made,
    /// which has no stored data to check.
    pub verified: Option<bool>,
    /// The library member a host read it from (containers.md 1.4), with
    /// or without its streams loaded; `None` for data a calculation made.
    pub library: Option<std::sync::Arc<LibraryRef>>,
    /// The type of the path its `FNAM` names it with (`image`, `movie`,
    /// `file`; containers.md 2.4) when its streams are typed as stored:
    /// data a host read from storage or a step typed (Insert from URL,
    /// the Insert steps). `None` for data a calculation decoded, which
    /// FileMaker types by its name when a field stores it
    /// ([`crate::functions::stored_streams`]).
    pub path_type: Option<&'static str>,
}

/// The actual versioned field read that produced a container. Callers must
/// revalidate it against their original row and owning permissions before copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContainerSource {
    pub table: u64,
    pub record: u64,
    pub field: u64,
    pub repetition: u32,
    pub version: u64,
}

/// A member of a table's library of container data (containers.md 1): the
/// base table, the member key (one serial segment) and the MD5 of its data
/// (2.2), which identifies the data and its name.
#[derive(Clone, Debug)]
pub struct LibraryRef {
    /// The owning desktop connection; absent for file-local server values.
    pub owner: Option<[u8; 16]>,
    pub table: u64,
    pub member: Vec<u8>,
    pub md5: [u8; 16],
    /// Actual owning record/cell read, not acquisition authority. Kept behind
    /// the existing shared reference so plain calculation values stay small.
    pub source: Option<ContainerSource>,
}

// Source provenance describes a read, not the identity of library data.
impl PartialEq for LibraryRef {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.table == other.table
            && self.member == other.member
            && self.md5 == other.md5
    }
}
impl Eq for LibraryRef {}
impl std::hash::Hash for LibraryRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.owner, state);
        std::hash::Hash::hash(&self.table, state);
        std::hash::Hash::hash(&self.member, state);
        std::hash::Hash::hash(&self.md5, state);
    }
}

impl PartialEq for Container {
    fn eq(&self, other: &Container) -> bool {
        match (&self.library, &other.library) {
            (Some(a), Some(b)) => a.md5 == b.md5,
            _ => self.text == other.text && self.streams == other.streams,
        }
    }
}

impl Eq for Container {}

impl Container {
    /// A file named `name` holding `data`, as `Base64Decode`, `HexDecode`
    /// and the crypt functions create it.
    pub fn file(name: impl Into<String>, data: Vec<u8>) -> Container {
        Container {
            text: Text::plain(name),
            streams: vec![(StreamType::FILE, data)],
            decompressed: None,
            verified: None,
            library: None,
            path_type: None,
        }
    }

    /// The `FNAM` path type of a path type text, as stored (2.4).
    pub fn known_path_type(text: &str) -> Option<&'static str> {
        ["image", "movie", "file"]
            .into_iter()
            .find(|t| t.eq_ignore_ascii_case(text))
    }

    /// Whether only its reference is in hand: a library member whose
    /// streams the host has not loaded.
    pub fn is_unloaded(&self) -> bool {
        self.library.is_some() && self.streams.is_empty()
    }

    /// The main data stream (`DBBinaryData::GetMasterSize` measures it):
    /// the first stream that is not metadata.
    pub fn main_stream(&self) -> Option<&(StreamType, Vec<u8>)> {
        self.streams.iter().find(|(kind, _)| !kind.is_supporting())
    }

    /// The main stream as stored, including compression, if any. Storage
    /// attributes and copies use this representation.
    pub fn stored_data(&self) -> &[u8] {
        self.main_stream().map_or(&[], |(_, data)| data.as_slice())
    }

    /// The logical file bytes; empty without a main stream. Encoding
    /// functions and exports read the decompressed data of a ZLIB stream.
    pub fn data(&self) -> &[u8] {
        self.decompressed
            .as_ref()
            .map_or_else(|| self.stored_data(), |bytes| bytes.as_slice())
    }

    /// Stream buffers held by a loaded value, including the expanded
    /// compressed body, for the hosts' container cache budgets.
    pub fn retained_data_size(&self) -> usize {
        self.streams.iter().map(|(_, b)| b.len()).sum::<usize>()
            + self.decompressed.as_ref().map_or(0, |b| b.len())
    }

    /// The byte size of the main stream (`Length` of container data).
    pub fn size(&self) -> usize {
        self.data().len()
    }

    /// No reference text and no data.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.streams.is_empty() && self.library.is_none()
    }
}

/// A calculation value.
#[derive(Clone, Debug)]
pub enum Value {
    /// Text, possibly styled; empty text is the empty text value.
    Text(Text),
    /// A number; [`Number`] has empty and `?` states of its own. `?` (the
    /// invalid result, spec 7.2) is `Number(Number::nan())`.
    Number(Number),
    /// A number shown as the text it was entered or declared with: a number
    /// field holding `12abc`, a calculation whose declared result type
    /// converted text (spec 6.5). Arithmetic sees the number, text
    /// operations the text.
    NumberText(Number, Text),
    Date(Date),
    /// A stored date with its original display text and character styles.
    /// Date operations use the parsed date; text reads retain the stored
    /// presentation. Explicit casts and arithmetic produce ordinary dates.
    DateText(Date, Text),
    Time(Time),
    Timestamp(Timestamp),
    Container(Container),
    /// An empty date, time, timestamp or container (`Timestamp(date; "")`,
    /// `GetAsTime(" ")`); it keeps its type in arithmetic (spec 6.4).
    Empty(ValueType),
}

/// What conversions and operators need besides the values: the file's
/// locale, today's date (for years text leaves out and the two-digit-year
/// window, spec 4.6), the evaluation precision (16, or what `SetPrecision`
/// set, spec 2.6), the bytes a text value may take and whether the
/// evaluation must stop.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    pub locale: &'a Locale,
    pub today: Date,
    pub precision: u16,
    /// [`crate::eval::CalcLimits::text`] of the evaluation.
    pub text_limit: usize,
    pub interrupt: Interrupt<'a>,
}

impl<'a> Ctx<'a> {
    /// The context of an evaluation at the default precision and limits,
    /// which nothing interrupts.
    pub fn new(locale: &'a Locale, today: Date) -> Ctx<'a> {
        Ctx {
            locale,
            today,
            precision: DEFAULT_PRECISION,
            text_limit: crate::eval::CalcLimits::DEFAULT.text,
            interrupt: Interrupt::NONE,
        }
    }

    /// Error 7 when a text value of `bytes` ([`Text::size`]) would be past
    /// the text limit: a function asks before it builds one.
    pub fn check_size(&self, bytes: usize) -> Result<(), CalcError> {
        if bytes > self.text_limit {
            Err(CalcError::OUT_OF_MEMORY)
        } else {
            Ok(())
        }
    }

    /// The same context at another evaluation precision.
    pub fn with_precision(self, precision: u16) -> Ctx<'a> {
        Ctx { precision, ..self }
    }
}

/// Whether the evaluation a function runs in must stop, as its host says
/// ([`CalcHost::interrupted`]). A function whose work is not bounded by a
/// small multiple of its input counts that work ([`Work`]) and fails with
/// [`CalcError::CANCELED`] when asked to stop; the evaluator then ends the
/// whole evaluation.
#[derive(Clone, Copy, Default)]
pub struct Interrupt<'a>(Option<&'a dyn CalcHost>);

impl<'a> Interrupt<'a> {
    /// Nothing interrupts: functions called outside an evaluation.
    pub const NONE: Interrupt<'static> = Interrupt(None);

    /// The evaluation's host decides.
    pub(crate) fn of(host: &'a dyn CalcHost) -> Interrupt<'a> {
        Interrupt(Some(host))
    }

    /// [`CalcError::CANCELED`] once the host asks the evaluation to stop.
    pub fn check(self) -> Result<(), CalcError> {
        match self.0 {
            Some(host) if host.interrupted() => Err(CalcError::CANCELED),
            _ => Ok(()),
        }
    }
}

impl std::fmt::Debug for Interrupt<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Interrupt(host)"
        } else {
            "Interrupt(none)"
        })
    }
}

/// A function's work, counted in units of about the cost of comparing a
/// character, with the host asked whether to stop every [`Work::CHECK`]
/// units ([`Interrupt::check`]): about a millisecond apart, so that the
/// question costs nothing measurable. The default is never interrupted.
#[derive(Default)]
pub struct Work<'a> {
    interrupt: Interrupt<'a>,
    units: usize,
}

impl<'a> Work<'a> {
    /// Units between two questions.
    pub const CHECK: usize = 1 << 20;

    pub fn new(cx: &Ctx<'a>) -> Work<'a> {
        Work {
            interrupt: cx.interrupt,
            units: 0,
        }
    }

    /// Count `units` more; [`CalcError::CANCELED`] when the host asks to
    /// stop.
    pub fn add(&mut self, units: usize) -> Result<(), CalcError> {
        self.units = self.units.saturating_add(units);
        if self.units >= Self::CHECK {
            self.units = 0;
            self.interrupt.check()?;
        }
        Ok(())
    }
}

impl Value {
    /// The `?` result.
    pub fn invalid() -> Value {
        Value::Number(Number::nan())
    }

    /// Empty text.
    pub fn empty_text() -> Value {
        Value::Text(Text::new())
    }

    /// The empty value of a type.
    pub fn empty(ty: ValueType) -> Value {
        match ty {
            ValueType::Text => Value::empty_text(),
            ValueType::Number => Value::Number(Number::empty()),
            other => Value::Empty(other),
        }
    }

    pub fn text(s: impl Into<String>) -> Value {
        Value::Text(Text::plain(s))
    }

    pub fn number(n: i64) -> Value {
        Value::Number(Number::from_i64(n))
    }

    /// 0 or 1.
    pub fn boolean(b: bool) -> Value {
        Value::Number(Number::from_bool(b))
    }

    pub fn value_type(&self) -> ValueType {
        match self {
            Value::Text(_) => ValueType::Text,
            Value::Number(_) | Value::NumberText(..) => ValueType::Number,
            Value::Date(_) | Value::DateText(..) => ValueType::Date,
            Value::Time(_) => ValueType::Time,
            Value::Timestamp(_) => ValueType::Timestamp,
            Value::Container(_) => ValueType::Container,
            Value::Empty(ty) => *ty,
        }
    }

    /// `IsEmpty`: empty text, an empty number, an empty date/time/timestamp.
    /// `?` is not empty.
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Text(t) | Value::NumberText(_, t) => t.is_empty(),
            Value::Number(n) => n.is_empty(),
            Value::Container(c) => c.is_empty(),
            Value::Empty(_) => true,
            Value::Date(_) | Value::DateText(..) | Value::Time(_) | Value::Timestamp(_) => false,
        }
    }

    /// Whether this is the `?` result.
    pub fn is_invalid(&self) -> bool {
        matches!(self, Value::Number(n) if n.is_nan())
    }

    /// The text form (spec 2.7, 4.5): what `&`, text functions and text
    /// comparison see. Styles are kept for text.
    pub fn to_text(&self, cx: &Ctx) -> Text {
        match self {
            // The text form is plain text: a parsed-JSON mark stays with the
            // value, not its text (`JSONParsedState ( GetAsText ( JSONParse
            // ( "[1]" ) ) )` = 0).
            Value::Text(t) | Value::NumberText(_, t) | Value::DateText(_, t) => {
                t.clone().with_json_state(0)
            }
            Value::Number(n) => Text::plain(n.to_text_with(cx.locale.decimal_separator)),
            Value::Date(d) => Text::plain(d.format(cx.locale)),
            Value::Time(t) => Text::plain(t.format(cx.locale)),
            Value::Timestamp(t) => Text::plain(t.format(cx.locale)),
            Value::Container(c) => c.text.clone(),
            Value::Empty(_) => Text::new(),
        }
    }

    /// The number form (spec 2.8, 4.4): text through the lenient parser,
    /// dates as day numbers, times and timestamps as seconds.
    pub fn to_number(&self, cx: &Ctx) -> Number {
        match self {
            Value::Text(t) => parse_number(t.as_str(), cx.locale),
            Value::Number(n) | Value::NumberText(n, _) => n.clone(),
            Value::Date(d) | Value::DateText(d, _) => d.to_number(),
            Value::Time(t) => t.to_number(),
            Value::Timestamp(t) => t.to_number(),
            // Container data is 0 as a number, whatever its name (`GetAsNumber
            // ( HexDecode ( "3132" ; "99" ) )` = 0); a bare reference reads
            // its text.
            Value::Container(c) if !c.streams.is_empty() || c.library.is_some() => Number::zero(),
            Value::Container(c) => parse_number(c.text.as_str(), cx.locale),
            Value::Empty(_) => Number::empty(),
        }
    }

    /// The number an operator or numeric function computes with: the number
    /// form, with a non-empty value whose text has no digits counting as 0
    /// (`"abc" + 1` = 1, `-"abc"` = 0, `Abs("abc")` = 0; spec 6.4). Callers
    /// check [`Value::is_empty`] first where empty input gives empty.
    pub fn numeric_operand(&self, cx: &Ctx) -> Number {
        let n = self.to_number(cx);
        if n.is_empty() && !self.is_empty() {
            Number::zero()
        } else {
            n
        }
    }

    /// `GetAsDate` (spec 4.4, 4.6): `Ok(None)` for an empty value, error 500
    /// for anything that is not a date (a time, a number outside
    /// 1..=1460970 after truncation, unparsable text).
    pub fn to_date(&self, cx: &Ctx) -> Result<Option<Date>, CalcError> {
        match self {
            Value::Text(t) => Date::parse(t.as_str(), cx.locale, cx.today),
            Value::Container(c) => Date::parse(c.text.as_str(), cx.locale, cx.today),
            Value::Number(n) | Value::NumberText(n, _) => {
                if n.is_empty() {
                    Ok(None)
                } else {
                    Date::from_number(n)
                        .map(Some)
                        .ok_or(CalcError::INVALID_DATE)
                }
            }
            Value::Date(d) | Value::DateText(d, _) => Ok(Some(*d)),
            Value::Time(_) => Err(CalcError::INVALID_DATE),
            Value::Timestamp(t) => Ok(Some(t.date())),
            Value::Empty(_) => Ok(None),
        }
    }

    /// `GetAsTime` (spec 4.4, 4.6): numbers are seconds, a date its elapsed
    /// seconds since 1/1/0001, a timestamp its time of day; error 501 for
    /// unparsable text.
    pub fn to_time(&self, cx: &Ctx) -> Result<Option<Time>, CalcError> {
        match self {
            Value::Text(t) => Time::parse(t.as_str(), cx.locale, cx.today),
            Value::Container(c) => Time::parse(c.text.as_str(), cx.locale, cx.today),
            Value::Number(n) | Value::NumberText(n, _) => {
                if n.is_empty() {
                    Ok(None)
                } else {
                    Time::from_seconds(n)
                        .map(Some)
                        .ok_or(CalcError::INVALID_TIME)
                }
            }
            Value::Date(d) | Value::DateText(d, _) => {
                Ok(Time::from_seconds(&d.to_timestamp().to_number()))
            }
            Value::Time(t) => Ok(Some(*t)),
            Value::Timestamp(t) => Ok(Some(t.time_of_day())),
            Value::Empty(_) => Ok(None),
        }
    }

    /// `GetAsTimestamp` (spec 4.4, 4.6): numbers are seconds since
    /// 1/1/0001, a date its midnight, a time 1/1/0001 plus the time; error
    /// 500 out of range or for unparsable text.
    pub fn to_timestamp(&self, cx: &Ctx) -> Result<Option<Timestamp>, CalcError> {
        match self {
            Value::Text(t) => Timestamp::parse(t.as_str(), cx.locale, cx.today),
            Value::Container(c) => Timestamp::parse(c.text.as_str(), cx.locale, cx.today),
            Value::Number(n) | Value::NumberText(n, _) => {
                if n.is_empty() {
                    Ok(None)
                } else {
                    Timestamp::from_seconds(n)
                        .map(Some)
                        .ok_or(CalcError::INVALID_DATE)
                }
            }
            Value::Date(d) | Value::DateText(d, _) => Ok(Some(d.to_timestamp())),
            Value::Time(t) => t
                .to_timestamp(TimeStyle::TimeOfDay)
                .map(Some)
                .ok_or(CalcError::INVALID_DATE),
            Value::Timestamp(t) => Ok(Some(*t)),
            Value::Empty(_) => Ok(None),
        }
    }

    /// Truthiness for `If`, `Case`, `and`, `or`, `not`, `While` (spec 2.9):
    /// a non-zero number with every held digit; text through the lenient
    /// parser (`"a1"` is true, `"abc"` false, `"?"` true); dates always;
    /// times and timestamps unless zero.
    pub fn is_true(&self, cx: &Ctx) -> bool {
        match self {
            Value::Date(_) | Value::DateText(..) => true,
            Value::Time(t) => t.micros() != 0,
            Value::Timestamp(t) => t.micros() != 0,
            other => other.to_number(cx).is_true(),
        }
    }

    /// The conversion a calculation's declared result type applies to the
    /// final value (`DBData::ConvertData`, spec 6.5). It reports no error:
    /// what cannot convert becomes `?`. A number result keeps text it was
    /// given (`12abc`); a time converted to a timestamp keeps its style
    /// (`1/1/0001 1:00:00`).
    pub fn convert_declared(&self, ty: ValueType, cx: &Ctx) -> Value {
        if self.is_invalid() {
            return self.clone();
        }
        let or_invalid = |v: Option<Value>| v.unwrap_or_else(Value::invalid);
        match ty {
            ValueType::Text => Value::Text(self.to_text(cx)),
            ValueType::Number => match self {
                Value::Text(t) | Value::Container(Container { text: t, .. }) => {
                    if t.is_empty() {
                        Value::Number(Number::empty())
                    } else {
                        Value::NumberText(parse_number(t.as_str(), cx.locale), t.clone())
                    }
                }
                Value::Number(_) | Value::NumberText(..) => self.clone(),
                Value::Empty(_) => Value::Number(Number::empty()),
                other => Value::Number(other.to_number(cx)),
            },
            ValueType::Date if matches!(self, Value::DateText(..)) => self.clone(),
            ValueType::Date => match self.to_date(cx) {
                Ok(Some(d)) => Value::Date(d),
                Ok(None) => Value::empty(ty),
                Err(_) => Value::invalid(),
            },
            ValueType::Time => match self.to_time(cx) {
                Ok(Some(t)) => Value::Time(t),
                Ok(None) => Value::empty(ty),
                Err(_) => Value::invalid(),
            },
            ValueType::Timestamp => match self {
                Value::Time(t) => or_invalid(t.to_timestamp(t.style()).map(Value::Timestamp)),
                _ => match self.to_timestamp(cx) {
                    Ok(Some(t)) => Value::Timestamp(t),
                    Ok(None) => Value::empty(ty),
                    Err(_) => Value::invalid(),
                },
            },
            ValueType::Container => self.clone(),
        }
    }
}

/// Text to number with the locale's separators (spec 2.8).
pub fn parse_number(text: &str, locale: &Locale) -> Number {
    Number::parse_text(text, locale.decimal_separator, locale.thousands_separator)
}

impl From<Text> for Value {
    fn from(t: Text) -> Value {
        Value::Text(t)
    }
}

impl From<Number> for Value {
    fn from(n: Number) -> Value {
        Value::Number(n)
    }
}

impl From<Date> for Value {
    fn from(d: Date) -> Value {
        Value::Date(d)
    }
}

impl From<Time> for Value {
    fn from(t: Time) -> Value {
        Value::Time(t)
    }
}

impl From<Timestamp> for Value {
    fn from(t: Timestamp) -> Value {
        Value::Timestamp(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(locale: &Locale) -> Ctx<'_> {
        Ctx::new(locale, Date::from_ymd(2026, 10, 28).unwrap())
    }

    #[test]
    fn conversions() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let date = Value::Date(Date::from_ymd(2001, 1, 1).unwrap());
        assert_eq!(date.to_text(&cx).as_str(), "1/1/2001");
        assert_eq!(date.to_number(&cx).to_text_with('.'), "730486");
        let as_time = date.to_time(&cx).unwrap().unwrap();
        assert_eq!(as_time.format(&en), "17531640:00:00");
        let n = Value::Number(Number::from_i64(0));
        assert!(n.to_date(&cx).is_err());
        assert_eq!(
            n.to_timestamp(&cx).unwrap().unwrap().format(&en),
            "1/1/0001 12:00 AM"
        );
        assert!(Value::text("abc").to_date(&cx).is_err());
        assert!(Value::text(" ").to_time(&cx).unwrap().is_none());
    }

    #[test]
    fn truthiness() {
        let en = Locale::en_us();
        let cx = cx(&en);
        for (text, truth) in [
            ("a1", true),
            ("abc", false),
            ("0.0", false),
            ("?", true),
            (" ", false),
            ("e5", true),
        ] {
            assert_eq!(Value::text(text).is_true(&cx), truth, "{text}");
        }
        assert!(Value::Date(Date::from_ymd(1, 1, 1).unwrap()).is_true(&cx));
        assert!(Value::invalid().is_true(&cx));
        assert!(!Value::empty(ValueType::Timestamp).is_true(&cx));
    }

    #[test]
    fn declared_results() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let show =
            |v: &Value, ty: ValueType| v.convert_declared(ty, &cx).to_text(&cx).into_string();
        let date = Value::Date(Date::from_ymd(2001, 1, 1).unwrap());
        assert_eq!(show(&date, ValueType::Timestamp), "1/1/2001 12:00 AM");
        let hour = Value::Time(Time::from_seconds(&Number::from_i64(3600)).unwrap());
        assert_eq!(show(&hour, ValueType::Timestamp), "1/1/0001 1:00:00");
        assert_eq!(show(&hour, ValueType::Date), "?");
        assert_eq!(show(&Value::text("12abc"), ValueType::Number), "12abc");
        assert_eq!(show(&Value::text("90"), ValueType::Time), "90:00:00");
        assert_eq!(show(&Value::number(90), ValueType::Time), "0:01:30");
        assert_eq!(show(&Value::text("730486"), ValueType::Date), "?");
    }
}
