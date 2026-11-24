//! Walking the log on disk: from a segment's start, batch by batch and
//! entry by entry, each entry checked by the hash chain as it is read
//! (`docs/server-storage-format.md` section 3.5). Opening, reading and
//! cutting all walk the log this way; what they make of where it ends is
//! theirs.

use crate::sys::{self, File};

use super::format::{
    entry_hash, BatchHeader, EntryHead, SegmentHeader, BATCH_HEADER, ENTRY_FIXED, HASH,
    SEGMENT_HEADER,
};

/// A listed segment, its header checked.
pub(crate) struct Seg {
    pub number: u64,
    pub header: SegmentHeader,
    pub file: Box<dyn File>,
}

/// How much a cursor reads at a time.
const CHUNK: usize = 64 * 1024;

/// Buffered reads of one segment, never past its size.
struct Cursor {
    buf: Vec<u8>,
    /// The offset of `buf[0]`.
    at: u64,
}

impl Cursor {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            at: 0,
        }
    }

    /// The bytes from `offset` to `offset + len` of `file`, or fewer when the
    /// file or the segment (`size`) ends first.
    fn get(&mut self, file: &dyn File, size: u64, offset: u64, len: u64) -> sys::Result<&[u8]> {
        let end = offset.saturating_add(len).min(size);
        if offset >= end {
            return Ok(&[]);
        }
        let have = self.at + self.buf.len() as u64;
        if offset < self.at || end > have {
            let want = (end - offset).max(CHUNK as u64).min(size - offset);
            self.buf.resize(want as usize, 0);
            let mut done = 0;
            while done < self.buf.len() {
                let n = file.read_at(offset + done as u64, &mut self.buf[done..])?;
                if n == 0 {
                    break;
                }
                done += n;
            }
            self.buf.truncate(done);
            self.at = offset;
        }
        let from = (offset - self.at) as usize;
        let to = ((end - self.at) as usize).min(self.buf.len());
        Ok(&self.buf[from.min(to)..to])
    }
}

/// An entry the walk found whole and chained.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    /// The segment, as an index into the run.
    pub seg: usize,
    pub offset: u64,
    /// The bytes the entry takes.
    pub size: u64,
    pub index: u64,
    pub term: u64,
    pub kind: u16,
    pub file: u64,
    pub hash: [u8; 32],
    /// The payload, when the walk keeps payloads.
    pub payload: Vec<u8>,
    /// Where its batch's header is.
    pub batch_offset: u64,
    /// It is its batch's first entry.
    pub first_in_batch: bool,
}

/// What the walk met next.
#[derive(Debug)]
pub(crate) enum Event {
    /// A batch begins.
    Batch,
    Entry(Found),
    /// The batch ended: whole when all its entries were read, its last
    /// entry has the hash its header names and they fill its length.
    BatchEnd {
        whole: bool,
    },
    /// The walk moved to the next listed segment, whose header continues
    /// the chain.
    Segment {
        seg: usize,
    },
    /// Nothing valid follows at `seg` and `offset`: no batch continues the
    /// log there and no listed segment does, or a batch's entry there is
    /// missing, damaged or not chained.
    End {
        seg: usize,
        offset: u64,
    },
}

/// A read error, at a place in the log.
#[derive(Debug)]
pub(crate) struct ReadFault {
    pub seg: usize,
    pub offset: u64,
    pub error: sys::Error,
}

/// The batch being read.
struct OpenBatch {
    header: BatchHeader,
    offset: u64,
    read: u32,
}

/// A walk of the log from the start of one listed segment.
pub(crate) struct Scanner<'a> {
    segs: &'a [Seg],
    seg: usize,
    offset: u64,
    /// The index the next entry must have, and the hash and term of the
    /// entry before it.
    next: u64,
    prev: [u8; 32],
    prev_term: u64,
    batch: Option<OpenBatch>,
    /// A batch header found where an entry was expected, to begin once the
    /// batch it ends is reported.
    pending: Option<(BatchHeader, u64)>,
    /// The walk ends once the batch it is in is reported.
    ending: Option<(usize, u64)>,
    ended: bool,
    cursor: Cursor,
    keep_payload: bool,
}

impl<'a> Scanner<'a> {
    /// A walk from the start of `segs[seg]`, chained from its header.
    pub fn new(segs: &'a [Seg], seg: usize, keep_payload: bool) -> Self {
        let header = &segs[seg].header;
        Self {
            segs,
            seg,
            offset: SEGMENT_HEADER,
            next: header.first,
            prev: header.prev_hash,
            prev_term: header.prev_term,
            batch: None,
            pending: None,
            ending: None,
            ended: false,
            cursor: Cursor::new(),
            keep_payload,
        }
    }

    /// The index the next entry would have, and the hash and term of the
    /// last entry read (or the one before the first segment walked).
    pub fn position(&self) -> (u64, [u8; 32], u64) {
        (self.next, self.prev, self.prev_term)
    }

    /// The bytes at `offset` of the current segment, up to `len`.
    fn bytes(&mut self, offset: u64, len: u64) -> Result<&[u8], ReadFault> {
        let seg = &self.segs[self.seg];
        let size = seg.header.size;
        match self.cursor.get(&*seg.file, size, offset, len) {
            Ok(bytes) => Ok(bytes),
            Err(error) => Err(ReadFault {
                seg: self.seg,
                offset,
                error,
            }),
        }
    }

    /// A valid batch header at `offset` whose first entry is the next one.
    fn header_at(&mut self, offset: u64) -> Result<Option<BatchHeader>, ReadFault> {
        let size = self.segs[self.seg].header.size;
        let next = self.next;
        let bytes = self.bytes(offset, BATCH_HEADER)?;
        Ok(BatchHeader::decode(bytes).filter(|h| {
            h.first == next
                && offset
                    .checked_add(BATCH_HEADER)
                    .and_then(|o| o.checked_add(h.length))
                    .is_some_and(|end| end <= size)
        }))
    }

    /// A whole entry at `offset` that is the next one and continues the
    /// chain, within `limit`.
    fn entry_at(&mut self, offset: u64, limit: u64) -> Result<Option<Found>, ReadFault> {
        let fixed = self.bytes(offset, ENTRY_FIXED)?;
        if (fixed.len() as u64) < ENTRY_FIXED {
            return Ok(None);
        }
        let Some(head) = EntryHead::decode(fixed) else {
            return Ok(None);
        };
        if head.index != self.next || offset.saturating_add(head.size()) > limit {
            return Ok(None);
        }
        let content_len = ENTRY_FIXED + u64::from(head.len);
        let prev = self.prev;
        let keep = self.keep_payload;
        let bytes = self.bytes(offset, content_len + HASH)?;
        if (bytes.len() as u64) < content_len + HASH {
            return Ok(None);
        }
        let content = &bytes[..content_len as usize];
        let hash = entry_hash(&prev, content);
        if hash[..] != bytes[content_len as usize..] {
            return Ok(None);
        }
        let payload = if keep {
            content[ENTRY_FIXED as usize..].to_vec()
        } else {
            Vec::new()
        };
        let batch = self
            .batch
            .as_ref()
            .expect("entries are read inside a batch");
        Ok(Some(Found {
            seg: self.seg,
            offset,
            size: head.size(),
            index: head.index,
            term: head.term,
            kind: head.kind,
            file: head.file,
            hash,
            payload,
            batch_offset: batch.offset,
            first_in_batch: batch.read == 0,
        }))
    }

    /// Whether the next listed segment's header continues the chain from
    /// here: the writer moved on to it at this point, so nothing after this
    /// point in the current segment is part of the log.
    fn next_segment_continues(&self) -> bool {
        self.segs.get(self.seg + 1).is_some_and(|s| {
            s.header.first == self.next
                && s.header.prev_hash == self.prev
                && s.header.prev_term == self.prev_term
        })
    }

    pub fn next_event(&mut self) -> Result<Event, ReadFault> {
        if self.ended {
            return Ok(Event::End {
                seg: self.seg,
                offset: self.offset,
            });
        }
        if let Some((count, read, batch_offset, length, last)) = self.batch.as_ref().map(|b| {
            (
                b.header.count,
                b.read,
                b.offset,
                b.header.length,
                b.header.last,
            )
        }) {
            if read == count {
                let whole = self.prev == last
                    && Some(self.offset)
                        == batch_offset
                            .checked_add(BATCH_HEADER)
                            .and_then(|o| o.checked_add(length));
                self.batch = None;
                return Ok(Event::BatchEnd { whole });
            }
            if self.next_segment_continues() {
                self.batch = None;
                return Ok(Event::BatchEnd { whole: false });
            }
            let offset = self.offset;
            // A cut and the batch written after it: a header where an
            // entry was expected, which begins with the next entry.
            if let Some(header) = self.header_at(offset)? {
                self.batch = None;
                self.pending = Some((header, offset));
                return Ok(Event::BatchEnd { whole: false });
            }
            let limit = batch_offset + BATCH_HEADER + length;
            match self.entry_at(offset, limit)? {
                Some(found) => {
                    self.offset += found.size;
                    self.next += 1;
                    self.prev = found.hash;
                    self.prev_term = found.term;
                    if let Some(batch) = &mut self.batch {
                        batch.read += 1;
                    }
                    return Ok(Event::Entry(found));
                }
                None => {
                    self.batch = None;
                    self.ending = Some((self.seg, offset));
                    return Ok(Event::BatchEnd { whole: false });
                }
            }
        }
        if let Some((seg, offset)) = self.ending.take() {
            self.ended = true;
            self.offset = offset;
            return Ok(Event::End { seg, offset });
        }
        let begin = match self.pending.take() {
            Some(found) => Some(found),
            None if self.next_segment_continues() => {
                self.seg += 1;
                self.offset = SEGMENT_HEADER;
                self.cursor = Cursor::new();
                return Ok(Event::Segment { seg: self.seg });
            }
            None => {
                let offset = self.offset;
                self.header_at(offset)?.map(|h| (h, offset))
            }
        };
        match begin {
            Some((header, offset)) => {
                self.batch = Some(OpenBatch {
                    header,
                    offset,
                    read: 0,
                });
                self.offset = offset + BATCH_HEADER;
                Ok(Event::Batch)
            }
            None => {
                self.ended = true;
                Ok(Event::End {
                    seg: self.seg,
                    offset: self.offset,
                })
            }
        }
    }
}
