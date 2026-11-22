//! Calculation text: a string with optional character style runs (spec
//! `docs/re/calc-values.md` 5), its characters (legacy grapheme clusters),
//! value lists, collation, and the character rules of the text functions
//! (case, words, sorting, CSS).
//!
//! Text is held as UTF-8; FileMaker's own text is UTF-16, which matters only
//! for `Code` (UTF-16 units, the function library's business) and for lone
//! surrogates, which UTF-8 cannot hold: each is held as a stand-in
//! ([`lone_surrogate`]). `¶` in formulas is U+000D.

pub mod case;
mod cjk;
mod collate;
mod grapheme;
pub mod markup;
pub mod sorting;
pub mod style_ops;
mod tables;
mod weights;
pub mod words;

pub use collate::{extreme_cmp, text_cmp, text_eq, text_eq_key};
pub use grapheme::{grapheme_count, graphemes};

use std::ops::Range;

/// The return character `¶` stands for.
pub const RETURN: char = '\r';

/// Where the stand-ins of lone surrogates start: U+D800..U+DFFF are held as
/// U+10F800..U+10FFFF (the end of Supplementary Private Use Area-B).
const LONE_SURROGATES: u32 = 0x10F800;

/// The stand-in of a lone surrogate. FileMaker's UTF-16 text holds one as
/// a code unit (`Char ( 55357 )`): `Code` reports it, it counts as one
/// character, and a high one followed by a low one is the character they
/// encode (`Char ( 55357 ) & Char ( 56832 )` is 😀). UTF-8 cannot hold it,
/// so om-calc holds U+10F800 + (unit - 0xD800) instead and joins such pairs
/// whenever a text is built ([`Text`]). `None` for a unit that is not a
/// surrogate.
pub fn lone_surrogate(unit: u16) -> Option<char> {
    (0xD800..=0xDFFF)
        .contains(&unit)
        .then(|| char::from_u32(LONE_SURROGATES + u32::from(unit - 0xD800)))
        .flatten()
}

/// The lone surrogate `c` stands for, if it is a stand-in.
pub fn as_lone_surrogate(c: char) -> Option<u16> {
    let offset = u32::from(c).checked_sub(LONE_SURROGATES)?;
    u16::try_from(0xD800 + offset).ok()
}

/// The UTF-16 units of `text` as FileMaker holds it: stand-ins are their
/// lone surrogates.
pub fn utf16_units(text: &str) -> impl Iterator<Item = u16> + '_ {
    text.chars().flat_map(|c| {
        let mut buf = [0; 2];
        match as_lone_surrogate(c) {
            Some(unit) => vec![unit],
            None => c.encode_utf16(&mut buf).to_vec(),
        }
    })
}

/// The style of a run of characters (`Text::mCharacterStyleBuffer`, spec
/// 5.2). Attributes left `None` (and flags 0) are the field's or layout's
/// defaults. The flag bits are the text formatting functions' style
/// constants (`Bold`, `Italic`, ...), which that library defines; see
/// [`style_ops`] for their layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct CharStyle {
    pub font: Option<String>,
    pub size: Option<u16>,
    /// 0xRRGGBB.
    pub color: Option<u32>,
    pub flags: u32,
}

impl CharStyle {
    pub fn is_plain(&self) -> bool {
        *self == CharStyle::default()
    }

    /// `top` laid over `self`, as a replacement takes the style where it
    /// goes (spec 5.2): the attributes `top` sets win; its styles are added
    /// the way `TextStyleAdd` adds them (each style group `top` turns on
    /// replaces `self`'s, the high half of `flags` being the styles set to
    /// normal), so bold under italic is bold italic and `Plain` over bold is
    /// plain.
    pub fn overlay(&self, top: &CharStyle) -> CharStyle {
        const GROUPS: [u32; 4] = [16 | 32, 1024 | 64 | 128, 4 | 8, 8192 | 16384];
        let (top_on, top_normal) = (top.flags & 0xFFFF, top.flags >> 16);
        let (mut on, mut normal) = (self.flags & 0xFFFF, self.flags >> 16);
        for group in GROUPS {
            if top_on & group != 0 {
                on &= !group;
                normal &= !group;
            }
        }
        on = (on | top_on) & !top_normal;
        normal = (normal & !top_on) | top_normal;
        CharStyle {
            font: top.font.clone().or_else(|| self.font.clone()),
            size: top.size.or(self.size),
            color: top.color.or(self.color),
            flags: on | normal << 16,
        }
    }
}

/// Text with style runs (`Text::mCharacterStyleBuffer`, spec 5.2 and the
/// value-core oracle answers in calc-values.md 13).
///
/// Runs cover the whole text in order with non-empty byte ranges on
/// character boundaries, unstyled stretches included. Neighbouring runs may
/// have equal styles: FileMaker edits text in place and keeps such
/// boundaries (`TextColorRemove` with a color, `Substitute`, `Left`,
/// `Upper` keep them; `&` and the functions that set a style merge equal
/// runs). A text that is one unstyled run stores no runs.
#[derive(Clone, Debug, Default)]
pub struct Text {
    text: String,
    runs: Vec<(Range<usize>, CharStyle)>,
    /// What `JSONParsedState` reports (`DBData::mJsonData`, calc-values.md
    /// 1; calc-functions.md 5.5): 0 not parsed, -1 parsed but invalid, 1-6
    /// the JSON type. Only the JSON functions set it; it travels with the
    /// value through variables and `If`, and every text operation, including
    /// [`crate::Value::to_text`], drops it.
    json_state: i8,
}

impl Text {
    /// Bytes reserved for the text, style runs and their font names.
    pub fn heap_capacity_bytes(&self) -> usize {
        self.text.capacity()
            + self.runs.capacity() * std::mem::size_of::<(Range<usize>, CharStyle)>()
            + self
                .runs
                .iter()
                .filter_map(|(_, style)| style.font.as_ref())
                .map(String::capacity)
                .sum::<usize>()
    }

    /// Empty text.
    pub fn new() -> Text {
        Text::default()
    }

    /// Unstyled text.
    pub fn plain(text: impl Into<String>) -> Text {
        Text {
            text: text.into(),
            runs: Vec::new(),
            json_state: 0,
        }
    }

    /// Text in one style.
    pub fn styled(text: impl Into<String>, style: CharStyle) -> Text {
        let text = text.into();
        let runs = if style.is_plain() || text.is_empty() {
            Vec::new()
        } else {
            vec![(0..text.len(), style)]
        };
        Text {
            text,
            runs,
            json_state: 0,
        }
    }

    /// `text` with the given runs (contiguous, covering it).
    fn with_runs(text: String, runs: Vec<(Range<usize>, CharStyle)>) -> Text {
        let mut out = Text {
            text,
            runs,
            json_state: 0,
        };
        out.normalize();
        out
    }

    /// Joins the stand-ins of a high and a low surrogate that have come
    /// together into their character (it takes the high one's run) and
    /// drops the stored runs of a text that is one unstyled run.
    fn normalize(&mut self) {
        self.join_surrogates();
        if self.text.is_empty() || (self.runs.len() == 1 && self.runs[0].1.is_plain()) {
            self.runs.clear();
        }
    }

    fn join_surrogates(&mut self) {
        let is_high = |c: char| as_lone_surrogate(c).is_some_and(|u| u < 0xDC00);
        if !self.text.chars().any(is_high) {
            return;
        }
        let mut joined = String::with_capacity(self.text.len());
        // The new offset of each old character boundary.
        let mut offsets: Vec<(usize, usize)> = Vec::new();
        let mut chars = self.text.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            offsets.push((i, joined.len()));
            let next = chars
                .peek()
                .and_then(|&(j, n)| Some((j, as_lone_surrogate(n)?)));
            let pair = match (as_lone_surrogate(c), next) {
                (Some(high @ 0xD800..=0xDBFF), Some((j, low @ 0xDC00..=0xDFFF))) => {
                    char::decode_utf16([high, low])
                        .next()
                        .and_then(Result::ok)
                        .map(|c| (j, c))
                }
                _ => None,
            };
            match pair {
                Some((j, c)) => {
                    chars.next();
                    joined.push(c);
                    // The boundary between the halves moves to the end.
                    offsets.push((j, joined.len()));
                }
                None => joined.push(c),
            }
        }
        offsets.push((self.text.len(), joined.len()));
        if joined.len() == self.text.len() {
            return;
        }
        let map = |offset: usize| {
            offsets
                .binary_search_by_key(&offset, |&(old, _)| old)
                .map_or(offset, |i| offsets[i].1)
        };
        self.runs = std::mem::take(&mut self.runs)
            .into_iter()
            .map(|(r, style)| (map(r.start)..map(r.end), style))
            .filter(|(r, _)| r.start < r.end)
            .collect();
        self.text = joined;
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The parsed-JSON state the JSON functions gave this text (0 when
    /// none).
    pub fn json_state(&self) -> i8 {
        self.json_state
    }

    /// The same text marked with a parsed-JSON state.
    pub fn with_json_state(mut self, state: i8) -> Text {
        self.json_state = state;
        self
    }

    pub fn into_string(self) -> String {
        self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether no character has a style (run boundaries may remain).
    pub fn is_plain(&self) -> bool {
        self.runs.iter().all(|(_, style)| style.is_plain())
    }

    /// The runs whose style is not the default (byte ranges).
    pub fn styled_runs(&self) -> Vec<(Range<usize>, CharStyle)> {
        self.runs
            .iter()
            .filter(|(_, style)| !style.is_plain())
            .cloned()
            .collect()
    }

    /// Every run in order, unstyled ones included with the default style,
    /// as `GetAsCSS` and `GetAsSVG` walk them.
    pub fn runs(&self) -> Vec<(Range<usize>, CharStyle)> {
        if self.runs.is_empty() && !self.text.is_empty() {
            vec![(0..self.text.len(), CharStyle::default())]
        } else {
            self.runs.clone()
        }
    }

    /// The style of the character at byte `offset`.
    pub fn style_at(&self, offset: usize) -> CharStyle {
        self.runs
            .iter()
            .find(|(range, _)| range.contains(&offset))
            .map(|(_, style)| style.clone())
            .unwrap_or_default()
    }

    /// Appends a run, merging it with the last one when the styles are
    /// equal (`&`, spec 5.2).
    fn push_run(&mut self, range: Range<usize>, style: CharStyle) {
        if range.is_empty() {
            return;
        }
        if self.runs.is_empty() && range.start > 0 {
            self.runs.push((0..range.start, CharStyle::default()));
        }
        match self.runs.last_mut() {
            Some((last, last_style)) if *last_style == style => last.end = range.end,
            _ => self.runs.push((range, style)),
        }
    }

    /// `&`: the left text keeps its runs; each run of the right one merges
    /// with the run before it when their styles are equal (`"" & t` merges
    /// all of `t`'s equal neighbours, `t & ""` keeps them).
    pub fn push(&mut self, other: &Text) {
        self.json_state = 0;
        let offset = self.text.len();
        self.text.push_str(&other.text);
        for (range, style) in other.runs() {
            self.push_run(range.start + offset..range.end + offset, style);
        }
        self.normalize();
    }

    /// Appends unstyled text.
    pub fn push_str(&mut self, s: &str) {
        self.push_styled(s, &CharStyle::default());
    }

    /// Appends text in `style`.
    pub fn push_styled(&mut self, s: &str, style: &CharStyle) {
        self.json_state = 0;
        let start = self.text.len();
        self.text.push_str(s);
        self.push_run(start..self.text.len(), style.clone());
        self.normalize();
    }

    /// The characters in a byte range (on character boundaries) with their
    /// runs, boundaries included.
    pub fn slice(&self, range: Range<usize>) -> Text {
        let runs = self
            .runs
            .iter()
            .filter_map(|(r, style)| {
                let start = r.start.max(range.start);
                let end = r.end.min(range.end);
                (start < end).then(|| (start - range.start..end - range.start, style.clone()))
            })
            .collect();
        Text::with_runs(self.text[range].to_string(), runs)
    }

    /// The text with `range` replaced, as `Replace` does it in place (see
    /// [`Text::splice_all`]).
    pub fn splice(&self, range: Range<usize>, replacement: &Text) -> Text {
        self.splice_all(std::slice::from_ref(&range), replacement)
    }

    /// The text with each of `ranges` (ascending, not overlapping, on
    /// character boundaries) replaced by `replacement`, left to right, as
    /// `Substitute` and `Replace` edit the style buffer in place:
    ///
    /// 1. the replacement takes the style of the first replaced character
    ///    (for an insertion the character after it, at the end the last
    ///    one) with its own attributes laid over it
    ///    (`Replace(TextStyleAdd("abc";Bold);2;1;TextStyleAdd("X";Italic))`
    ///    gives a bold italic `X`);
    /// 2. the replaced characters go: their runs shrink, and when a whole
    ///    run goes the runs that meet merge if their styles are equal;
    /// 3. the new characters join the run they are inserted in when it has
    ///    their base style, at a boundary the run before them or else the
    ///    run after them; otherwise they start a run;
    /// 4. replacement pieces whose style differs from the base are then
    ///    restyled, merging with the run after them when equal, or with the
    ///    run before when they end the text.
    pub fn splice_all(&self, ranges: &[Range<usize>], replacement: &Text) -> Text {
        self.splice_all_checked(ranges, replacement, None)
            .expect("an unlimited splice cannot be refused")
    }

    /// The same edit with a limit on UTF-8 and stored styles. Check the
    /// surviving text, not the sum of the operands, and stop while editing
    /// styles rather than building every repeated run before refusing it.
    pub(crate) fn splice_all_within(
        &self,
        ranges: &[Range<usize>],
        replacement: &Text,
        limit: usize,
    ) -> Option<Text> {
        self.splice_all_checked(ranges, replacement, Some(limit))
    }

    fn splice_all_checked(
        &self,
        ranges: &[Range<usize>],
        replacement: &Text,
        limit: Option<usize>,
    ) -> Option<Text> {
        let capacity = if let Some(limit) = limit {
            let len = ranges.iter().try_fold(self.text.len(), |len, range| {
                len.checked_sub(range.len())?
                    .checked_add(replacement.text.len())
            })?;
            if len > limit {
                return None;
            }
            len
        } else {
            self.text.len()
        };
        let mut edit = RunEdit::of(self);
        let mut out = String::with_capacity(capacity);
        let mut at = 0;
        let pieces: Vec<(usize, CharStyle)> = replacement
            .runs()
            .into_iter()
            .map(|(r, style)| (r.len(), style))
            .collect();
        for range in ranges {
            out.push_str(&self.text[at..range.start]);
            let start = out.len();
            let end = start + range.len();
            let current_len = start + (self.text.len() - range.start);
            let base = if start < current_len {
                edit.style_at(start)
            } else if start > 0 {
                edit.style_at(start - 1)
            } else {
                CharStyle::default()
            };
            edit.delete(start, end - start);
            if !replacement.is_empty() {
                edit.insert(start, replacement.text.len(), &base);
                let mut p = start;
                for (len, style) in &pieces {
                    let style = base.overlay(style);
                    if style != base {
                        edit.set_style(p, *len, style);
                    }
                    p += len;
                    if limit.is_some_and(|limit| {
                        capacity.saturating_add(edit.prefix_storage_size(p)) > limit
                    }) {
                        return None;
                    }
                }
            }
            if limit.is_some_and(|limit| {
                capacity.saturating_add(edit.prefix_storage_size(start + replacement.text.len()))
                    > limit
            }) {
                return None;
            }
            out.push_str(&replacement.text);
            at = range.end;
        }
        out.push_str(&self.text[at..]);
        if limit.is_some_and(|limit| capacity.saturating_add(edit.storage_size()) > limit) {
            return None;
        }
        Some(Text::with_runs(out, edit.into_runs()))
    }

    /// Maps each character to a string, each staying in its run (`Upper`,
    /// `Lower`, `Proper` keep run boundaries).
    pub fn map_chars(&self, mut f: impl FnMut(char) -> String) -> Text {
        let mut out = Rebuild::new(self);
        for (i, c) in self.text.char_indices() {
            out.insert(&f(c), i);
        }
        out.finish()
    }

    /// Changes every run's style, unstyled characters included, and merges
    /// neighbouring runs that end up equal, as the functions that set a
    /// style (`TextFont`, `TextStyleAdd`, ...) and remove one from all text
    /// (`TextColorRemove ( text )`, `TextFormatRemove`) do.
    pub fn map_styles(&self, mut f: impl FnMut(&CharStyle) -> CharStyle) -> Text {
        let mut out = Text::plain(self.text.clone());
        for (range, style) in self.runs() {
            let style = f(&style);
            out.push_run(range, style);
        }
        out.normalize();
        out
    }

    /// Changes every run's style in place, keeping every boundary, as the
    /// functions that remove one attribute value do (`TextColorRemove (
    /// text ; color )`, `TextStyleRemove`).
    pub fn restyle(&self, mut f: impl FnMut(&CharStyle) -> CharStyle) -> Text {
        let runs = self
            .runs()
            .into_iter()
            .map(|(range, style)| {
                let style = f(&style);
                (range, style)
            })
            .collect();
        Text::with_runs(self.text.clone(), runs)
    }

    /// The text without styles, as arithmetic and comparison see it.
    pub fn without_styles(&self) -> Text {
        Text::plain(self.text.clone())
    }

    /// The number of characters (spec 5.1).
    pub fn length(&self) -> usize {
        grapheme_count(&self.text)
    }

    /// The bytes the text takes in memory, as the evaluation's text limit
    /// counts them (`om_calc::eval::CalcLimits::text`): its UTF-8 bytes and
    /// its style runs with their font names.
    pub fn size(&self) -> usize {
        let run = std::mem::size_of::<(Range<usize>, CharStyle)>();
        self.runs.iter().fold(self.text.len(), |size, (_, style)| {
            size + run + style.font.as_ref().map_or(0, String::len)
        })
    }

    /// Byte ranges of the values of a value list (spec 5.1,
    /// calc-functions.md 3.4): values end at LF, CR, U+2028 or U+2029 (CR LF
    /// is two separators), and a trailing separator does not start another
    /// value (`ValueCount("a¶b¶")` = 2, `ValueCount("¶")` = 1, empty text has
    /// no values).
    pub fn value_ranges(&self) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        for (i, c) in self.text.char_indices() {
            if is_value_separator(c) {
                out.push(start..i);
                start = i + c.len_utf8();
            }
        }
        if start < self.text.len() {
            out.push(start..self.text.len());
        }
        out
    }
}

/// A style buffer edited in place: run lengths (bytes) and styles.
struct RunEdit {
    runs: Vec<(usize, CharStyle)>,
}

impl RunEdit {
    fn of(text: &Text) -> RunEdit {
        RunEdit {
            runs: text
                .runs()
                .into_iter()
                .map(|(range, style)| (range.len(), style))
                .collect(),
        }
    }

    fn into_runs(self) -> Vec<(Range<usize>, CharStyle)> {
        let mut at = 0;
        self.runs
            .into_iter()
            .map(|(len, style)| {
                at += len;
                (at - len..at, style)
            })
            .collect()
    }

    fn run_storage(style: &CharStyle) -> usize {
        std::mem::size_of::<(Range<usize>, CharStyle)>()
            .saturating_add(style.font.as_ref().map_or(0, String::len))
    }

    fn storage_size(&self) -> usize {
        if self.runs.len() == 1 && self.runs[0].1.is_plain() {
            return 0;
        }
        self.runs.iter().fold(0usize, |size, (_, style)| {
            size.saturating_add(Self::run_storage(style))
        })
    }

    /// Later ascending edits can only merge the last run at the boundary.
    /// Earlier complete runs are final: count their fonts as well, leaving
    /// that last run out so a later merge cannot cause a false refusal.
    fn prefix_storage_size(&self, end: usize) -> usize {
        let mut at = 0usize;
        let mut size = 0usize;
        let mut last = 0usize;
        for (len, style) in &self.runs {
            at = at.saturating_add(*len);
            if at > end {
                break;
            }
            size = size.saturating_add(last);
            last = Self::run_storage(style);
        }
        size
    }

    fn style_at(&self, offset: usize) -> CharStyle {
        let mut at = 0;
        for (len, style) in &self.runs {
            if offset < at + len {
                return style.clone();
            }
            at += len;
        }
        CharStyle::default()
    }

    /// Removes `len` bytes at `at`; when a whole run goes, the runs that
    /// meet at `at` merge if their styles are equal.
    fn delete(&mut self, at: usize, len: usize) {
        if len == 0 {
            return;
        }
        let end = at + len;
        let mut pos = 0;
        let mut vanished = false;
        let mut kept = Vec::with_capacity(self.runs.len());
        for (rlen, style) in self.runs.drain(..) {
            let overlap = (pos + rlen).min(end).saturating_sub(pos.max(at));
            pos += rlen;
            if overlap == rlen {
                vanished = true;
            } else {
                kept.push((rlen - overlap, style));
            }
        }
        self.runs = kept;
        if vanished {
            let mut pos = 0;
            for i in 0..self.runs.len() {
                pos += self.runs[i].0;
                if pos == at && i + 1 < self.runs.len() {
                    if self.runs[i].1 == self.runs[i + 1].1 {
                        let next = self.runs.remove(i + 1).0;
                        self.runs[i].0 += next;
                    }
                    break;
                }
                if pos > at {
                    break;
                }
            }
        }
    }

    /// Inserts `len` bytes in `style` at `at`: inside a run of that style
    /// they join it, inside another they split it; at a boundary they join
    /// the run before when it has the style, else the run after.
    fn insert(&mut self, at: usize, len: usize, style: &CharStyle) {
        if self.runs.is_empty() {
            self.runs.push((len, style.clone()));
            return;
        }
        if at == 0 {
            if self.runs[0].1 == *style {
                self.runs[0].0 += len;
            } else {
                self.runs.insert(0, (len, style.clone()));
            }
            return;
        }
        let mut pos = 0;
        for i in 0..self.runs.len() {
            let (rlen, rstyle) = self.runs[i].clone();
            if at < pos + rlen {
                if rstyle == *style {
                    self.runs[i].0 += len;
                } else {
                    let before = at - pos;
                    self.runs[i].0 = before;
                    self.runs.insert(i + 1, (len, style.clone()));
                    self.runs.insert(i + 2, (rlen - before, rstyle));
                }
                return;
            }
            pos += rlen;
            if at == pos {
                if rstyle == *style {
                    self.runs[i].0 += len;
                } else if self.runs.get(i + 1).is_some_and(|(_, next)| next == style) {
                    self.runs[i + 1].0 += len;
                } else {
                    self.runs.insert(i + 1, (len, style.clone()));
                }
                return;
            }
        }
    }

    /// Restyles the `len` bytes at `at` (inside one run): they become a run
    /// of their own that merges with the run after it when equal, or with
    /// the run before when it ends the text.
    fn set_style(&mut self, at: usize, len: usize, style: CharStyle) {
        let mut pos = 0;
        for i in 0..self.runs.len() {
            let (rlen, rstyle) = self.runs[i].clone();
            if at < pos + rlen {
                let before = at - pos;
                let after = rlen - before - len;
                let mut i = i;
                self.runs.remove(i);
                if before > 0 {
                    self.runs.insert(i, (before, rstyle.clone()));
                    i += 1;
                }
                self.runs.insert(i, (len, style.clone()));
                if after > 0 {
                    self.runs.insert(i + 1, (after, rstyle));
                }
                match self.runs.get(i + 1) {
                    Some((_, next)) => {
                        if *next == style {
                            let next = self.runs.remove(i + 1).0;
                            self.runs[i].0 += next;
                        }
                    }
                    None => {
                        if i > 0 && self.runs[i - 1].1 == style {
                            let this = self.runs.remove(i).0;
                            self.runs[i - 1].0 += this;
                        }
                    }
                }
                return;
            }
            pos += rlen;
        }
    }
}

/// Builds a text from pieces of a source text as editing it in place does:
/// each piece stays in the run of the source character it comes from or is
/// inserted next to, so run boundaries survive (`Filter`, `FilterValues`,
/// `Quote`, `TrimAll`, case mapping). When every character of a run is
/// gone, the runs that meet merge if their styles are equal.
pub struct Rebuild<'a> {
    source: &'a Text,
    runs: Vec<(Range<usize>, CharStyle)>,
    out: Text,
    last: Option<usize>,
    join: bool,
}

impl<'a> Rebuild<'a> {
    pub fn new(source: &'a Text) -> Rebuild<'a> {
        let mut runs = source.runs();
        if runs.is_empty() {
            runs.push((0..0, CharStyle::default()));
        }
        Rebuild {
            source,
            runs,
            out: Text::new(),
            last: None,
            join: false,
        }
    }

    /// The index of the run holding the source byte `offset` (the last run
    /// past the end).
    fn run_of(&self, offset: usize) -> usize {
        self.runs
            .iter()
            .position(|(range, _)| offset < range.end)
            .unwrap_or(self.runs.len() - 1)
    }

    fn piece(&mut self, s: &str, run: usize) {
        if s.is_empty() {
            return;
        }
        let style = &self.runs[run].1;
        let start = self.out.text.len();
        self.out.text.push_str(s);
        let end = self.out.text.len();
        let extend = match self.last {
            Some(last) if last == run => true,
            Some(last) => {
                (self.join || run > last + 1 || run < last)
                    && self.out.runs.last().is_some_and(|(_, prev)| prev == style)
            }
            None => false,
        };
        if self.last.is_some_and(|last| last != run) {
            self.join = false;
        }
        match self.out.runs.last_mut() {
            Some((range, _)) if extend => range.end = end,
            _ => self.out.runs.push((start..end, style.clone())),
        }
        self.last = Some(run);
    }

    /// Copies the source bytes `range`, each character in its run.
    pub fn keep(&mut self, range: Range<usize>) {
        for i in 0..self.runs.len() {
            let r = self.runs[i].0.clone();
            let start = r.start.max(range.start);
            let end = r.end.min(range.end);
            if start < end {
                self.piece(&self.source.text[start..end], i);
            }
        }
    }

    /// Appends `s` in the run of the source character at byte `offset`.
    pub fn insert(&mut self, s: &str, offset: usize) {
        let run = self.run_of(offset);
        self.piece(s, run);
    }

    /// Makes the next change of source run merge with the output's last
    /// run when their styles are equal (`TrimAll` joining the runs around a
    /// space it keeps).
    pub fn join(&mut self) {
        self.join = true;
    }

    pub fn finish(mut self) -> Text {
        self.out.normalize();
        self.out
    }
}

/// Whether `c` ends a value in a value list.
pub fn is_value_separator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl PartialEq for Text {
    /// Exact equality of characters and styles (not FileMaker's `=`, which
    /// is [`text_eq`]).
    fn eq(&self, other: &Text) -> bool {
        self.text == other.text && self.runs == other.runs
    }
}

impl Eq for Text {}

impl From<&str> for Text {
    fn from(s: &str) -> Text {
        Text::plain(s)
    }
}

impl From<String> for Text {
    fn from(s: String) -> Text {
        Text::plain(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_surrogates_have_stand_ins_that_join_into_pairs() {
        let high = lone_surrogate(0xD83D).unwrap();
        let low = lone_surrogate(0xDE00).unwrap();
        assert_eq!(as_lone_surrogate(high), Some(0xD83D));
        assert_eq!(lone_surrogate(0x41), None);
        assert_eq!(as_lone_surrogate('a'), None);
        assert_eq!(
            utf16_units(&format!("a{high}")).collect::<Vec<_>>(),
            [0x61, 0xD83D]
        );
        let mut t = Text::plain(format!("a{high}"));
        t.push(&Text::plain(format!("{low}b")));
        assert_eq!(t.as_str(), "a😀b");
        assert_eq!(t.length(), 3);
        // A low one before a high one stays apart.
        let mut t = Text::plain(low.to_string());
        t.push(&Text::plain(high.to_string()));
        assert_eq!(t.length(), 2);
        // The pair takes the high half's run.
        let blue = CharStyle {
            color: Some(0xFF),
            ..CharStyle::default()
        };
        let mut t = Text::plain(format!("x{high}"));
        t.push(&Text::styled(format!("{low}y"), blue.clone()));
        assert_eq!(t.as_str(), "x😀y");
        assert_eq!(t.runs(), [(0..5, CharStyle::default()), (5..6, blue)]);
    }

    fn bold() -> CharStyle {
        CharStyle {
            flags: 1,
            ..CharStyle::default()
        }
    }

    #[test]
    fn concatenation_merges_identical_runs() {
        let mut t = Text::styled("a", bold());
        t.push(&Text::styled("b", bold()));
        assert_eq!(t.styled_runs(), &[(0..2, bold())]);
        t.push(&Text::plain("c"));
        assert_eq!(t.runs().len(), 2);
        assert_eq!(t.runs()[1], (2..3, CharStyle::default()));
    }

    #[test]
    fn slicing_and_replacing_keep_styles() {
        let t = Text::styled("abc", bold());
        let middle = t.slice(1..2);
        assert_eq!(middle.as_str(), "b");
        assert_eq!(middle.styled_runs(), &[(0..1, bold())]);
        let italic = CharStyle {
            flags: 2,
            ..CharStyle::default()
        };
        let spliced = t.splice(1..2, &Text::styled("X", italic));
        assert_eq!(spliced.as_str(), "aXc");
        let both = CharStyle {
            flags: 3,
            ..CharStyle::default()
        };
        assert_eq!(
            spliced.styled_runs(),
            &[(0..1, bold()), (1..2, both), (2..3, bold())]
        );
        assert_eq!(
            Text::plain("ab").splice(2..2, &Text::plain("X")).as_str(),
            "abX"
        );
        let replaced = t.splice(1..2, &Text::plain("X"));
        assert_eq!(replaced.as_str(), "aXc");
        assert_eq!(replaced.styled_runs(), &[(0..3, bold())]);
        let upper = t.map_chars(|c| c.to_uppercase().collect());
        assert_eq!(upper.as_str(), "ABC");
        assert_eq!(upper.styled_runs(), &[(0..3, bold())]);
        assert!(t.map_styles(|_| CharStyle::default()).is_plain());

        // Replacing a whole styled value counts only the surviving style.
        let font = CharStyle {
            font: Some("F".repeat(512)),
            ..CharStyle::default()
        };
        let short_font = Text::styled(
            "X",
            CharStyle {
                font: Some("F".into()),
                ..CharStyle::default()
            },
        );
        assert!(Text::styled("abc", font.clone())
            .splice_all_within(std::slice::from_ref(&(0..3)), &short_font, 128)
            .is_some());

        // Repeated styles inherit the source font. The bytes and bare run
        // structs fit 2 KiB, but the repeated font names do not (#347).
        let mut replacement = Text::styled("b", bold());
        replacement.push(&Text::plain("c"));
        let ranges: Vec<_> = (0..8).map(|i| i..i + 1).collect();
        assert!(Text::plain("a".repeat(8))
            .splice_all_within(&ranges, &replacement, 2048)
            .is_some());
        assert!(Text::styled("a".repeat(8), font)
            .splice_all_within(&ranges, &replacement, 2048)
            .is_none());
    }

    fn color(rgb: u32) -> CharStyle {
        CharStyle {
            color: Some(rgb),
            ..CharStyle::default()
        }
    }

    /// The runs as `[text|style]` pieces, `.` for the default style, `b`
    /// bold, `B` blue (255), `G` green (65280).
    fn shape(t: &Text) -> String {
        t.runs()
            .into_iter()
            .map(|(range, style)| {
                let tag = match (&style.color, style.flags) {
                    (None, 0) => ".",
                    (None, 1) => "b",
                    (Some(255), 0) => "B",
                    (Some(65280), 0) => "G",
                    _ => "?",
                };
                format!("[{}|{tag}]", &t.as_str()[range])
            })
            .collect()
    }

    /// `TextColorRemove ( TextColor ( "a" ; 255 ) & "b" ; 255 )`: two
    /// unstyled runs.
    fn kept(a: &str, b: &str) -> Text {
        let mut t = Text::styled(a, color(255));
        t.push(&Text::plain(b));
        t.restyle(|s| CharStyle {
            color: None,
            ..s.clone()
        })
    }

    #[test]
    fn removing_in_place_keeps_boundaries() {
        let k = kept("a", "b");
        assert_eq!(shape(&k), "[a|.][b|.]");
        assert!(k.is_plain());
        assert_ne!(k, Text::plain("ab"));
        // `&` keeps the left boundaries and merges the right text's runs
        // into the run before them.
        let mut left = k.clone();
        left.push(&Text::plain("c"));
        assert_eq!(shape(&left), "[a|.][bc|.]");
        let mut right = Text::plain("c");
        right.push(&k);
        assert_eq!(shape(&right), "[cab|.]");
        let mut both = k.clone();
        both.push(&k);
        assert_eq!(shape(&both), "[a|.][bab|.]");
        // Setting a style merges; slices and case mapping keep boundaries.
        assert_eq!(shape(&k.map_styles(|s| s.clone())), "[ab|.]");
        let k3 = kept("ab", "cd");
        assert_eq!(shape(&k3.slice(1..3)), "[b|.][c|.]");
        assert_eq!(
            shape(&k3.map_chars(|c| c.to_uppercase().collect())),
            "[AB|.][CD|.]"
        );
    }

    #[test]
    fn replacing_in_place() {
        let k3 = kept("ab", "cd");
        let x = Text::plain("X");
        let blue_x = Text::styled("X", color(255));
        assert_eq!(shape(&k3.splice(2..3, &Text::plain("c"))), "[abc|.][d|.]");
        assert_eq!(shape(&k3.splice(0..2, &x)), "[Xcd|.]");
        assert_eq!(shape(&k3.splice(1..3, &Text::new())), "[a|.][d|.]");
        assert_eq!(shape(&k3.splice(3..4, &blue_x)), "[ab|.][c|.][X|B]");
        assert_eq!(shape(&k3.splice(2..2, &blue_x)), "[ab|.][X|B][cd|.]");
        // a, b, c with b unstyled between two blue runs.
        let mut abc = Text::styled("a", color(255));
        abc.push(&Text::plain("b"));
        abc.push(&Text::styled("c", color(255)));
        assert_eq!(shape(&abc.splice(1..2, &Text::new())), "[ac|B]");
        assert_eq!(shape(&abc.splice(1..2, &x)), "[a|B][X|.][c|B]");
        assert_eq!(shape(&abc.splice(1..2, &blue_x)), "[a|B][Xc|B]");
        let mut ab_c = Text::styled("ab", color(255));
        ab_c.push(&Text::plain("c"));
        assert_eq!(shape(&ab_c.splice(2..3, &blue_x)), "[abX|B]");
        let mut a_b_g = Text::styled("a", color(255));
        a_b_g.push(&Text::plain("b"));
        a_b_g.push(&Text::styled("c", color(65280)));
        assert_eq!(shape(&a_b_g.splice(1..2, &blue_x)), "[a|B][X|B][c|G]");
        // Substitute: every match left to right.
        let mut xbab = Text::plain("a");
        xbab.push(&Text::styled("b", color(255)));
        xbab.push(&Text::plain("a"));
        xbab.push(&Text::styled("b", color(255)));
        assert_eq!(
            shape(&xbab.splice_all(&[0..1, 2..3], &blue_x)),
            "[Xb|B][Xb|B]"
        );
    }

    #[test]
    fn rebuilding_in_place() {
        let k3 = kept("ab", "cd");
        let mut r = Rebuild::new(&k3);
        r.keep(0..1);
        r.keep(3..4);
        assert_eq!(shape(&r.finish()), "[a|.][d|.]");
        let mut abc = Text::styled("a", color(255));
        abc.push(&Text::plain("b"));
        abc.push(&Text::styled("c", color(255)));
        let mut r = Rebuild::new(&abc);
        r.keep(0..1);
        r.keep(2..3);
        assert_eq!(shape(&r.finish()), "[ac|B]");
        let mut r = Rebuild::new(&k3);
        r.insert("\"", 0);
        r.keep(0..4);
        r.insert("\"", 3);
        assert_eq!(shape(&r.finish()), "[\"ab|.][cd\"|.]");
        let mut r = Rebuild::new(&k3);
        r.keep(0..2);
        r.join();
        r.keep(2..4);
        assert_eq!(shape(&r.finish()), "[abcd|.]");
    }

    #[test]
    fn value_lists() {
        let count = |s: &str| Text::plain(s).value_ranges().len();
        assert_eq!(count("a\rb"), 2);
        assert_eq!(count("a\rb\r"), 2);
        assert_eq!(count("a\rb\r\r"), 3);
        assert_eq!(count("a\nb"), 2);
        assert_eq!(count("a\r\nb"), 3);
        assert_eq!(count(""), 0);
        assert_eq!(count("\r"), 1);
        assert_eq!(count("a\u{85}b"), 1);
    }
}
