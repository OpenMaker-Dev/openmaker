//! Excel 97-2004 workbooks (`XLS `, `.xls`): BIFF8 records in the
//! `Workbook` stream of a compound file, read from Microsoft's published
//! formats, [MS-CFB] (the compound file) and [MS-XLS] (the records).
//!
//! A sheet is read into rows as [`super::read::workbook`] reads an `.xlsx`
//! sheet, so both kinds of workbook import alike: shared and inline
//! strings as text (line feeds as returns), numbers as text, Booleans as 1
//! and 0, errors as Excel spells them (`#DIV/0!`), a formula as its cached
//! result, and blank cells as nothing. Rows run from the first to the last
//! row with a cell or a row record; columns from A to the row's last cell.
//! A number is written as the shortest decimal that reads back as the
//! same double (`0.1`, `-2.5`, `1000000000000000000000`), the form an
//! `.xlsx` holds for most numbers [I: how FileMaker spells a number it
//! reads from an `.xls` is unmeasured].
//!
//! Refused with error 730, as an unreadable `.xlsx`: a file that is not a
//! compound file, a stream or sector chain out of bounds or looping, a
//! workbook that is not BIFF8 (Excel 5.0/95's `Book` stream), an encrypted
//! one (`FILEPASS`), and a sheet that is not a worksheet (a chart, macro or
//! dialog sheet). The `Workbook` stream is the root storage's own, never an
//! embedded object's, and a worksheet's embedded substreams (a chart's
//! cached data) are not its cells. Input is hostile:
//! every length is checked against the file, chains are bounded by the
//! number of sectors, and what the rows take is charged to the import's
//! budget ([`super::limit`]).

use super::limit::{self, Budget};
use super::read::SourceRow;
use crate::{Result, TransferError};

/// The error of an unreadable Excel workbook (as `.xlsx`).
const WORKBOOK: i32 = 730;

fn bad() -> TransferError {
    TransferError::new(WORKBOOK, "The Excel workbook could not be read.")
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

// ------------------------------------------------------------ compound file

const SIGNATURE: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
/// The last regular sector number; above it are markers.
const MAX_SECTOR: u32 = 0xffff_fffa;
const END_OF_CHAIN: u32 = 0xffff_fffe;
const FREE: u32 = 0xffff_ffff;
const HEADER_DIFAT: usize = 109;

/// A compound file ([MS-CFB] 2): its sectors and allocation tables.
struct Compound<'a> {
    bytes: &'a [u8],
    sector: usize,
    /// The sectors the file holds: no chain without a loop is longer, so a
    /// chain's bytes stay within the file's size.
    sectors: usize,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_cutoff: u64,
    /// The root entry's stream, which holds the mini sectors.
    mini_stream: Vec<u8>,
    entries: Vec<Entry>,
}

struct Entry {
    name: String,
    kind: u8,
    /// The entry's siblings and first child in the directory's trees
    /// ([MS-CFB] 2.6.1), [`NO_ENTRY`] for none.
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    size: u64,
}

/// No directory entry (`NOSTREAM`).
const NO_ENTRY: u32 = 0xffff_ffff;

impl<'a> Compound<'a> {
    fn open(bytes: &'a [u8]) -> Result<Compound<'a>> {
        if bytes.get(..8) != Some(&SIGNATURE[..]) || u16_at(bytes, 0x1c) != Some(0xfffe) {
            return Err(bad());
        }
        let shift = u16_at(bytes, 0x1e).ok_or_else(bad)?;
        let major = u16_at(bytes, 0x1a).ok_or_else(bad)?;
        // Version 3 has 512-byte sectors, version 4 4,096-byte ones.
        let sector = match (major, shift) {
            (3, 9) => 512,
            (4, 12) => 4096,
            _ => return Err(bad()),
        };
        if u16_at(bytes, 0x20) != Some(6) {
            return Err(bad());
        }
        let fat_sectors = u32_at(bytes, 0x2c).ok_or_else(bad)? as usize;
        let first_directory = u32_at(bytes, 0x30).ok_or_else(bad)?;
        let mini_cutoff = u64::from(u32_at(bytes, 0x38).ok_or_else(bad)?);
        let first_mini_fat = u32_at(bytes, 0x3c).ok_or_else(bad)?;
        let first_difat = u32_at(bytes, 0x44).ok_or_else(bad)?;
        let difat_sectors = u32_at(bytes, 0x48).ok_or_else(bad)? as usize;
        // Sectors the file holds after its header (sector -1).
        let sectors = (bytes.len() / sector).saturating_sub(1);
        if fat_sectors > sectors || difat_sectors > sectors {
            return Err(bad());
        }
        let mut file = Compound {
            bytes,
            sector,
            sectors,
            fat: Vec::new(),
            mini_fat: Vec::new(),
            mini_cutoff,
            mini_stream: Vec::new(),
            entries: Vec::new(),
        };
        // The FAT's sectors: 109 in the header, the rest in DIFAT sectors.
        let mut fat_list = Vec::with_capacity(fat_sectors);
        for i in 0..HEADER_DIFAT.min(fat_sectors) {
            fat_list.push(u32_at(bytes, 0x4c + 4 * i).ok_or_else(bad)?);
        }
        let per_difat = sector / 4 - 1;
        let mut next = first_difat;
        let mut seen = 0;
        while fat_list.len() < fat_sectors {
            if next > MAX_SECTOR || seen >= difat_sectors {
                return Err(bad());
            }
            seen += 1;
            let data = file.sector_data(next)?;
            for i in 0..per_difat {
                if fat_list.len() == fat_sectors {
                    break;
                }
                fat_list.push(u32_at(data, 4 * i).ok_or_else(bad)?);
            }
            next = u32_at(data, 4 * per_difat).ok_or_else(bad)?;
        }
        // A FAT sector listed twice is no FAT a writer makes.
        let mut listed = vec![false; sectors];
        for &s in &fat_list {
            let seen = listed.get_mut(s as usize).ok_or_else(bad)?;
            if *seen {
                return Err(bad());
            }
            *seen = true;
        }
        let mut fat = Vec::with_capacity(fat_sectors * (sector / 4));
        for &s in &fat_list {
            let data = file.sector_data(s)?;
            fat.extend(
                data.chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])),
            );
        }
        file.fat = fat;
        let directory = file.chain_bytes(first_directory, None)?;
        for raw in directory.chunks_exact(128) {
            let len = usize::from(u16_at(raw, 64).ok_or_else(bad)?).min(64);
            let units: Vec<u16> = raw[..len.saturating_sub(2)]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let size = u64_at(raw, 120).ok_or_else(bad)?;
            file.entries.push(Entry {
                name: String::from_utf16_lossy(&units),
                kind: raw[66],
                left: u32_at(raw, 68).ok_or_else(bad)?,
                right: u32_at(raw, 72).ok_or_else(bad)?,
                child: u32_at(raw, 76).ok_or_else(bad)?,
                start: u32_at(raw, 116).ok_or_else(bad)?,
                // Version 3 files may leave the high half undefined.
                size: if major == 3 { size & 0xffff_ffff } else { size },
            });
        }
        let root = file
            .entries
            .first()
            .filter(|e| e.kind == 5)
            .ok_or_else(bad)?;
        let (root_start, root_size) = (root.start, root.size);
        if root_size > 0 {
            file.mini_stream = file.chain_bytes(root_start, Some(root_size))?;
        }
        if first_mini_fat <= MAX_SECTOR {
            let table = file.chain_bytes(first_mini_fat, None)?;
            file.mini_fat = table
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
        }
        Ok(file)
    }

    fn sector_data(&self, n: u32) -> Result<&'a [u8]> {
        if n > MAX_SECTOR {
            return Err(bad());
        }
        let at = (n as usize + 1).checked_mul(self.sector).ok_or_else(bad)?;
        let end = at.checked_add(self.sector).ok_or_else(bad)?;
        self.bytes.get(at..end).ok_or_else(bad)
    }

    /// A regular chain's bytes from sector `start`, up to `size` when
    /// given (sectors past it are not read). A chain that comes back to a
    /// sector it holds, its link past `size` too, loops and is refused, so
    /// a chain is never longer than the file.
    fn chain_bytes(&self, start: u32, size: Option<u64>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut seen = vec![false; self.sectors];
        let mut next = start;
        while next != END_OF_CHAIN {
            let visited = seen.get_mut(next as usize).ok_or_else(bad)?;
            if *visited {
                return Err(bad());
            }
            if size.is_some_and(|s| out.len() as u64 >= s) {
                break;
            }
            *visited = true;
            out.extend_from_slice(self.sector_data(next)?);
            next = *self.fat.get(next as usize).ok_or_else(bad)?;
            if next == FREE {
                return Err(bad());
            }
        }
        if let Some(size) = size {
            if (out.len() as u64) < size {
                return Err(bad());
            }
            out.truncate(size as usize);
        }
        Ok(out)
    }

    /// A chain of 64-byte mini sectors in the mini stream; one that comes
    /// back to a mini sector it holds loops and is refused.
    fn mini_chain_bytes(&self, start: u32, size: u64) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut seen = vec![false; self.mini_stream.len() / 64];
        let mut next = start;
        while (out.len() as u64) < size {
            // In range before any offset is computed from it.
            let visited = seen.get_mut(next as usize).ok_or_else(bad)?;
            if *visited {
                return Err(bad());
            }
            *visited = true;
            let at = next as usize * 64;
            out.extend_from_slice(self.mini_stream.get(at..at + 64).ok_or_else(bad)?);
            next = *self.mini_fat.get(next as usize).ok_or_else(bad)?;
        }
        if next != END_OF_CHAIN && seen.get(next as usize) == Some(&true) {
            return Err(bad());
        }
        out.truncate(size as usize);
        Ok(out)
    }

    /// The stream named `name` (any case) among the root storage's own
    /// entries: its child's tree of siblings ([MS-CFB] 2.6.4), each entry
    /// visited once, so a stream of an embedded storage (another workbook's)
    /// is never taken for it.
    fn stream(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let mut seen = vec![false; self.entries.len()];
        let mut stack = vec![self.entries[0].child];
        let mut found = None;
        while let Some(id) = stack.pop() {
            if id == NO_ENTRY {
                continue;
            }
            let visited = seen.get_mut(id as usize).ok_or_else(bad)?;
            if *visited {
                return Err(bad());
            }
            *visited = true;
            let entry = &self.entries[id as usize];
            if entry.kind == 2 && entry.name.eq_ignore_ascii_case(name) {
                found = Some(entry);
                break;
            }
            stack.extend([entry.left, entry.right]);
        }
        let Some(entry) = found else {
            return Ok(None);
        };
        // No stream is larger than the file that holds it.
        if entry.size > self.bytes.len() as u64 {
            return Err(bad());
        }
        if entry.size < self.mini_cutoff {
            self.mini_chain_bytes(entry.start, entry.size).map(Some)
        } else {
            self.chain_bytes(entry.start, Some(entry.size)).map(Some)
        }
    }
}

// ------------------------------------------------------------------ records

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000a;
const FILEPASS: u16 = 0x002f;
const BOUNDSHEET: u16 = 0x0085;
const SST: u16 = 0x00fc;
const CONTINUE: u16 = 0x003c;
const LABELSST: u16 = 0x00fd;
const LABEL: u16 = 0x0204;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027e;
const MULRK: u16 = 0x00bd;
const BOOLERR: u16 = 0x0205;
const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;
const ROW: u16 = 0x0208;
const WSBOOL: u16 = 0x0081;
/// `WsBool`'s `fDialog`: a dialog sheet, stored as a worksheet.
const DIALOG: u16 = 0x0010;

/// The records of a stream from `at`: (type, data, offset of the next).
fn record(stream: &[u8], at: usize) -> Option<(u16, &[u8], usize)> {
    let kind = u16_at(stream, at)?;
    let len = usize::from(u16_at(stream, at + 2)?);
    let data = stream.get(at + 4..at + 4 + len)?;
    Some((kind, data, at + 4 + len))
}

/// A record's data with the `CONTINUE` records after it, read across their
/// boundaries; in a string's characters each continuation starts with the
/// flags byte of the characters after it ([MS-XLS] 2.5.293). The records
/// are read as they are reached, so a long run of them costs no memory.
struct Pieces<'s> {
    stream: &'s [u8],
    /// The data of the record being read, and where in it.
    data: &'s [u8],
    at: usize,
    /// The offset of the record after it.
    next: usize,
}

impl<'s> Pieces<'s> {
    /// The record at `at` ([`record`]).
    fn new(stream: &'s [u8], at: usize) -> Option<Pieces<'s>> {
        let (_, data, next) = record(stream, at)?;
        Some(Pieces {
            stream,
            data,
            at: 0,
            next,
        })
    }

    /// On to the next `CONTINUE` record; the data ends without one.
    fn advance(&mut self) -> Result<()> {
        match record(self.stream, self.next) {
            Some((CONTINUE, data, next)) => {
                (self.data, self.at, self.next) = (data, 0, next);
                Ok(())
            }
            _ => Err(bad()),
        }
    }

    /// The offset of the first record after this one's `CONTINUE`s.
    fn end(mut self) -> usize {
        while let Some((CONTINUE, _, next)) = record(self.stream, self.next) {
            self.next = next;
        }
        self.next
    }

    fn byte(&mut self) -> Result<u8> {
        while self.at >= self.data.len() {
            self.advance()?;
        }
        self.at += 1;
        Ok(self.data[self.at - 1])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes([self.byte()?, self.byte()?]))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes([
            self.byte()?,
            self.byte()?,
            self.byte()?,
            self.byte()?,
        ]))
    }

    fn skip(&mut self, mut n: usize) -> Result<()> {
        while n > 0 {
            if self.at >= self.data.len() {
                self.advance()?;
                continue;
            }
            let take = (self.data.len() - self.at).min(n);
            self.at += take;
            n -= take;
        }
        Ok(())
    }

    /// `count` characters, 1 byte each when `high` is off, 2 otherwise; at
    /// a continuation the next flags byte says which for the rest. At most
    /// 65,535 of them (a u16 count), so the text is charged by the caller.
    fn chars(&mut self, count: usize, mut high: bool) -> Result<String> {
        let mut units: Vec<u16> = Vec::with_capacity(count);
        let mut left = count;
        while left > 0 {
            if self.at >= self.data.len() {
                self.advance()?;
                high = self.byte()? & 1 != 0;
                continue;
            }
            if high {
                let lo = self.byte()?;
                let hi = self.byte()?;
                units.push(u16::from_le_bytes([lo, hi]));
            } else {
                units.push(u16::from(self.byte()?));
            }
            left -= 1;
        }
        Ok(String::from_utf16_lossy(&units)
            .replace("\r\n", "\r")
            .replace('\n', "\r"))
    }

    /// An `XLUnicodeRichExtendedString` (shared strings) or, with `rich`
    /// off, an `XLUnicodeString` (labels, formula strings).
    /// Line feeds are returns, as in an `.xlsx`.
    fn string(&mut self) -> Result<String> {
        let count = usize::from(self.u16()?);
        let flags = self.byte()?;
        let runs = if flags & 0x08 != 0 { self.u16()? } else { 0 };
        let ext = if flags & 0x04 != 0 { self.u32()? } else { 0 };
        let text = self.chars(count, flags & 0x01 != 0)?;
        self.skip(usize::from(runs) * 4)?;
        self.skip(ext as usize)?;
        Ok(text)
    }
}

/// Excel's spelling of an error value ([MS-XLS] 2.5.13 `BErr`).
fn error_text(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0f => "#VALUE!",
        0x17 => "#REF!",
        0x1d => "#NAME?",
        0x24 => "#NUM!",
        0x2a => "#N/A",
        0x2b => "#GETTING_DATA",
        _ => "#N/A",
    }
}

/// The shortest decimal that reads back as `value` (module docs).
fn number_text(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    format!("{value}")
}

/// An `RkNumber` ([MS-XLS] 2.5.217).
fn rk_value(rk: u32) -> f64 {
    let value = if rk & 0x02 != 0 {
        f64::from((rk as i32) >> 2)
    } else {
        f64::from_bits(u64::from(rk & 0xffff_fffc) << 32)
    };
    if rk & 0x01 != 0 {
        value / 100.0
    } else {
        value
    }
}

/// A sheet of the workbook in `bytes` (see the module docs): by name when
/// `worksheet` names one, else the `sheet`th (0-based).
pub(crate) fn workbook(
    bytes: &[u8],
    sheet: usize,
    worksheet: &str,
    budget: &mut Budget,
) -> Result<Vec<SourceRow>> {
    let file = Compound::open(bytes)?;
    let Some(stream) = file.stream("Workbook")? else {
        return Err(TransferError::new(
            WORKBOOK,
            "Excel workbooks older than Excel 97 are not supported.",
        ));
    };
    // The globals: BIFF8's BOF, then up to their EOF the sheets, the
    // shared strings and whether the workbook is encrypted.
    let (kind, data, mut at) = record(&stream, 0).ok_or_else(bad)?;
    if kind != BOF || u16_at(data, 0) != Some(0x0600) || u16_at(data, 2) != Some(0x0005) {
        return Err(bad());
    }
    let mut sheets: Vec<(usize, u8, String)> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    loop {
        let (kind, data, next) = record(&stream, at).ok_or_else(bad)?;
        match kind {
            EOF => break,
            FILEPASS => {
                return Err(TransferError::new(
                    WORKBOOK,
                    "Encrypted Excel workbooks are not supported.",
                ))
            }
            BOUNDSHEET => {
                let position = u32_at(data, 0).ok_or_else(bad)? as usize;
                let sheet_kind = *data.get(5).ok_or_else(bad)?;
                let count = usize::from(*data.get(6).ok_or_else(bad)?);
                let high = *data.get(7).ok_or_else(bad)? & 1 != 0;
                let name = if high {
                    let raw = data.get(8..8 + 2 * count).ok_or_else(bad)?;
                    let units: Vec<u16> = raw
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    String::from_utf16_lossy(&units)
                } else {
                    data.get(8..8 + count)
                        .ok_or_else(bad)?
                        .iter()
                        .map(|&b| char::from(b))
                        .collect()
                };
                budget.charge(std::mem::size_of::<(usize, u8, String)>() + name.len())?;
                sheets.push((position, sheet_kind, name));
                at = next;
            }
            SST => {
                let mut sst = Pieces::new(&stream, at).ok_or_else(bad)?;
                sst.u32()?;
                let unique = sst.u32()? as usize;
                for _ in 0..unique {
                    let text = sst.string()?;
                    budget.charge(std::mem::size_of::<String>() + text.len())?;
                    strings.push(text);
                }
                at = sst.end();
            }
            _ => at = next,
        }
    }
    let chosen = sheets
        .iter()
        .find(|(_, _, name)| !worksheet.is_empty() && name == worksheet)
        .or_else(|| sheets.get(sheet))
        .ok_or_else(bad)?;
    // A worksheet (dt 0) only: chart, macro and module sheets have no rows.
    if chosen.1 != 0 {
        return Err(bad());
    }
    sheet_rows(&stream, chosen.0, &strings, budget)
}

/// The rows of the sheet whose BOF is at `at`.
fn sheet_rows(
    stream: &[u8],
    mut at: usize,
    strings: &[String],
    budget: &mut Budget,
) -> Result<Vec<SourceRow>> {
    let (kind, data, next) = record(stream, at).ok_or_else(bad)?;
    if kind != BOF || u16_at(data, 0) != Some(0x0600) || u16_at(data, 2) != Some(0x0010) {
        return Err(bad());
    }
    at = next;
    let mut rows: Vec<SourceRow> = Vec::new();
    // A formula whose cached result is a string: its STRING record follows.
    let mut pending: Option<(usize, usize)> = None;
    // The substreams a worksheet holds (an embedded chart's, with its own
    // BOF and EOF and cell-like records of its cached data) are skipped
    // whole: they are not the sheet's cells.
    let mut nested = 0usize;
    loop {
        let (kind, data, next) = record(stream, at).ok_or_else(bad)?;
        match kind {
            BOF => {
                nested += 1;
                at = next;
                continue;
            }
            EOF if nested > 0 => {
                nested -= 1;
                at = next;
                continue;
            }
            _ if nested > 0 => {
                at = next;
                continue;
            }
            WSBOOL if u16_at(data, 0).is_some_and(|flags| flags & DIALOG != 0) => {
                return Err(bad());
            }
            _ => {}
        }
        let cell = |data: &[u8]| -> Result<(usize, usize)> {
            Ok((
                usize::from(u16_at(data, 0).ok_or_else(bad)?),
                usize::from(u16_at(data, 2).ok_or_else(bad)?),
            ))
        };
        let mut put: Vec<((usize, usize), String)> = Vec::new();
        let mut after = next;
        match kind {
            EOF => break,
            ROW => {
                let row = usize::from(u16_at(data, 0).ok_or_else(bad)?);
                grow(&mut rows, row, budget)?;
            }
            LABELSST => {
                let index = u32_at(data, 6).ok_or_else(bad)? as usize;
                let text = strings.get(index).ok_or_else(bad)?;
                put.push((cell(data)?, text.clone()));
            }
            LABEL => {
                let mut text = Pieces::new(stream, at).ok_or_else(bad)?;
                text.skip(6)?;
                put.push((cell(data)?, text.string()?));
                after = text.end();
            }
            NUMBER => {
                let value = f64::from_bits(u64_at(data, 6).ok_or_else(bad)?);
                put.push((cell(data)?, number_text(value)));
            }
            RK => {
                let rk = u32_at(data, 6).ok_or_else(bad)?;
                put.push((cell(data)?, number_text(rk_value(rk))));
            }
            MULRK => {
                let (row, first) = cell(data)?;
                let count = data.len().saturating_sub(6) / 6;
                for i in 0..count {
                    let rk = u32_at(data, 4 + 6 * i + 2).ok_or_else(bad)?;
                    put.push(((row, first + i), number_text(rk_value(rk))));
                }
            }
            BOOLERR => {
                let value = *data.get(6).ok_or_else(bad)?;
                let error = *data.get(7).ok_or_else(bad)?;
                let text = match error {
                    0 => if value != 0 { "1" } else { "0" }.to_owned(),
                    _ => error_text(value).to_owned(),
                };
                put.push((cell(data)?, text));
            }
            FORMULA => {
                let result = data.get(6..14).ok_or_else(bad)?;
                let position = cell(data)?;
                if result[6..8] == [0xff, 0xff] {
                    match result[0] {
                        0 => pending = Some(position),
                        1 => put.push((position, if result[2] != 0 { "1" } else { "0" }.into())),
                        2 => put.push((position, error_text(result[2]).into())),
                        _ => put.push((position, String::new())),
                    }
                } else {
                    let value = f64::from_bits(u64_at(result, 0).ok_or_else(bad)?);
                    put.push((position, number_text(value)));
                }
            }
            STRING => {
                let mut text = Pieces::new(stream, at).ok_or_else(bad)?;
                let value = text.string()?;
                if let Some(position) = pending.take() {
                    put.push((position, value));
                }
                after = text.end();
            }
            _ => {}
        }
        for ((row, column), text) in put {
            budget.columns(column + 1)?;
            grow(&mut rows, row, budget)?;
            let v = vec![text];
            budget.value(&v)?;
            let cells = &mut rows[row];
            if cells.len() <= column {
                budget.empty_values(column + 1 - cells.len())?;
                cells.resize_with(column + 1, Vec::new);
            }
            cells[column] = v;
        }
        at = after;
    }
    Ok(rows)
}

/// Rows up to `row`, each charged.
fn grow(rows: &mut Vec<SourceRow>, row: usize, budget: &mut Budget) -> Result<()> {
    if row >= limit::SHEET_ROWS {
        return Err(bad());
    }
    while rows.len() <= row {
        budget.row()?;
        rows.push(Vec::new());
    }
    Ok(())
}
