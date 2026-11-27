//! Clipboard transfers between the pages and the app, on the display's
//! loop: a page's copy read from a pipe, the app's text written to a page
//! that pastes. The page host is not trusted to read or write promptly, or
//! at all, so each transfer is non-blocking, bounded in bytes, and dropped
//! with its descriptor at a deadline; no thread waits on a page.

use std::cell::Cell;
use std::fs::File;
use std::io::{ErrorKind, Read as _, Write as _};
use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};

/// How long a page has to hand over its copy, or take a paste.
const DEADLINE: Duration = Duration::from_secs(5);

/// A transfer under way: cancelled when a newer one supersedes it.
pub(super) struct Transfer {
    token: RegistrationToken,
    over: Rc<Cell<bool>>,
}

impl Transfer {
    /// Whether it has ended (finished, failed or timed out).
    pub fn over(&self) -> bool {
        self.over.get()
    }

    /// Stop it now, closing its descriptor.
    pub fn cancel<D>(self, handle: &LoopHandle<'static, D>) {
        if !self.over.replace(true) {
            handle.remove(self.token);
        }
    }
}

/// Put `fd` in non-blocking mode.
fn nonblocking(fd: impl AsFd) -> bool {
    smithay::reexports::rustix::io::ioctl_fionbio(fd, true).is_ok()
}

/// Drop the transfer `token` at the deadline, unless it ended first.
fn deadline<D: 'static>(
    handle: &LoopHandle<'static, D>,
    token: RegistrationToken,
    over: &Rc<Cell<bool>>,
) {
    // A weak handle: the loop's sources must not keep the loop, and with
    // it their descriptors, alive once it is dropped.
    let (remove, ended) = (handle.downgrade(), Rc::clone(over));
    let timer = handle.insert_source(Timer::from_duration(DEADLINE), move |_, _, _| {
        if !ended.replace(true) {
            if let Some(handle) = remove.upgrade() {
                handle.remove(token);
            }
        }
        TimeoutAction::Drop
    });
    if timer.is_err() {
        // Without a deadline the transfer does not run.
        if !over.replace(true) {
            handle.remove(token);
        }
    }
}

/// Read `reader` to its end, then give `done` what it held: nothing when
/// it held more than `max` bytes, failed, or missed the deadline.
pub(super) fn read<D: 'static>(
    handle: &LoopHandle<'static, D>,
    reader: std::io::PipeReader,
    max: usize,
    done: impl FnOnce(&mut D, Vec<u8>) + 'static,
) -> Option<Transfer> {
    if !nonblocking(&reader) {
        return None;
    }
    let over = Rc::new(Cell::new(false));
    let ended = Rc::clone(&over);
    let mut text = Vec::new();
    let mut done = Some(done);
    let token = handle
        .insert_source(
            Generic::new(reader, Interest::READ, Mode::Level),
            move |_, reader, state| {
                let mut chunk = [0u8; 64 * 1024];
                loop {
                    match reader.as_ref().read(&mut chunk) {
                        Ok(0) => {
                            ended.set(true);
                            if let Some(done) = done.take() {
                                done(state, std::mem::take(&mut text));
                            }
                            return Ok(PostAction::Remove);
                        }
                        Ok(read) if text.len() + read <= max => {
                            text.extend_from_slice(&chunk[..read]);
                        }
                        Ok(_) => {
                            ended.set(true);
                            return Ok(PostAction::Remove);
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            return Ok(PostAction::Continue);
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => {}
                        Err(_) => {
                            ended.set(true);
                            return Ok(PostAction::Remove);
                        }
                    }
                }
            },
        )
        .ok()?;
    deadline(handle, token, &over);
    Some(Transfer { token, over })
}

/// Write `text` to `fd`, then close it.
pub(super) fn write<D: 'static>(
    handle: &LoopHandle<'static, D>,
    fd: OwnedFd,
    text: Arc<str>,
) -> Option<Transfer> {
    if !nonblocking(&fd) {
        return None;
    }
    let file = File::from(fd);
    let over = Rc::new(Cell::new(false));
    let ended = Rc::clone(&over);
    let mut at = 0;
    let token = handle
        .insert_source(
            Generic::new(file, Interest::WRITE, Mode::Level),
            move |_, file, _| {
                while at < text.len() {
                    match file.as_ref().write(&text.as_bytes()[at..]) {
                        Ok(0) => break,
                        Ok(written) => at += written,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            return Ok(PostAction::Continue);
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                ended.set(true);
                Ok(PostAction::Remove)
            },
        )
        .ok()?;
    deadline(handle, token, &over);
    Some(Transfer { token, over })
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Write as _};

    use smithay::reexports::calloop::EventLoop;

    /// A page that never finishes its copy holds its pipe only while the
    /// display's loop lives: dropping the loop closes the descriptor.
    #[test]
    fn a_stalled_copy_closes_with_its_loop() {
        let event_loop: EventLoop<'static, ()> = EventLoop::try_new().expect("a loop");
        let (reader, mut writer) = std::io::pipe().expect("a pipe");
        let transfer = super::read(&event_loop.handle(), reader, 16, |_: &mut (), _| {});
        assert!(transfer.is_some());
        writer.write_all(b"partial").expect("the reader is open");
        drop(transfer);
        drop(event_loop);
        let closed = writer.write_all(b"more").expect_err("the reader closed");
        assert_eq!(closed.kind(), ErrorKind::BrokenPipe);
    }
}
