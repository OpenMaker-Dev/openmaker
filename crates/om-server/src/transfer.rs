//! Paged transfers (spec `docs/re/network-protocol.md` 5.6.6c).
//!
//! A slot stream larger than one reply is served in pages: the server keeps a
//! cursor on the connection and returns its handle; the client echoes the
//! resume block until a reply comes back with cursor 0. A request payload too
//! large for one request arrives the same way in parts. A handle is honoured
//! only on the connection that issued it, and only by the operation and file
//! that started the transfer.
//!
//! The connection authorizes every request, each part of an upload included,
//! before it reaches this table, so nothing is held for a request that may
//! not run. What one connection holds is limited. The limits are OpenMaker's
//! choice (FileMaker Server's are unmeasured), set where a real client never
//! reaches them: it runs one transfer at a time and finishes it before it
//! starts the next (om-session's loops; FileMaker Pro's `MultiDownload` and
//! `MultiUpload`, spec 5.6.6c), so any other transfer a connection holds is
//! one its client abandoned.
//!
//! - At most [`MAX_TRANSFERS`] transfers: starting another evicts the least
//!   recently used.
//! - The transfers other than the one in use hold at most [`MAX_IDLE`]
//!   bytes; the least recently used are evicted to stay within it. The one
//!   in use is never evicted: a download holds a copy of the file's own data,
//!   an upload is limited by the next rule.
//! - One upload holds at most [`MAX_UPLOAD`] bytes, and at most
//!   [`MAX_PRE_LOGIN_UPLOAD`] for a file not logged in to, where the only
//!   upload allowed is an op92 key list for a path readable before Login.
//! - The bytes of uploads held in parts are charged, as each part arrives,
//!   to the server's budget for what clients' uploads take
//!   ([`crate::uploads`]), shared with the trees read from them and the
//!   records clients stage. When a part does not fit what the client may
//!   still hold, the connection first evicts its own idle uploads, least
//!   recently used first. An idle upload on another connection of the same
//!   client address keeps counting until that connection starts another
//!   transfer or its session ends. Downloads are not charged.
//!
//! Continuing an evicted transfer, or one dropped with its file's session
//! ([`Transfers::drop_file`]: the file was closed, logged out of, or logged
//! in to again), and uploading past a limit are [`Refused`]: the connection
//! answers with a CORBA system exception, which the client reports as an
//! error. An upload refused part way is dropped whole, its charge given
//! back, and its cursor remembered like an evicted one. A cursor this connection never issued, or finished, gets the empty
//! last page FileMaker Server's code gives instead, which a client reads as
//! the end of the stream; an evicted cursor never does, so a client is
//! never handed a silently truncated result.
//!
//! Receiving an upload costs time linear in its size: the scan of the parts
//! received ([`UploadScan`]) is kept with the transfer.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;

use log::{info, warn};
use om_protocol::{page_cut, Resume, UploadScan, PAGE_LIMIT};
use om_rctransaction::Header;

use crate::uploads::{Charge, UploadMemory};

/// The most transfers one connection holds.
pub(crate) const MAX_TRANSFERS: usize = 8;

/// The most bytes the transfers other than the one in use hold.
pub(crate) const MAX_IDLE: usize = 64 << 20;

/// The largest upload: container data, which can be large (the whole
/// payload is held in memory until it is complete).
pub(crate) const MAX_UPLOAD: usize = 1 << 30;

/// The largest upload for a file not logged in to: an op92 key list for a
/// path readable before Login holds a few keys.
pub(crate) const MAX_PRE_LOGIN_UPLOAD: usize = 64 << 10;

/// How many evicted cursors are remembered, so that continuing one is
/// refused instead of answered as the end of its stream.
const EVICTED_REMEMBERED: usize = 64;

/// Who may continue a transfer: the operation and file that started it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    opcode: u8,
    file_id: u32,
}

impl Owner {
    /// The operation and file `header` names.
    pub fn of(header: &Header) -> Self {
        Self {
            opcode: header.opcode,
            file_id: header.file_id,
        }
    }
}

/// A request refused at a limit on what a connection or its client holds:
/// a transfer it will not hold or no longer holds, or an upload's tree or
/// records past what the client may hold ([`crate::uploads`]). The request
/// is answered with a CORBA system exception.
#[derive(Debug)]
pub(crate) struct Refused;

/// What a cursor handle refers to.
enum Transfer {
    /// A slot stream being paged out: the bytes not yet sent start at
    /// `offset`, where the open-list depth is `depth`.
    Download {
        stream: Vec<u8>,
        offset: usize,
        depth: i16,
    },
    /// A request payload arriving in parts, the scan of what arrived, and
    /// its bytes' charge on the server's budget.
    Upload {
        payload: Vec<u8>,
        scan: UploadScan,
        charge: Charge,
    },
}

impl Transfer {
    /// The bytes it holds.
    fn bytes(&self) -> usize {
        match self {
            Transfer::Download { stream, .. } => stream.len(),
            Transfer::Upload { payload, .. } => payload.len(),
        }
    }
}

/// A transfer in progress.
struct Held {
    owner: Owner,
    perform: u64,
    /// When it was last used, by the connection's own clock.
    used: u64,
    transfer: Transfer,
}

/// The outcome of receiving one part of an uploaded payload.
pub(crate) enum Upload {
    /// The payload is complete, or nothing can complete it (the operation
    /// then fails on it as on any malformed request); the operation can run
    /// on it. With the charge its bytes held, if they arrived in parts:
    /// keep it while the payload lives.
    Complete(Vec<u8>, Charge),
    /// More parts follow; answer with this resume block and no slots.
    Partial(Resume),
    /// The cursor is not an upload this operation started on this
    /// connection.
    Unknown,
}

/// The transfers open on one connection.
pub(crate) struct Transfers {
    /// The server's budget its uploads are charged to, and the client
    /// address they are charged under.
    memory: Arc<UploadMemory>,
    client: IpAddr,
    by_cursor: HashMap<u64, Held>,
    /// The bytes the transfers hold.
    bytes: usize,
    /// Advances each time a transfer is used.
    clock: u64,
    /// The cursors evicted most recently, oldest first.
    evicted: VecDeque<u64>,
}

/// The most slot bytes a reply may carry for this `maxreturn`.
pub(crate) fn page_limit(max_return: u32) -> usize {
    (max_return as usize).min(PAGE_LIMIT)
}

impl Transfers {
    /// No transfers, their uploads charged to `memory` under `client`.
    pub fn new(memory: &Arc<UploadMemory>, client: IpAddr) -> Self {
        Self {
            memory: memory.clone(),
            client,
            by_cursor: HashMap::new(),
            bytes: 0,
            clock: 0,
            evicted: VecDeque::new(),
        }
    }

    /// Serve the first page of `stream` for `owner`. `new_handle` mints a
    /// nonzero handle; it is called only when the stream does not fit one
    /// page.
    pub fn start(
        &mut self,
        owner: Owner,
        stream: Vec<u8>,
        limit: usize,
        mut new_handle: impl FnMut() -> u64,
    ) -> (Vec<u8>, Resume) {
        self.page(owner, stream, (0, 0), limit, || {
            (new_handle(), new_handle())
        })
    }

    /// Serve the next page of the download `resume` names, or `None` if
    /// `owner` has no such download on this connection. Refused when it was
    /// evicted.
    pub fn next(
        &mut self,
        owner: Owner,
        resume: &Resume,
        limit: usize,
    ) -> Result<Option<(Vec<u8>, Resume)>, Refused> {
        let Some(held) = self.remove(resume.cursor) else {
            return self.unknown(resume.cursor).map(|()| None);
        };
        match held.transfer {
            Transfer::Download {
                stream,
                offset,
                depth,
            } if held.owner == owner && held.perform == resume.perform => {
                let handles = || (held.perform, resume.cursor);
                Ok(Some(self.page(
                    owner,
                    stream,
                    (offset, depth),
                    limit,
                    handles,
                )))
            }
            transfer => {
                self.insert(resume.cursor, Held { transfer, ..held });
                Ok(None)
            }
        }
    }

    /// Cut the page at `offset`, where the open-list depth is `depth`, and
    /// keep the rest under `handles` (asked for only then) when it does not
    /// complete the stream.
    fn page(
        &mut self,
        owner: Owner,
        stream: Vec<u8>,
        (offset, depth): (usize, i16),
        limit: usize,
        handles: impl FnOnce() -> (u64, u64),
    ) -> (Vec<u8>, Resume) {
        let (end, depth) = page_cut(&stream, offset, limit, depth);
        let page = stream[offset..end].to_vec();
        if end == stream.len() {
            return (page, Resume::complete());
        }
        let (perform, cursor) = handles();
        let download = Transfer::Download {
            stream,
            offset: end,
            depth,
        };
        self.hold(owner, perform, cursor, download);
        let resume = Resume {
            perform,
            cursor,
            depth,
            copy_key: Vec::new(),
        };
        (page, resume)
    }

    /// Receive one part of an uploaded payload for `owner`: the first part
    /// (cursor 0) or a continuation of an upload it started on this
    /// connection. A payload still incomplete after `max` bytes is refused
    /// and dropped.
    pub fn upload(
        &mut self,
        owner: Owner,
        resume: &Resume,
        part: Vec<u8>,
        max: usize,
        mut new_handle: impl FnMut() -> u64,
    ) -> Result<Upload, Refused> {
        if !resume.has_more() {
            let none = Charge::none(&self.memory, self.client);
            // An op13 that stores a value sends no slots at all
            // (schema-path.md 3.3).
            if part.is_empty() {
                return Ok(Upload::Complete(part, none));
            }
            let mut scan = UploadScan::default();
            scan.scan(&part);
            if scan.complete() || scan.never_completes() {
                return Ok(Upload::Complete(part, none));
            }
            if part.len() > max {
                warn!(
                    "refusing to hold an upload part of {} bytes; the limit is {max}",
                    part.len()
                );
                return Err(Refused);
            }
            let mut charge = none;
            self.charge(&mut charge, part.len())?;
            return self.keep_upload(owner, new_handle(), new_handle(), (part, scan, charge));
        }
        let Some(held) = self.remove(resume.cursor) else {
            return self.unknown(resume.cursor).map(|()| Upload::Unknown);
        };
        let (mut payload, mut scan, mut charge) = match held.transfer {
            Transfer::Upload {
                payload,
                scan,
                charge,
            } if held.owner == owner && held.perform == resume.perform => (payload, scan, charge),
            transfer => {
                self.insert(resume.cursor, Held { transfer, ..held });
                return Ok(Upload::Unknown);
            }
        };
        // A part that adds nothing would only be answered with the same
        // continuation again: the client has no more, so the operation runs
        // on what arrived.
        if part.is_empty() {
            warn!("an upload continued with an empty part; ending it");
            return Ok(Upload::Complete(payload, charge));
        }
        if payload.len() + part.len() > max {
            warn!(
                "dropping an upload that would exceed {max} bytes ({} held, {} more)",
                payload.len(),
                part.len()
            );
            self.remember(resume.cursor);
            return Err(Refused);
        }
        // Dropped whole when it does not fit: its charge goes with it.
        if let Err(refused) = self.charge(&mut charge, part.len()) {
            self.remember(resume.cursor);
            return Err(refused);
        }
        payload.extend_from_slice(&part);
        scan.scan(&payload);
        if scan.complete() || scan.never_completes() {
            return Ok(Upload::Complete(payload, charge));
        }
        self.keep_upload(owner, held.perform, resume.cursor, (payload, scan, charge))
    }

    /// Drop the transfers of file `file_id`, whose session in it ended (the
    /// client closed the file, logged out of it, or logged in to it again):
    /// their charges are given back and their cursors remembered, so that
    /// continuing one is refused rather than answered as finished. The
    /// connection's transfers on its other files go on. Returns how many
    /// were dropped.
    pub fn drop_file(&mut self, file_id: u32) -> usize {
        let cursors: Vec<u64> = self
            .by_cursor
            .iter()
            .filter(|(_, held)| held.owner.file_id == file_id)
            .map(|(cursor, _)| *cursor)
            .collect();
        for cursor in &cursors {
            self.remove(*cursor);
            self.remember(*cursor);
        }
        cursors.len()
    }

    /// Hold an incomplete upload and answer the block that continues it.
    fn keep_upload(
        &mut self,
        owner: Owner,
        perform: u64,
        cursor: u64,
        (payload, scan, charge): (Vec<u8>, UploadScan, Charge),
    ) -> Result<Upload, Refused> {
        // FileMaker Server's answer when the copy key cannot be framed is
        // unmeasured; no key path of a real file is that long.
        let Some(copy_key) = scan.copy_key() else {
            warn!("dropping an upload that stopped at a key path too long to report");
            self.remember(cursor);
            return Err(Refused);
        };
        let upload = Transfer::Upload {
            payload,
            scan,
            charge,
        };
        self.hold(owner, perform, cursor, upload);
        Ok(Upload::Partial(Resume {
            perform,
            cursor,
            depth: -1,
            copy_key,
        }))
    }

    /// Keep `transfer` under `cursor` as the one in use, then evict the
    /// least recently used others until the limits hold.
    fn hold(&mut self, owner: Owner, perform: u64, cursor: u64, transfer: Transfer) {
        self.clock += 1;
        self.insert(
            cursor,
            Held {
                owner,
                perform,
                used: self.clock,
                transfer,
            },
        );
        let in_use = self
            .by_cursor
            .get(&cursor)
            .map_or(0, |h| h.transfer.bytes());
        while self.by_cursor.len() > MAX_TRANSFERS || self.bytes - in_use > MAX_IDLE {
            let Some(oldest) = self.oldest(|c, _| c != cursor) else {
                break;
            };
            self.evict(oldest);
        }
    }

    /// Grow `charge` by `more` bytes on the server's budget, evicting this
    /// connection's idle uploads, least recently used first, while it does
    /// not fit what the client may still hold; refused when none is left.
    /// (The upload being received is out of the table, so every one left is
    /// idle; idle downloads are not charged, so evicting them would not
    /// help.)
    fn charge(&mut self, charge: &mut Charge, more: usize) -> Result<(), Refused> {
        while !charge.grow(more) {
            let upload = |_, held: &Held| matches!(held.transfer, Transfer::Upload { .. });
            let Some(oldest) = self.oldest(upload) else {
                warn!("refusing {more} more bytes of an upload past what the client may hold");
                return Err(Refused);
            };
            self.evict(oldest);
        }
        Ok(())
    }

    /// The least recently used transfer `keep` accepts.
    fn oldest(&self, keep: impl Fn(u64, &Held) -> bool) -> Option<u64> {
        self.by_cursor
            .iter()
            .filter(|(cursor, held)| keep(**cursor, held))
            .min_by_key(|(_, held)| held.used)
            .map(|(cursor, _)| *cursor)
    }

    /// Drop the transfer under `cursor`, and remember the cursor.
    fn evict(&mut self, cursor: u64) {
        if let Some(evicted) = self.remove(cursor) {
            info!(
                "evicting an unfinished transfer of {} bytes to stay within the limits",
                evicted.transfer.bytes()
            );
        }
        self.remember(cursor);
    }

    /// Remember a cursor whose transfer was dropped for a limit, so that
    /// continuing it is refused rather than answered as finished.
    fn remember(&mut self, cursor: u64) {
        self.evicted.push_back(cursor);
        if self.evicted.len() > EVICTED_REMEMBERED {
            self.evicted.pop_front();
        }
    }

    /// Put `held` under `cursor`.
    fn insert(&mut self, cursor: u64, held: Held) {
        self.bytes += held.transfer.bytes();
        // Handles are random: a collision replaces the older transfer.
        if let Some(replaced) = self.by_cursor.insert(cursor, held) {
            self.bytes -= replaced.transfer.bytes();
        }
    }

    /// Take the transfer under `cursor` out of the table.
    fn remove(&mut self, cursor: u64) -> Option<Held> {
        let held = self.by_cursor.remove(&cursor)?;
        self.bytes -= held.transfer.bytes();
        Some(held)
    }

    /// A continuation of a cursor this connection does not hold: refused if
    /// it was dropped for a limit, otherwise answered as unknown.
    fn unknown(&self, cursor: u64) -> Result<(), Refused> {
        if self.evicted.contains(&cursor) {
            warn!("refusing to continue transfer {cursor:#x}, dropped to stay within the limits");
            return Err(Refused);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: Owner = Owner {
        opcode: 92,
        file_id: 1,
    };

    /// No transfers, on a budget that refuses nothing.
    fn transfers() -> Transfers {
        Transfers::new(&UploadMemory::new(usize::MAX), [192, 0, 2, 1].into())
    }

    fn counter() -> impl FnMut() -> u64 {
        let mut next = 0x7e0f_0000_0000;
        move || {
            next += 1;
            next
        }
    }

    #[test]
    fn empty_uploads_are_complete() {
        let mut transfers = transfers();
        // An op13 of a value: no slots, complete at once.
        assert!(matches!(
            transfers.upload(OWNER, &Resume::start(), Vec::new(), MAX_UPLOAD, counter()),
            Ok(Upload::Complete(p, _)) if p.is_empty()
        ));
        // A continuation that adds nothing ends the transfer instead of
        // asking for more forever.
        let Ok(Upload::Partial(resume)) = transfers.upload(
            OWNER,
            &Resume::start(),
            vec![0x80, 0x20, 0x01],
            MAX_UPLOAD,
            counter(),
        ) else {
            panic!("an unfinished list continues");
        };
        assert!(matches!(
            transfers.upload(OWNER, &resume, Vec::new(), MAX_UPLOAD, counter()),
            Ok(Upload::Complete(p, _)) if p == [0x80, 0x20, 0x01]
        ));
        assert!(matches!(
            transfers.upload(OWNER, &resume, Vec::new(), MAX_UPLOAD, counter()),
            Ok(Upload::Unknown)
        ));
    }

    #[test]
    fn pages_concatenate_to_the_stream() {
        // A node with 40 leaves of 6 bytes each, then the closing pop.
        let mut stream = vec![0x80];
        for key in 1..=40u8 {
            stream.extend([0x03, key, 1, 2, 3, 4]);
        }
        stream.push(0x40);
        let mut transfers = transfers();
        let mut handles = counter();
        let (mut got, mut resume) = transfers.start(OWNER, stream.clone(), 64, &mut handles);
        let mut pages = 1;
        while resume.has_more() {
            assert_eq!(resume.depth, 1, "every cut is inside the node's list");
            let (page, next) = transfers
                .next(OWNER, &resume, 64)
                .unwrap()
                .expect("known cursor");
            assert!(page.len() <= 64);
            got.extend(page);
            resume = next;
            pages += 1;
        }
        assert_eq!(got, stream);
        assert_eq!(pages, 4);
        assert_eq!(resume, Resume::complete());
        assert!(transfers.by_cursor.is_empty(), "the cursor is freed");
    }

    #[test]
    fn a_stream_that_fits_retains_nothing() {
        let mut transfers = transfers();
        let (page, resume) = transfers.start(OWNER, vec![0x80, 0x40], 64, || unreachable!());
        assert_eq!((page, resume), (vec![0x80, 0x40], Resume::complete()));
    }

    #[test]
    fn unknown_or_mismatched_handles_are_refused() {
        let mut transfers = transfers();
        let stream: Vec<u8> = [0x80]
            .into_iter()
            .chain([0x00, 0x01].repeat(30))
            .chain([0x40])
            .collect();
        let (_, resume) = transfers.start(OWNER, stream, 8, counter());
        let wrong = Resume {
            perform: resume.perform + 1,
            ..resume.clone()
        };
        assert!(transfers.next(OWNER, &wrong, 8).unwrap().is_none());
        assert!(
            transfers.next(OWNER, &resume, 8).unwrap().is_some(),
            "still held after a bad echo"
        );
    }

    #[test]
    fn dropping_a_files_transfers_refuses_their_continuation() {
        let mut transfers = transfers();
        let other = Owner {
            opcode: 92,
            file_id: 2,
        };
        let stream = vec![0x0f; 3 * PAGE_LIMIT];
        let mut handles = counter();
        let (_, kept) = transfers.start(other, stream.clone(), PAGE_LIMIT, &mut handles);
        let (_, dropped) = transfers.start(OWNER, stream, PAGE_LIMIT, &mut handles);
        assert_eq!(transfers.drop_file(OWNER.file_id), 1);
        assert!(transfers.next(OWNER, &dropped, PAGE_LIMIT).is_err());
        assert!(matches!(
            transfers.next(other, &kept, PAGE_LIMIT),
            Ok(Some(_))
        ));
    }

    #[test]
    fn uploads_in_parts() {
        let mut transfers = transfers();
        let mut handles = counter();
        let part1 = vec![0x80, 0x00, 0x10, 0x20, 0x11, 0x80, 0x00, 0x01];
        let Ok(Upload::Partial(resume)) =
            transfers.upload(OWNER, &Resume::start(), part1, MAX_UPLOAD, &mut handles)
        else {
            panic!("first part is partial");
        };
        assert_eq!(
            (resume.depth, resume.copy_key.clone()),
            (-1, vec![vec![0x11]])
        );
        let Ok(Upload::Complete(payload, _)) = transfers.upload(
            OWNER,
            &resume,
            vec![0x40, 0x00, 0xfb, 0x40],
            MAX_UPLOAD,
            &mut handles,
        ) else {
            panic!("second part completes");
        };
        assert_eq!(
            payload,
            [0x80, 0x00, 0x10, 0x20, 0x11, 0x80, 0x00, 0x01, 0x40, 0x00, 0xfb, 0x40]
        );
        assert!(matches!(
            transfers.upload(OWNER, &resume, vec![0x40], MAX_UPLOAD, &mut handles),
            Ok(Upload::Unknown)
        ));
    }

    /// Abandoned transfers hold at most `MAX_IDLE` bytes besides the one in
    /// use: the least recently used is evicted, and continuing it is refused
    /// rather than answered as the end of its stream.
    #[test]
    fn idle_transfers_are_evicted_beyond_their_bytes() {
        // A node of 64 KiB leaves, a third of `MAX_IDLE` in all.
        let leaf = [&[0x07, 0x01, 0xff, 0xff][..], &[0x5a; 0xffff]].concat();
        let mut stream = vec![0x80];
        while stream.len() < MAX_IDLE / 3 {
            stream.extend_from_slice(&leaf);
        }
        stream.push(0x40);
        let mut transfers = transfers();
        let mut handles = counter();
        let mut start = |transfers: &mut Transfers| {
            let (_, resume) = transfers.start(OWNER, stream.clone(), PAGE_LIMIT, &mut handles);
            assert!(resume.has_more());
            resume
        };
        let first = start(&mut transfers);
        let second = start(&mut transfers);
        let third = start(&mut transfers);
        assert_eq!(transfers.by_cursor.len(), 3, "two idle thirds fit");
        let fourth = start(&mut transfers);
        assert_eq!(transfers.by_cursor.len(), 3, "three idle thirds do not");
        assert!(transfers.next(OWNER, &first, PAGE_LIMIT).is_err());
        for resume in [second, third, fourth] {
            assert!(transfers
                .next(OWNER, &resume, PAGE_LIMIT)
                .unwrap()
                .is_some());
        }
        assert!(transfers.bytes <= MAX_IDLE + stream.len());
    }
}
