//! Walking the log on disk: from a segment's start, batch by batch and
//! entry by entry, each entry checked by the hash chain as it is read
//! (`docs/server-storage-format.md` section 3.5). Opening, reading and
//! cutting all walk the log this way; what they make of where it ends is
//! theirs.
//!
//! Each place the walk decides on is read once, into one buffer, in steps
//! that each go only as far as the decision at hand needs; every check,
//! every field returned and every byte the open writes back comes from that
//! buffer (format section 3.11, "Whatever is checked is what is used").

use crate::sys::{self, ErrorKind, File};

use super::format::{
    entry_hash, is_batch_mark, BatchHeader, BatchStart, EntryHead, SegmentHeader, BATCH_HEADER,
    BATCH_MARK, BATCH_START, ENTRY_FIXED, ENTRY_OVERHEAD, SEGMENT_HEADER,
};

/// A listed segment, its header checked.
pub(crate) struct Seg {
    pub number: u64,
    pub header: SegmentHeader,
    pub file: Box<dyn File>,
}

/// The largest buffer a walk keeps for its next place; a larger one, read
/// for a large entry, is let go with it.
const KEEP_BUFFER: usize = 64 * 1024;

/// The bytes read at one place of a segment, from its offset on. No byte of
/// a place is read twice, and nothing is read past what is asked for or
/// past the segment's size (format section 3.5): a place the device cannot
/// read is met only by a decision that needs it.
struct Place {
    offset: u64,
    buf: Vec<u8>,
    /// The file ended before a read did.
    short: bool,
}

impl Place {
    /// The place's first `len` bytes, or fewer where the segment (`size`)
    /// or the file ends: those read before, and the rest, read now. A
    /// failed read gives its offset and error.
    fn get(&mut self, file: &dyn File, size: u64, len: u64) -> Result<&[u8], (u64, sys::Error)> {
        let want = self
            .offset
            .saturating_add(len)
            .min(size)
            .saturating_sub(self.offset);
        let want = usize::try_from(want).expect("bounded by a segment's size");
        let have = self.buf.len();
        if want > have && !self.short {
            self.buf.resize(want, 0);
            let at = self.offset + have as u64;
            match read_exact(file, at, &mut self.buf[have..]) {
                Ok(n) => {
                    self.short = n < want - have;
                    self.buf.truncate(have + n);
                }
                Err(error) => {
                    self.buf.truncate(have);
                    return Err((at, error));
                }
            }
        }
        Ok(&self.buf[..want.min(self.buf.len())])
    }
}

/// Read up to `buf.len()` bytes at `offset`, as far as the file reaches.
pub(crate) fn read_exact(file: &dyn File, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        let n = file.read_at(offset + done as u64, &mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(done)
}

/// What a walk keeps of the entries it reads, besides their fields.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Keep {
    Nothing,
    /// Their payloads (a read).
    Payloads,
    /// The whole bytes of the entries after this index, as verified (the
    /// open, which writes them back).
    BytesAfter(u64),
}

/// The open's rule after its firm position (format section 3.6): a batch
/// written under another cut count than `cut_count` counts only up to
/// `firm`, so nothing of it past `firm` is read beyond what shows that.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Judge {
    pub firm: u64,
    pub cut_count: u64,
}

/// An entry the walk found whole and chained. Every field comes from the
/// bytes its hash was checked over.
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
    /// Its hash was checked over its bytes, chained. An entry the walk
    /// took by its framing alone, at or before the entry it is anchored at
    /// ([`Scanner::anchored_at`]), was not; the anchor's own hash and term
    /// are then the anchor's, not what its bytes say.
    pub verified: bool,
    /// The entry's bytes as verified, when the walk keeps them.
    pub bytes: Vec<u8>,
    /// Where its batch's header is.
    pub batch_offset: u64,
    /// It is its batch's first entry.
    pub first_in_batch: bool,
}

/// Where the chain of entries before a walk's anchor stopped holding: the
/// segment (an index into the run), the offset, the first entry from which
/// it does not hold, and whether the device failed to read there (else the
/// bytes do not check).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChainStop {
    pub seg: usize,
    pub offset: u64,
    pub index: u64,
    pub unreadable: bool,
}

/// What the walk met next.
#[derive(Debug)]
pub(crate) enum Event {
    /// A batch begins at `offset` of segment `seg`, written under
    /// `cut_count`. `header` is its header's bytes as verified; `None` for
    /// a batch of another cut count than the open's after its firm
    /// position, of whose header only the start was read and of which
    /// nothing more is read.
    Batch {
        seg: usize,
        offset: u64,
        cut_count: u64,
        header: Option<[u8; BATCH_HEADER as usize]>,
    },
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

/// A read error, at a place in the log: the offset of the read that
/// failed.
#[derive(Debug)]
pub(crate) struct ReadFault {
    pub seg: usize,
    pub offset: u64,
    pub error: sys::Error,
}

/// What a look for a batch header at a place found.
#[derive(Debug)]
enum Probe {
    /// A valid header that continues the chain, and its bytes.
    Header(BatchHeader, [u8; BATCH_HEADER as usize]),
    /// The start of a header that continues the chain after the open's firm
    /// position, written under another cut count: nothing more of it is
    /// read.
    Stale(BatchStart),
    /// A batch's mark, and no valid header that continues the log.
    Marked,
    Nothing,
}

/// The batch being read.
struct OpenBatch {
    offset: u64,
    count: u32,
    length: u64,
    /// The hash its header names for its last entry; `None` for a batch of
    /// which only the start of its header was read.
    last: Option<[u8; 32]>,
    read: u32,
    /// No entry past this index is read: the batch was written before the
    /// last cut, and counts only up to the firm position.
    read_no_further_than: Option<u64>,
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
    pending: Option<(Probe, u64)>,
    /// The walk ends once the batch it is in is reported.
    ending: Option<(usize, u64)>,
    ended: bool,
    keep: Keep,
    judge: Option<Judge>,
    /// The entry the walk is anchored at, while the walk has not passed
    /// it: index, hash and term. Until then entries are taken by their
    /// framing alone (format section 3.5, "Anchored walks").
    anchor: Option<(u64, [u8; 32], u64)>,
    /// The walk ended where a batch still had an entry to come, or where a
    /// batch's mark is and no valid header continues the log.
    broke: bool,
    /// The walk ended between batches, where a batch header was looked for.
    between: bool,
    /// Where the anchor ends is known: the bytes where its framing places
    /// its hash hold the anchor's hash (or the walk began after it).
    placed: bool,
    /// Before the anchor, the hash of the last entry taken while the chain
    /// from the segment's header still holds; `None` once it stopped.
    chain: Option<[u8; 32]>,
    chain_fault: Option<ChainStop>,
    /// A buffer for the next place.
    spare: Vec<u8>,
}

impl<'a> Scanner<'a> {
    /// A walk from the start of `segs[seg]`, chained from its header.
    pub fn new(segs: &'a [Seg], seg: usize, keep: Keep) -> Self {
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
            keep,
            judge: None,
            anchor: None,
            broke: false,
            between: false,
            placed: true,
            chain: None,
            chain_fault: None,
            spare: Vec::new(),
        }
    }

    /// Anchor the walk at entry `index`, whose hash and term are `hash` and
    /// `term` (the entry a checkpoint covers): the entries up to it are
    /// taken by their framing alone (their fixed fields, the lengths of
    /// entries and batches, a batch header's own hash), not by the chain,
    /// since nothing before the anchor is needed after it; the entry after
    /// it is checked by the chain from `hash`. Nothing changes when the
    /// walk begins after the anchor.
    pub fn anchored_at(mut self, index: u64, hash: [u8; 32], term: u64) -> Self {
        if self.next <= index {
            self.anchor = Some((index, hash, term));
            self.chain = Some(self.prev);
            self.placed = false;
        }
        self
    }

    /// Whether the walk knows where the anchor ends, and so where the entry
    /// after it begins: the anchor's bytes end with the anchor's hash where
    /// its framing places it, or the walk began after the anchor. An anchor
    /// whose length was damaged is placed wrong and does not end so.
    pub fn anchor_placed(&self) -> bool {
        self.placed
    }

    /// Whether the walk has not yet passed the entry it is anchored at.
    pub fn before_anchor(&self) -> bool {
        self.anchor.is_some()
    }

    /// Whether the walk ended where its framing broke: a batch still had an
    /// entry to come, or a batch's mark stood where no valid header
    /// continued the log.
    pub fn broke(&self) -> bool {
        self.broke
    }

    /// Whether the walk ended between batches, where it looked for a batch
    /// header (else inside a batch, where an entry was to come).
    pub fn ended_between(&self) -> bool {
        self.between
    }

    /// Walk under the open's rule after its firm position (see [`Judge`]).
    pub fn judging(mut self, judge: Judge) -> Self {
        self.judge = Some(judge);
        self
    }

    /// The index the next entry would have, and the hash and term of the
    /// last entry read (or the one before the first segment walked).
    pub fn position(&self) -> (u64, [u8; 32], u64) {
        (self.next, self.prev, self.prev_term)
    }

    fn place(&mut self, offset: u64) -> Place {
        let mut buf = std::mem::take(&mut self.spare);
        buf.clear();
        Place {
            offset,
            buf,
            short: false,
        }
    }

    fn recycle(&mut self, place: Place) {
        if place.buf.capacity() <= KEEP_BUFFER {
            self.spare = place.buf;
        }
    }

    /// The first `len` bytes of `place` in the current segment.
    fn get<'p>(&self, place: &'p mut Place, len: u64) -> Result<&'p [u8], ReadFault> {
        let seg = &self.segs[self.seg];
        place
            .get(&*seg.file, seg.header.size, len)
            .map_err(|(offset, error)| ReadFault {
                seg: self.seg,
                offset,
                error,
            })
    }

    /// Whether a batch header that continues the chain begins at `place`,
    /// decided from what must be there (format section 3.6): no batch
    /// begins where fewer bytes are left than a header and one entry of no
    /// payload, and none where the mark is not; then the rest of the
    /// header's start must name the index expected and the entry just read,
    /// and fit in the segment; then, under the open's rule, a header of
    /// another cut count after the firm position is settled; only then is
    /// the rest of the header read and its hash checked. Each step reads
    /// only what it needs, so that a read fails only where a decision
    /// depends on the bytes it could not read.
    fn probe(&self, place: &mut Place) -> Result<Probe, ReadFault> {
        let size = self.segs[self.seg].header.size;
        let offset = place.offset;
        if size.saturating_sub(offset) < BATCH_HEADER + ENTRY_OVERHEAD {
            return Ok(Probe::Nothing);
        }
        if !is_batch_mark(self.get(place, BATCH_MARK)?) {
            return Ok(Probe::Nothing);
        }
        let Some(start) = BatchStart::decode(self.get(place, BATCH_START)?) else {
            return Ok(Probe::Marked);
        };
        let fits = offset
            .checked_add(BATCH_HEADER)
            .and_then(|o| o.checked_add(start.length))
            .is_some_and(|end| end <= size);
        // Before the anchor the chain is not followed: the batch must only
        // begin at the index expected.
        let chained = self.anchor.is_some() || start.prev == self.prev;
        if start.first != self.next || !chained || !fits {
            return Ok(Probe::Marked);
        }
        if let Some(judge) = self.judge {
            if start.first > judge.firm && start.cut_count != judge.cut_count {
                return Ok(Probe::Stale(start));
            }
        }
        let bytes = self.get(place, BATCH_HEADER)?;
        Ok(match BatchHeader::decode(bytes) {
            Some(header) => Probe::Header(
                header,
                bytes[..BATCH_HEADER as usize]
                    .try_into()
                    .expect("a whole header"),
            ),
            None => Probe::Marked,
        })
    }

    /// The entry at `place`, if it is whole, the next one, chained, and
    /// within `limit`. Its fixed fields are checked (index and payload
    /// length within the format's bounds, the entry within the batch)
    /// before anything is read for its payload, so that no length read from
    /// disk sizes more than [`super::format::MAX_PAYLOAD`] bytes; then the
    /// rest of it is read, once, and its hash checked over the bytes its
    /// fields were decoded from.
    fn entry(&mut self, mut place: Place, limit: u64) -> Result<Option<Found>, ReadFault> {
        let offset = place.offset;
        let fixed = self.get(&mut place, ENTRY_FIXED)?;
        let head = if (fixed.len() as u64) < ENTRY_FIXED {
            None
        } else {
            EntryHead::decode(fixed)
                .filter(|h| h.index == self.next && offset.saturating_add(h.size()) <= limit)
        };
        let Some(head) = head else {
            self.recycle(place);
            return Ok(None);
        };
        let size = head.size();
        let content = (ENTRY_FIXED + u64::from(head.len)) as usize;
        if let Some(anchor) = self.anchor.filter(|(at, _, _)| head.index <= *at) {
            return self.framed(place, &head, offset, size, content, anchor);
        }
        let bytes = self.get(&mut place, size)?;
        if (bytes.len() as u64) < size {
            self.recycle(place);
            return Ok(None);
        }
        let hash = entry_hash(&self.prev, &bytes[..content]);
        if hash[..] != bytes[content..] {
            self.recycle(place);
            return Ok(None);
        }
        let (term, verified) = (head.term, true);
        let (payload, bytes) = match self.keep {
            Keep::Payloads => {
                let mut buf = std::mem::take(&mut place.buf);
                buf.truncate(content);
                buf.drain(..ENTRY_FIXED as usize);
                (buf, Vec::new())
            }
            Keep::BytesAfter(index) if head.index > index => {
                let mut buf = std::mem::take(&mut place.buf);
                buf.truncate(size as usize);
                (Vec::new(), buf)
            }
            _ => {
                self.recycle(place);
                (Vec::new(), Vec::new())
            }
        };
        let batch = self
            .batch
            .as_ref()
            .expect("entries are read inside a batch");
        Ok(Some(Found {
            seg: self.seg,
            offset,
            size,
            index: head.index,
            term,
            kind: head.kind,
            file: head.file,
            hash,
            payload,
            bytes,
            verified,
            batch_offset: batch.offset,
            first_in_batch: batch.read == 0,
        }))
    }

    /// An entry at or before the anchor, taken by its framing: its fixed
    /// fields placed it. Its bytes are still read and checked by the chain
    /// as far as the chain holds from the segment's header, and the first
    /// place it does not hold (bytes that do not check, a read the device
    /// fails) is noted ([`Scanner::chain_fault`]); the walk goes on either
    /// way. The anchor takes the anchor's hash and term; an entry before it,
    /// the hash its bytes name.
    fn framed(
        &mut self,
        mut place: Place,
        head: &EntryHead,
        offset: u64,
        size: u64,
        content: usize,
        (at, anchor_hash, anchor_term): (u64, [u8; 32], u64),
    ) -> Result<Option<Found>, ReadFault> {
        let read = match self.get(&mut place, size) {
            Ok(bytes) if (bytes.len() as u64) < size => None,
            Ok(bytes) => {
                let mut stored = [0u8; 32];
                stored.copy_from_slice(&bytes[content..]);
                let computed = self.chain.map(|prev| entry_hash(&prev, &bytes[..content]));
                Some(Ok((stored, computed)))
            }
            // Only a read the device failed is noted and passed; any other
            // error (a stopped process) is the walk's.
            Err(fault) if fault.error.kind() == ErrorKind::Read => Some(Err(fault)),
            Err(fault) => {
                self.recycle(place);
                return Err(fault);
            }
        };
        self.recycle(place);
        let stored = match read {
            // The file ends inside it: not whole, and nothing after it.
            None => return Ok(None),
            Some(Err(fault)) => {
                self.chain_stops(fault.offset, head.index, true);
                [0u8; 32]
            }
            Some(Ok((stored, computed))) => {
                if head.index == at {
                    self.placed = stored == anchor_hash;
                }
                let holds = computed == Some(stored) && (head.index != at || stored == anchor_hash);
                if holds {
                    self.chain = Some(stored);
                } else {
                    self.chain_stops(offset, head.index, false);
                }
                stored
            }
        };
        let (hash, term) = if head.index == at {
            (anchor_hash, anchor_term)
        } else {
            (stored, head.term)
        };
        let batch = self
            .batch
            .as_ref()
            .expect("entries are read inside a batch");
        Ok(Some(Found {
            seg: self.seg,
            offset,
            size,
            index: head.index,
            term,
            kind: head.kind,
            file: head.file,
            hash,
            payload: Vec::new(),
            bytes: Vec::new(),
            verified: false,
            batch_offset: batch.offset,
            first_in_batch: batch.read == 0,
        }))
    }

    /// The chain before the anchor stops holding at `offset`, at entry
    /// `index`: noted, the first time.
    fn chain_stops(&mut self, offset: u64, index: u64, unreadable: bool) {
        if self.chain.take().is_some() {
            self.chain_fault = Some(ChainStop {
                seg: self.seg,
                offset,
                index,
                unreadable,
            });
        }
    }

    /// Where the chain of the entries the walk took by their framing, before
    /// its anchor, stopped holding from the segment's header: `None` when it
    /// held through the anchor, or when the walk took none so.
    pub fn chain_fault(&self) -> Option<ChainStop> {
        self.chain_fault
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

    /// The current batch ends at `offset`, and the walk with it.
    fn end_batch_here(&mut self, offset: u64) -> Event {
        self.broke = true;
        self.batch = None;
        self.ending = Some((self.seg, offset));
        Event::BatchEnd { whole: false }
    }

    pub fn next_event(&mut self) -> Result<Event, ReadFault> {
        if self.ended {
            return Ok(Event::End {
                seg: self.seg,
                offset: self.offset,
            });
        }
        if let Some(batch) = &self.batch {
            let (batch_offset, count, length, last, read, no_further) = (
                batch.offset,
                batch.count,
                batch.length,
                batch.last,
                batch.read,
                batch.read_no_further_than,
            );
            // A batch of which only the start of its header was read:
            // nothing of it is read, and the walk ends at it.
            let Some(last) = last else {
                return Ok(self.end_batch_here(batch_offset));
            };
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
            let limit = batch_offset + BATCH_HEADER + length;
            let mut place = self.place(offset);
            if no_further.is_some_and(|last| self.next > last)
                || limit.saturating_sub(offset) < ENTRY_OVERHEAD
            {
                // No entry of the batch is read here: it was written before
                // the last cut and this is past the firm position, or it
                // has no room left for one. Only a header that ends the
                // batch (a cut, and the batch written after it) counts
                // here, and that shows in its mark first.
                let probe = self.probe(&mut place)?;
                self.recycle(place);
                return Ok(match probe {
                    Probe::Nothing | Probe::Marked => self.end_batch_here(offset),
                    found => {
                        self.batch = None;
                        self.pending = Some((found, offset));
                        Event::BatchEnd { whole: false }
                    }
                });
            }
            // The next entry, or a header that ends the batch: both begin
            // here, with the bytes an entry's fixed fields take. A header
            // is looked for only when they begin with its mark.
            if is_batch_mark(self.get(&mut place, ENTRY_FIXED)?) {
                match self.probe(&mut place)? {
                    Probe::Nothing | Probe::Marked => {}
                    found => {
                        self.recycle(place);
                        self.batch = None;
                        self.pending = Some((found, offset));
                        return Ok(Event::BatchEnd { whole: false });
                    }
                }
            }
            return Ok(match self.entry(place, limit)? {
                Some(found) => {
                    self.offset += found.size;
                    // An entry's index is at most `MAX_INDEX`, far below
                    // the largest number: the next index never wraps.
                    self.next = found.index + 1;
                    self.prev = found.hash;
                    self.prev_term = found.term;
                    if self.anchor.is_some_and(|(at, _, _)| found.index >= at) {
                        self.anchor = None;
                    }
                    if let Some(batch) = &mut self.batch {
                        batch.read += 1;
                    }
                    Event::Entry(found)
                }
                None => self.end_batch_here(offset),
            });
        }
        if let Some((seg, offset)) = self.ending.take() {
            self.ended = true;
            self.offset = offset;
            return Ok(Event::End { seg, offset });
        }
        let begin = match self.pending.take() {
            Some(found) => found,
            None if self.next_segment_continues() => {
                self.seg += 1;
                self.offset = SEGMENT_HEADER;
                return Ok(Event::Segment { seg: self.seg });
            }
            None => {
                let offset = self.offset;
                let mut place = self.place(offset);
                let probe = self.probe(&mut place)?;
                self.recycle(place);
                (probe, offset)
            }
        };
        let firm = self.judge.map(|j| j.firm);
        match begin {
            (Probe::Header(header, bytes), offset) => {
                // Before the anchor, the chain also runs through the
                // batch's header.
                if self.anchor.is_some() && self.chain.is_some_and(|prev| prev != header.prev) {
                    self.chain_stops(offset, header.first, false);
                }
                let stale = self.judge.is_some_and(|j| header.cut_count != j.cut_count);
                self.batch = Some(OpenBatch {
                    offset,
                    count: header.count,
                    length: header.length,
                    last: Some(header.last),
                    read: 0,
                    read_no_further_than: if stale { firm } else { None },
                });
                self.offset = offset + BATCH_HEADER;
                Ok(Event::Batch {
                    seg: self.seg,
                    offset,
                    cut_count: header.cut_count,
                    header: Some(bytes),
                })
            }
            (Probe::Stale(start), offset) => {
                self.batch = Some(OpenBatch {
                    offset,
                    count: start.count,
                    length: start.length,
                    last: None,
                    read: 0,
                    read_no_further_than: firm,
                });
                self.offset = offset;
                Ok(Event::Batch {
                    seg: self.seg,
                    offset,
                    cut_count: start.cut_count,
                    header: None,
                })
            }
            (probe @ (Probe::Nothing | Probe::Marked), _) => {
                self.broke |= matches!(probe, Probe::Marked);
                self.between = true;
                self.ended = true;
                Ok(Event::End {
                    seg: self.seg,
                    offset: self.offset,
                })
            }
        }
    }
}
