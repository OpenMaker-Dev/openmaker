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
//!   transfer or its session ends.
//! - The download streams every connection keeps paged are charged to a
//!   server-wide budget of their own ([`DownloadMemory`]). Past it, the
//!   streams least recently paged, on any connection, are dropped, never
//!   the one just started (#145). A client continues its download at once,
//!   so the least recently paged streams are those clients abandoned. They
//!   are dropped, not the new download refused, since a refused read shows
//!   where a dropped abandoned one does not; a client address is not the
//!   unit, since many connections behind one NAT address each page a
//!   download in ordinary use.
//!
//! Continuing an evicted transfer, or one dropped with its file's session
//! ([`Transfers::drop_file`]: the file was closed, logged out of, or logged
//! in to again), and uploading past a limit are [`Refused`]: the connection
//! answers with a CORBA system exception, which the client reports as an
//! error. An upload refused part way is dropped whole, its charge given
//! back, and its cursor remembered like an evicted one; an evicted cursor
//! never reads as the end of a stream, so a client is never handed a
//! silently truncated result. A continuation of a download (op90, op92,
//! op86's results) whose cursor this connection never issued, or finished,
//! gets the empty last page FileMaker Server's code gives, which a client
//! reads as the end of the stream. An upload's continuation with such a
//! cursor ([`Upload::Unknown`]) is a failure for its operation to answer,
//! as nothing of it is held: the empty last page would read as the upload
//! done (op13, op14, op70 and op88 fail it, and op86 for a program's
//! part). A repeat of the last part of an upload this connection completed
//! in parts is told apart ([`Upload::Repeated`]): the operation ran on it.
//!
//! Receiving an upload costs time linear in its size: the scan of the parts
//! received ([`UploadScan`]) is kept with the transfer.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

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
/// refused instead of answered as the end of its stream (32 KiB a
/// connection). Downloads are dropped for the server's budget by other
/// connections' downloads too, so this is far more than one connection's
/// own transfers would evict.
const EVICTED_REMEMBERED: usize = 4096;

/// How many uploads completed in parts are remembered, so that a repeat of
/// one's last part (a client retrying after a timeout) is answered as the
/// upload that completed, not as a cursor never issued (6 KiB a
/// connection): a retry follows its upload at once.
const COMPLETED_REMEMBERED: usize = 256;

/// The server's budget for the download streams connections keep paged:
/// the bytes not yet sent and those sent, of each stream larger than one
/// page, from its first page until its last is sent or it is dropped.
#[derive(Debug)]
pub(crate) struct DownloadMemory {
    /// The most the streams may hold together, in bytes.
    limit: AtomicUsize,
    /// What they hold now.
    held: AtomicUsize,
    /// Advances each time a stream is paged, on any connection.
    clock: AtomicU64,
    /// Every stream held, some perhaps already dropped.
    streams: Mutex<Vec<Weak<Stream>>>,
}

impl DownloadMemory {
    /// A budget of `limit` bytes.
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: AtomicUsize::new(limit),
            held: AtomicUsize::new(0),
            clock: AtomicU64::new(0),
            streams: Mutex::default(),
        })
    }

    /// Change the limit. What is held stays until a stream is next held.
    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit, Ordering::Relaxed);
    }

    /// What all streams hold now.
    #[cfg(test)]
    pub fn in_use(&self) -> usize {
        self.held.load(Ordering::Relaxed)
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn streams(&self) -> MutexGuard<'_, Vec<Weak<Stream>>> {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold `data` as a stream being paged, first dropping the streams
    /// least recently paged, on any connection, while all of them together
    /// would pass the limit. The new stream is held however large it is.
    /// Charging, dropping and listing happen under the list's lock, so two
    /// streams held at once cannot both miss what the other added.
    fn hold(self: &Arc<Self>, mut data: Vec<u8>) -> Arc<Stream> {
        // What is kept is what is charged: the stream was grown as it was
        // written, and could hold up to twice its length.
        data.shrink_to_fit();
        let bytes = data.capacity();
        let stream = Arc::new(Stream {
            bytes,
            used: AtomicU64::new(self.tick()),
            data: Mutex::new(Some(data)),
            memory: self.clone(),
        });
        let limit = self.limit.load(Ordering::Relaxed);
        // Dropped after the list's lock is let go: the last reference to a
        // stream may be one of these.
        let mut dropped = Vec::new();
        let mut streams = self.streams();
        self.held.fetch_add(bytes, Ordering::Relaxed);
        streams.retain(|stream| stream.strong_count() > 0);
        // Two passes, least recently paged first. The first spares a
        // stream paged since the order was taken, as its owner may have
        // just answered with a cursor into it; the second, which runs only
        // if the first left the budget passed, drops in the order as it
        // then is, so the budget holds however the others are paged.
        for spare_paged in [true, false] {
            if self.held.load(Ordering::Relaxed) <= limit {
                break;
            }
            let mut others: Vec<(u64, Arc<Stream>)> = (streams.iter())
                .filter_map(Weak::upgrade)
                .map(|other| (other.used.load(Ordering::Relaxed), other))
                .collect();
            others.sort_by_key(|(used, _)| *used);
            for (used, other) in others {
                if self.held.load(Ordering::Relaxed) <= limit {
                    break;
                }
                let gone = if spare_paged {
                    other.drop_unless_paged_since(used)
                } else {
                    other.drop_data()
                };
                if gone {
                    info!(
                        "dropping an unfinished download of {} bytes to stay within the \
                         server's download memory",
                        other.bytes
                    );
                }
                dropped.push(other);
            }
            streams.retain(|stream| stream.upgrade().is_some_and(|s| s.holds()));
        }
        streams.push(Arc::downgrade(&stream));
        drop(streams);
        drop(dropped);
        stream
    }
}

/// A download stream being paged. Its bytes are charged to the server's
/// [`DownloadMemory`] while they are held: until its last page is sent,
/// its transfer is evicted or its connection ends, or the budget drops it.
#[derive(Debug)]
struct Stream {
    bytes: usize,
    /// When it was last paged, by the budget's clock.
    used: AtomicU64,
    /// `None` once the budget dropped it.
    data: Mutex<Option<Vec<u8>>>,
    memory: Arc<DownloadMemory>,
}

impl Stream {
    fn data(&self) -> MutexGuard<'_, Option<Vec<u8>>> {
        self.data.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn holds(&self) -> bool {
        self.data().is_some()
    }

    /// Give its bytes back to the budget, once: whether this did.
    fn drop_data(&self) -> bool {
        Self::give_back(&self.memory, self.bytes, &mut self.data())
    }

    /// [`Self::drop_data`], unless it was paged after `used`: its owner
    /// marks it paged with its data locked, so it is never dropped between
    /// a page cut from it and the mark.
    fn drop_unless_paged_since(&self, used: u64) -> bool {
        let mut data = self.data();
        self.used.load(Ordering::Relaxed) == used
            && Self::give_back(&self.memory, self.bytes, &mut data)
    }

    fn give_back(memory: &DownloadMemory, bytes: usize, data: &mut Option<Vec<u8>>) -> bool {
        let held = data.take().is_some();
        if held {
            memory.held.fetch_sub(bytes, Ordering::Relaxed);
        }
        held
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.drop_data();
    }
}

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
        stream: Arc<Stream>,
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
            Transfer::Download { stream, .. } => stream.bytes,
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
    /// A repeat of the last part of an upload this operation completed on
    /// this connection ([`COMPLETED_REMEMBERED`]): its operation ran on
    /// it; nothing is held again.
    Repeated,
    /// The cursor is not an upload this operation started on this
    /// connection, or not one it remembers completing.
    Unknown,
}

/// The transfers open on one connection.
pub(crate) struct Transfers {
    /// The server's budget its uploads are charged to, and the client
    /// address they are charged under.
    memory: Arc<UploadMemory>,
    client: IpAddr,
    /// The server's budget its download streams are charged to.
    downloads: Arc<DownloadMemory>,
    by_cursor: HashMap<u64, Held>,
    /// The bytes the transfers hold.
    bytes: usize,
    /// Advances each time a transfer is used.
    clock: u64,
    /// The cursors evicted most recently, oldest first.
    evicted: VecDeque<u64>,
    /// The uploads completed most recently in more than one part, oldest
    /// first: cursor, owner and perform handle.
    completed: VecDeque<(u64, Owner, u64)>,
}

/// The most slot bytes a reply may carry for this `maxreturn`.
pub(crate) fn page_limit(max_return: u32) -> usize {
    (max_return as usize).min(PAGE_LIMIT)
}

impl Transfers {
    /// No transfers, their uploads charged to `memory` under `client` and
    /// their download streams to `downloads`.
    pub fn new(
        memory: &Arc<UploadMemory>,
        downloads: &Arc<DownloadMemory>,
        client: IpAddr,
    ) -> Self {
        Self {
            memory: memory.clone(),
            client,
            downloads: downloads.clone(),
            by_cursor: HashMap::new(),
            bytes: 0,
            clock: 0,
            evicted: VecDeque::new(),
            completed: VecDeque::new(),
        }
    }

    /// Serve the first page of `stream` for `owner`. `new_handle` mints a
    /// nonzero handle; it is called only when the stream does not fit one
    /// page, which is then held on the server's download budget.
    pub fn start(
        &mut self,
        owner: Owner,
        stream: Vec<u8>,
        limit: usize,
        mut new_handle: impl FnMut() -> u64,
    ) -> (Vec<u8>, Resume) {
        let (end, depth) = page_cut(&stream, 0, limit, 0);
        if end == stream.len() {
            return (stream, Resume::complete());
        }
        let page = stream[..end].to_vec();
        // This connection's own evictions come first: what they give back
        // to the server's budget is then not taken from other connections'
        // streams by it.
        self.make_room();
        let stream = self.downloads.hold(stream);
        let (perform, cursor) = (new_handle(), new_handle());
        (
            page,
            self.keep_download(owner, perform, cursor, stream, end, depth),
        )
    }

    /// Serve the next page of the download `resume` names, or `None` if
    /// `owner` has no such download on this connection. Refused when it was
    /// evicted, or dropped for the server's download budget.
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
                let cut = stream.data().as_deref().map(|data| {
                    stream.used.store(self.downloads.tick(), Ordering::Relaxed);
                    let (end, depth) = page_cut(data, offset, limit, depth);
                    (data[offset..end].to_vec(), end, depth, end == data.len())
                });
                let Some((page, end, depth, complete)) = cut else {
                    warn!(
                        "refusing to continue transfer {:#x}, dropped to stay within the \
                         server's download memory",
                        resume.cursor
                    );
                    self.remember(resume.cursor);
                    return Err(Refused);
                };
                if complete {
                    return Ok(Some((page, Resume::complete())));
                }
                let resume =
                    self.keep_download(owner, held.perform, resume.cursor, stream, end, depth);
                Ok(Some((page, resume)))
            }
            transfer => {
                self.insert(resume.cursor, Held { transfer, ..held });
                Ok(None)
            }
        }
    }

    /// Keep the rest of `stream`, from `offset`, where the open-list depth
    /// is `depth`, under `perform` and `cursor`, and answer the block that
    /// continues it.
    fn keep_download(
        &mut self,
        owner: Owner,
        perform: u64,
        cursor: u64,
        stream: Arc<Stream>,
        offset: usize,
        depth: i16,
    ) -> Resume {
        let download = Transfer::Download {
            stream,
            offset,
            depth,
        };
        self.hold(owner, perform, cursor, download);
        Resume {
            perform,
            cursor,
            depth,
            copy_key: Vec::new(),
        }
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
            if self
                .completed
                .contains(&(resume.cursor, owner, resume.perform))
            {
                return Ok(Upload::Repeated);
            }
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
            self.remember_completed(resume.cursor, owner, resume.perform);
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
            self.remember_completed(resume.cursor, owner, resume.perform);
            return Ok(Upload::Complete(payload, charge));
        }
        self.keep_upload(owner, held.perform, resume.cursor, (payload, scan, charge))
    }

    /// Drop the upload that `resume` continues, when `owner` holds it, with
    /// its charge, and remember its cursor, so that a later part of it is
    /// refused: a continuation refused before it reaches
    /// [`Transfers::upload`] (its file became read-only, its session may no
    /// longer write) ends the upload it belongs to, rather than leave the
    /// parts already held charged until they are evicted.
    pub fn abandon_upload(&mut self, owner: Owner, resume: &Resume) {
        if !resume.has_more() {
            return;
        }
        let Some(held) = self.remove(resume.cursor) else {
            return;
        };
        match held.transfer {
            Transfer::Upload { .. } if held.owner == owner && held.perform == resume.perform => {
                self.remember(resume.cursor);
            }
            transfer => self.insert(resume.cursor, Held { transfer, ..held }),
        }
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
    /// least recently used others until the limits hold. Downloads the
    /// server's budget dropped are evicted first: they hold nothing, and
    /// counting them would evict transfers that do.
    fn hold(&mut self, owner: Owner, perform: u64, cursor: u64, transfer: Transfer) {
        let dropped: Vec<u64> = (self.by_cursor.iter())
            .filter(|(_, held)| match &held.transfer {
                Transfer::Download { stream, .. } => !stream.holds(),
                Transfer::Upload { .. } => false,
            })
            .map(|(cursor, _)| *cursor)
            .collect();
        for cursor in dropped {
            self.evict(cursor);
        }
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

    /// Evict the least recently used transfers until one more, in use,
    /// would keep the limits: every transfer held now is then idle. The
    /// same evictions [`Self::hold`] would make once it is added.
    fn make_room(&mut self) {
        while self.by_cursor.len() >= MAX_TRANSFERS || self.bytes > MAX_IDLE {
            let Some(oldest) = self.oldest(|_, _| true) else {
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

    /// The least recently used transfer `keep` accepts, a download the
    /// server's budget dropped before any that holds data: one dropped by
    /// another connection since this one last looked holds nothing.
    fn oldest(&self, keep: impl Fn(u64, &Held) -> bool) -> Option<u64> {
        let holds = |held: &Held| match &held.transfer {
            Transfer::Download { stream, .. } => stream.holds(),
            Transfer::Upload { .. } => true,
        };
        self.by_cursor
            .iter()
            .filter(|(cursor, held)| keep(**cursor, held))
            .min_by_key(|(_, held)| (holds(held), held.used))
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

    /// Remember that the upload under `cursor` completed
    /// ([`Upload::Repeated`]).
    fn remember_completed(&mut self, cursor: u64, owner: Owner, perform: u64) {
        self.completed.push_back((cursor, owner, perform));
        if self.completed.len() > COMPLETED_REMEMBERED {
            self.completed.pop_front();
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

    /// No transfers, on budgets that refuse nothing.
    fn transfers() -> Transfers {
        Transfers::new(
            &UploadMemory::new(usize::MAX),
            &DownloadMemory::new(usize::MAX),
            [192, 0, 2, 1].into(),
        )
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
        // Its last part again: the upload it completed.
        assert!(matches!(
            transfers.upload(OWNER, &resume, Vec::new(), MAX_UPLOAD, counter()),
            Ok(Upload::Repeated)
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
        // The last part again is told apart from a cursor never issued, and
        // from one another operation or perform handle names.
        assert!(matches!(
            transfers.upload(OWNER, &resume, vec![0x40], MAX_UPLOAD, &mut handles),
            Ok(Upload::Repeated)
        ));
        let other = Owner {
            opcode: OWNER.opcode + 1,
            ..OWNER
        };
        for (owner, resume) in [
            (other, resume.clone()),
            (
                OWNER,
                Resume {
                    perform: resume.perform + 1,
                    ..resume.clone()
                },
            ),
            (
                OWNER,
                Resume {
                    cursor: resume.cursor + 1000,
                    ..resume.clone()
                },
            ),
        ] {
            assert!(matches!(
                transfers.upload(owner, &resume, vec![0x40], MAX_UPLOAD, &mut handles),
                Ok(Upload::Unknown)
            ));
        }
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

    /// The download streams of all connections share the server's budget
    /// (#145): past it the stream least recently paged, on any connection,
    /// is dropped, and continuing it is refused; the one just started is
    /// held even alone past the budget; what is held goes back as streams
    /// finish or their connections end; a connection's own evictions come
    /// before the budget's.
    #[test]
    fn download_streams_share_the_servers_budget() {
        // A node of 100 leaves of 4 bytes: 402 bytes, pages of 64.
        let stream: Vec<u8> = [0x80]
            .into_iter()
            .chain((0..100u8).flat_map(|key| [0x02, key, 7, 7]))
            .chain([0x40])
            .collect();
        let uploads = UploadMemory::new(usize::MAX);
        let downloads = DownloadMemory::new(1000);
        let connection = || Transfers::new(&uploads, &downloads, [192, 0, 2, 1].into());
        let (mut a, mut b, mut c) = (connection(), connection(), connection());
        let mut handles = counter();
        let (_, resume_a) = a.start(OWNER, stream.clone(), 64, &mut handles);
        let (_, resume_b) = b.start(OWNER, stream.clone(), 64, &mut handles);
        assert_eq!(downloads.in_use(), 2 * stream.len());
        // A pages on: B is now the least recently paged, and C's stream
        // drops it, not A's.
        let (_, resume_a) = a.next(OWNER, &resume_a, 64).unwrap().unwrap();
        let (_, resume_c) = c.start(OWNER, stream.clone(), 64, &mut handles);
        assert_eq!(downloads.in_use(), 2 * stream.len());
        assert!(b.next(OWNER, &resume_b, 64).is_err());
        assert!(b.next(OWNER, &resume_b, 64).is_err());
        // A and C read to the end, as the stream was.
        for (transfers, mut resume) in [(&mut a, resume_a), (&mut c, resume_c)] {
            while resume.has_more() {
                resume = transfers.next(OWNER, &resume, 64).unwrap().unwrap().1;
            }
        }
        assert_eq!(downloads.in_use(), 0);
        // A stream larger than the budget is still held, alone.
        let large: Vec<u8> = [0x80]
            .into_iter()
            .chain((0..=255u8).flat_map(|key| [0x02, key, 7, 7]))
            .chain([0x40])
            .collect();
        let (_, resume_a) = a.start(OWNER, stream.clone(), 64, &mut handles);
        let (_, resume_b) = b.start(OWNER, large.clone(), 64, &mut handles);
        assert_eq!(downloads.in_use(), large.len());
        assert!(a.next(OWNER, &resume_a, 64).is_err());
        assert!(b.next(OWNER, &resume_b, 64).unwrap().is_some());
        // A connection's end gives its streams back.
        drop(b);
        assert_eq!(downloads.in_use(), 0);
        // A dropped download no longer counts against its connection's own
        // limits once it holds another transfer, and continuing it is still
        // refused.
        let (_, dropped) = a.start(OWNER, stream.clone(), 64, &mut handles);
        let _ = c.start(OWNER, large.clone(), 64, &mut handles);
        assert_eq!(a.bytes, stream.len());
        let part = vec![0x80, 0x20, 0x01];
        assert!(matches!(
            a.upload(
                OWNER,
                &Resume::start(),
                part.clone(),
                MAX_UPLOAD,
                &mut handles
            ),
            Ok(Upload::Partial(_))
        ));
        assert_eq!(a.bytes, part.len());
        assert!(a.next(OWNER, &dropped, 64).is_err());
        // A connection past its own limits evicts its own oldest transfer
        // first, so the budget, just big enough for that, keeps another
        // connection's older stream.
        let budget = DownloadMemory::new((MAX_TRANSFERS + 1) * stream.len());
        let connection = || Transfers::new(&uploads, &budget, [192, 0, 2, 1].into());
        let (mut full, mut other) = (connection(), connection());
        let (_, older) = other.start(OWNER, stream.clone(), 64, &mut handles);
        for _ in 0..=MAX_TRANSFERS {
            full.start(OWNER, stream.clone(), 64, &mut handles);
        }
        assert!(other.next(OWNER, &older, 64).unwrap().is_some());
    }
}
