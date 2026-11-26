//! Hosted-file discovery (the Hosts dialog's List files): the files a host
//! publishes, read over a connection of the browser's own.
//!
//! OpenMaker Server publishes its list through OpenMaker's own extension
//! (`om_protocol::hosted_files`), asked for only once the host claims it.
//! A host without the extension, FileMaker Server above all, is asked with
//! FileMaker's own GetFileList (op77), anonymously, as FileMaker Pro's Hosts
//! window asks (network-protocol.md 5.3): its reply names each file by
//! child `10` of an entry of the list tree. A host that refuses the list or
//! answers with something else is reported as such, never as an empty list.

use std::sync::atomic::{AtomicBool, Ordering};

use om_data::{HostBrowser, HostListing, HostedFile, ListingError, ListingFailure};
use om_giop::ReadError;
use om_protocol::{hosted_files, MachineInfo};
use om_session::{Session, SessionError, TransportError};

use crate::NativeConfig;

/// Lists hosts' files for the Hosts dialog. Each listing connects on its
/// own, verifying the host's certificate as opening a file does, and
/// without the client license: a listing opens no file, so no host needs
/// one for it. The connection is closed before the listing returns,
/// whatever the outcome. Nothing of an open file's session is used. No
/// `Debug`: the configuration holds the client's private key.
pub(crate) struct NativeBrowser {
    config: NativeConfig,
    /// Where the additional certificate authority is trusted
    /// ([`crate::NativeProvider::with_ca_scope`]).
    ca_scope: Option<(String, u16)>,
    /// Certificates the user permitted
    /// ([`crate::NativeProvider::with_permitted_certificate`]).
    permits: Vec<crate::Permit>,
}

impl NativeBrowser {
    pub(crate) fn new(
        config: NativeConfig,
        ca_scope: Option<(String, u16)>,
        permits: Vec<crate::Permit>,
    ) -> Self {
        Self {
            config,
            ca_scope,
            permits,
        }
    }
}

impl HostBrowser for NativeBrowser {
    fn browse(
        &self,
        host: &str,
        port: u16,
        canceled: &AtomicBool,
    ) -> Result<HostListing, ListingError> {
        check(canceled)?;
        let host = host.trim();
        if host.is_empty() {
            return Err(ListingError::new(
                ListingFailure::Other,
                "Enter a host name or address.",
            ));
        }
        let machine = MachineInfo {
            install_code: String::new(),
            ..self.config.machine.clone()
        };
        let result = match Session::connect_trusting(
            host,
            port,
            &self.config.client_cert_pem,
            &self.config.client_key_pem,
            &self.config.callback_host,
            &machine,
            &crate::scoped_config(&self.config, self.ca_scope.as_ref(), host, port)
                .additional_ca_pem,
            &crate::permitted_for(&self.permits, host, port),
        ) {
            Ok(mut session) => {
                let listed = list(&mut session, host, port, canceled);
                // Always finish the owned connection, including when the
                // listing was canceled during an in-flight operation.
                let _ = session.disconnect();
                listed
            }
            Err(error) => Err(connect_failure(host, port, &error)),
        };
        // Cancellation wins over any result completed after it was set,
        // including connect failures, unsupported hosts and successful lists.
        check(canceled)?;
        result
    }
}

fn check(canceled: &AtomicBool) -> Result<(), ListingError> {
    if canceled.load(Ordering::SeqCst) {
        Err(ListingError::new(
            ListingFailure::Canceled,
            "Listing files was canceled.",
        ))
    } else {
        Ok(())
    }
}

fn list(
    session: &mut Session,
    host: &str,
    port: u16,
    canceled: &AtomicBool,
) -> Result<HostListing, ListingError> {
    check(canceled)?;
    let identity = session.identity();
    let host_name = identity.remote_name.clone();
    let host_version = identity.version_string.clone();
    let offered = session
        .is_a(hosted_files::REPO_ID)
        .map_err(|e| unsupported(host, port, &host_version, &format!(" ({e})")))?;
    if !offered {
        check(canceled)?;
        let files = file_list(session, host, port, &host_version)?;
        return Ok(HostListing {
            host_name,
            host_version,
            files,
        });
    }
    check(canceled)?;
    let mut names = session.list_hosted_files().map_err(|e| {
        ListingError::new(
            ListingFailure::Malformed,
            format!("The file list from {host}:{port} was malformed or incomplete ({e})."),
        )
    })?;
    // Alphabetical, as the Hosts dialog shows them (OpenMaker's order).
    names.sort_by_cached_key(|name| name.to_lowercase());
    Ok(HostListing {
        host_name,
        host_version,
        files: names.into_iter().map(|name| HostedFile { name }).collect(),
    })
}

/// The host's files by GetFileList (op77), sorted as the Hosts dialog
/// shows them. A reply this client cannot read is "unsupported", as before
/// op77 was asked (an OpenMaker Server without the extension answers op77
/// with a bare error 3); a reply that refuses the list says why.
fn file_list(
    session: &mut Session,
    host: &str,
    port: u16,
    host_version: &str,
) -> Result<Vec<HostedFile>, ListingError> {
    let reply = session
        .get_file_list()
        .map_err(|e| unsupported(host, port, host_version, &format!(" ({e})")))?;
    let refused = |message: String| Err(ListingError::new(ListingFailure::Other, message));
    match reply.error {
        0 => {}
        18 => {
            return refused(format!(
                "The host at {host}:{port} lists its files only to a signed-in account, \
                 which OpenMaker does not send yet. Enter the file name to open it."
            ))
        }
        812 | 1638 => {
            return refused(format!(
                "The host at {host}:{port} is not accepting new connections (FileMaker \
                 error {}).",
                reply.error
            ))
        }
        error => {
            return refused(format!(
                "The host at {host}:{port} did not list its files (FileMaker error {error})."
            ))
        }
    }
    let mut names = listed_names(&reply).map_err(|detail| {
        ListingError::new(
            ListingFailure::Malformed,
            format!("The file list from {host}:{port} was malformed or incomplete ({detail})."),
        )
    })?;
    // Alphabetical, as the Hosts dialog shows them (OpenMaker's order).
    names.sort_by_cached_key(|name| name.to_lowercase());
    Ok(names.into_iter().map(|name| HostedFile { name }).collect())
}

/// The names an op77 list names, in its order: child `10` of each entry
/// of the list tree; else why it cannot be read. The slots are read as a
/// stream, keeping only the names, so a hostile list costs no more than the
/// files it counts. Every entry must be a list holding one name that
/// decodes and is not empty; every list must close; and the entries must
/// be as many as the reply counts.
fn listed_names(reply: &om_protocol::FileListReply) -> Result<Vec<String>, String> {
    use om_hbam::ChunkKind;
    // The session reads every page of a list; one that still continues is
    // not whole: say so rather than show part of it.
    if reply.resume.has_more() {
        return Err("it continues on a page that was not read".into());
    }
    let count = usize::from(reply.count);
    let mut names = Vec::with_capacity(count.min(1024));
    // The entry being read, and its name once found.
    let mut entry: Option<Option<String>> = None;
    let finish = |names: &mut Vec<String>, entry: Option<Option<String>>| match entry {
        None => Ok(()),
        Some(None) => Err("an entry names no file".to_owned()),
        Some(Some(name)) => {
            names.push(name);
            Ok(())
        }
    };
    let mut reader = om_hbam::SlotReader::new(&reply.slots);
    for slot in reader.by_ref() {
        let slot = slot.map_err(|e| e.to_string())?;
        match (slot.path.len(), slot.kind) {
            (1, ChunkKind::Push) => {
                finish(&mut names, entry.take())?;
                if names.len() == count {
                    return Err(format!("it lists more files than the {count} it counts"));
                }
                entry = Some(None);
            }
            (1, _) => return Err("an entry is not a list".into()),
            (2, ChunkKind::Leaf) if slot.path[1].0 == [FILE_NAME as u8] => {
                let name = entry.as_mut().ok_or("a name outside an entry")?;
                if name.is_some() {
                    return Err("an entry names two files".into());
                }
                let value = slot.value.unwrap_or_default();
                if value.len() > MAX_NAME_BYTES {
                    return Err("a file name is too long".into());
                }
                let text = om_scsu::decode_scsu(value)
                    .map_err(|e| format!("a name does not decode ({e:?})"))?;
                if text.is_empty() {
                    return Err("an entry's name is empty".into());
                }
                *name = Some(text);
            }
            // An entry's id, folder and preferences are not read.
            _ => {}
        }
    }
    finish(&mut names, entry.take())?;
    if !reader.path().is_empty() {
        return Err("a list in it is not closed".into());
    }
    if names.len() != count {
        return Err(format!("it names {} files and counts {count}", names.len()));
    }
    Ok(names)
}

/// The longest file name, in stored bytes, an op77 list may hold:
/// OpenMaker's bound, far above any file name FileMaker accepts.
const MAX_NAME_BYTES: usize = 1024;

/// An op77 list entry's child holding the file's name, with its `.fmp12`
/// extension (network-protocol.md 5.3).
const FILE_NAME: u64 = 0x10;

fn unsupported(host: &str, port: u16, version: &str, detail: &str) -> ListingError {
    let version = match version.trim() {
        "" => String::new(),
        version => format!(" ({version})"),
    };
    ListingError::new(
        ListingFailure::Unsupported,
        format!(
            "The host at {host}:{port}{version} does not offer a file list OpenMaker can \
             read{detail}. FileMaker Server's own file list is not supported yet; enter the \
             file name to open it."
        ),
    )
}

/// What went wrong connecting: the host could not be reached (or trusted),
/// or what answered is not a FileMaker-protocol host.
fn connect_failure(host: &str, port: u16, error: &SessionError) -> ListingError {
    match error {
        SessionError::Transport(TransportError::Unverified(refused)) => {
            ListingError::untrusted(crate::untrusted_host(host, port, refused))
        }
        SessionError::Transport(TransportError::Tls(e)) => ListingError::new(
            ListingFailure::Unreachable,
            format!("Could not make a trusted connection to {host}:{port} ({e})."),
        ),
        SessionError::Transport(TransportError::Pem(e)) => ListingError::new(
            ListingFailure::Other,
            format!("Could not connect to {host}:{port} ({e})."),
        ),
        SessionError::Transport(TransportError::Read(
            ReadError::Giop(_) | ReadError::TooLarge(_) | ReadError::ExpectedFragment,
        ))
        | SessionError::Protocol(_) => ListingError::new(
            ListingFailure::Unsupported,
            format!("The host at {host}:{port} did not answer as a FileMaker host ({error})."),
        ),
        _ => ListingError::new(
            ListingFailure::Unreachable,
            format!("Could not reach {host}:{port} ({error})."),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::slot::{encode_chunk, minimal_code, POP};

    fn push(key: &[u8]) -> Vec<u8> {
        encode_chunk(minimal_code(true, key.len(), None).unwrap(), key, &[]).unwrap()
    }

    fn leaf(key: &[u8], value: &[u8]) -> Vec<u8> {
        encode_chunk(
            minimal_code(false, key.len(), Some(value.len())).unwrap(),
            key,
            value,
        )
        .unwrap()
    }

    /// An op77 list tree as network-protocol.md 5.3 lays it out: entries
    /// `01`, `02`, ..., each with the file's id `01`, folder `07` and name
    /// `10`.
    fn list(files: &[(u32, &str)]) -> Vec<u8> {
        let mut slots = Vec::new();
        for (index, (id, name)) in files.iter().enumerate() {
            slots.extend(push(&[index as u8 + 1]));
            slots.extend(leaf(&[0x01], &id.to_be_bytes()));
            slots.extend(leaf(&[0x07], b"Databases"));
            // Stored text: SCSU (here ASCII) XOR 0x5A, as `Node::text` reads it.
            let stored: Vec<u8> = name.bytes().map(|byte| byte ^ 0x5a).collect();
            slots.extend(leaf(&[0x10], &stored));
            slots.push(POP);
        }
        slots
    }

    #[test]
    fn an_op77_list_names_its_files_in_its_order() {
        let reply = om_protocol::FileListReply::listed(
            list(&[(7, "Tasks.fmp12"), (3, "Contacts.fmp12")]),
            2,
            "server",
        );
        assert_eq!(
            listed_names(&reply),
            Ok(vec!["Tasks.fmp12".to_owned(), "Contacts.fmp12".to_owned()])
        );
    }

    #[test]
    fn an_op77_list_that_does_not_add_up_or_continues_is_refused() {
        let short = om_protocol::FileListReply::listed(list(&[(7, "Tasks.fmp12")]), 2, "server");
        assert!(listed_names(&short).unwrap_err().contains("counts 2"));
        let mut paged =
            om_protocol::FileListReply::listed(list(&[(7, "Tasks.fmp12")]), 1, "server");
        paged.resume.cursor = 9;
        assert!(listed_names(&paged).unwrap_err().contains("page"));
        let garbled = om_protocol::FileListReply::listed(vec![0xff; 3], 1, "server");
        assert!(listed_names(&garbled).is_err());
        // An entry left open, though its name is there (#782's review).
        let mut open = list(&[(7, "Tasks.fmp12")]);
        open.pop();
        let open = om_protocol::FileListReply::listed(open, 1, "server");
        assert!(listed_names(&open).unwrap_err().contains("not closed"));
        // A second entry whose name does not decode is no shorter list:
        // stored 0x54 unmasks (XOR 0x5A) to SCSU's quote tag 0x0E, whose
        // two bytes are missing.
        let mut bad = list(&[(7, "Tasks.fmp12")]);
        bad.extend(push(&[2]));
        bad.extend(leaf(&[0x10], &[0x54]));
        bad.push(POP);
        let bad = om_protocol::FileListReply::listed(bad, 1, "server");
        assert!(listed_names(&bad).is_err());
        let bad_counted = om_protocol::FileListReply::listed(
            [
                list(&[(7, "Tasks.fmp12")]),
                push(&[2]),
                leaf(&[0x10], &[0x54]),
                vec![POP],
            ]
            .concat(),
            2,
            "server",
        );
        assert!(listed_names(&bad_counted).is_err());
        // An entry without a name, and a name that is not in a list.
        let nameless = [push(&[1]), leaf(&[0x01], &[0, 0, 0, 7]), vec![POP]].concat();
        let nameless = om_protocol::FileListReply::listed(nameless, 1, "server");
        assert!(listed_names(&nameless)
            .unwrap_err()
            .contains("names no file"));
        let flat = om_protocol::FileListReply::listed(leaf(&[0x10], b"x"), 1, "server");
        assert!(listed_names(&flat).is_err());
        // Many entries for a small count are refused at the first one too
        // many, not read to the end.
        let many: Vec<u8> = (0..10_000u32)
            .flat_map(|n| [push(&n.to_be_bytes()), vec![POP]].concat())
            .collect();
        let many = om_protocol::FileListReply::listed(many, 1, "server");
        assert!(listed_names(&many).is_err());
    }
}
