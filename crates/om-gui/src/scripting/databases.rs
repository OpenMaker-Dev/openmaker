//! Desktop file ownership. The active provider and FileState always move
//! together; window ids are unique across files and never reused in an epoch.
use super::session::{FileState, Shared};
use om_data::{
    file_refs::{hosted_key, FileIdentity},
    DataProvider,
};
use std::collections::BTreeMap;

/// Which file `state` is, as the desktop tells files apart
/// ([`FileIdentity`]): one on this computer by its path, a hosted one by
/// the host and port it was opened at and its name there. `None` when
/// neither is known (no host session).
pub(crate) fn file_identity(state: &FileState) -> Option<FileIdentity> {
    if let Some(path) = &state.context.local {
        return Some(FileIdentity::local(path));
    }
    let endpoint = state.endpoint.as_ref()?;
    Some(FileIdentity::hosted(
        &endpoint.host,
        endpoint.port,
        &state.context.file,
    ))
}

pub(crate) struct OpenFile {
    pub provider: Box<dyn DataProvider>,
    pub state: FileState,
}

pub(crate) struct OpenFiles {
    pub peers: BTreeMap<u64, OpenFile>,
    next_file: u64,
    next_window: u32,
}

impl Default for OpenFiles {
    fn default() -> Self {
        Self {
            peers: BTreeMap::new(),
            next_file: 1,
            next_window: 2,
        }
    }
}

impl OpenFiles {
    pub fn allocate_file(&mut self) -> u64 {
        let id = self.next_file;
        self.next_file += 1;
        id
    }
    pub fn allocate_window(&mut self) -> u32 {
        let id = self.next_window;
        self.next_window += 1;
        id
    }
    pub fn owner(&self, current: &FileState, window: u32) -> Option<u64> {
        let has =
            |s: &FileState| s.window.id() == window || s.others.iter().any(|w| w.id() == window);
        if has(current) {
            Some(current.id)
        } else {
            self.peers
                .iter()
                .find(|(_, f)| has(&f.state))
                .map(|(id, _)| *id)
        }
    }
    /// [`file_identity`] of open file `id`: `current` or a peer. The
    /// registry's one way of saying which file a `FileId` is: Go to
    /// Related Record compares its target's owning file with it.
    pub fn identity(&self, current: &FileState, id: u64) -> Option<FileIdentity> {
        if id == current.id {
            return file_identity(current);
        }
        file_identity(&self.peers.get(&id)?.state)
    }

    /// An open file on `host` at `port` (the host's text in any case):
    /// `current` or a peer, by [`file_identity`].
    pub fn on_endpoint(&self, current: &FileState, host: &str, port: u16) -> Option<u64> {
        let on = |state: &FileState| {
            matches!(
                file_identity(state),
                Some(FileIdentity::Hosted { host: there, port: at, .. })
                    if at == port && there.eq_ignore_ascii_case(host)
            )
        };
        if on(current) {
            return Some(current.id);
        }
        self.peers
            .iter()
            .find(|(_, f)| on(&f.state))
            .map(|(id, _)| *id)
    }

    /// The open file that is `identity`: `current` or a peer.
    pub fn with_identity(&self, current: &FileState, identity: &FileIdentity) -> Option<u64> {
        if file_identity(current).as_ref() == Some(identity) {
            return Some(current.id);
        }
        self.peers
            .iter()
            .find(|(_, f)| file_identity(&f.state).as_ref() == Some(identity))
            .map(|(id, _)| *id)
    }

    /// The open file `name` means from hosted file `current`: on another
    /// host and port when it is named `fmnet:/host:port/Name`
    /// ([`om_data::file_refs::fmnet_name`], #976), else on `current`'s, by
    /// [`FileIdentity`]. One name on two hosts is two files. A file whose
    /// host and port are not known is matched by name.
    pub fn named(&self, current: &FileState, name: &str) -> Option<u64> {
        let (wanted, file) = match FileIdentity::from_fmnet(name) {
            Some(identity) => (
                Some(identity),
                om_data::file_refs::fmnet_target(name).map_or_else(String::new, |(_, _, f)| f),
            ),
            None => (
                current
                    .endpoint
                    .as_ref()
                    .filter(|_| current.context.local.is_none())
                    .map(|e| FileIdentity::hosted(&e.host, e.port, name)),
                name.to_owned(),
            ),
        };
        if let Some(id) = wanted
            .as_ref()
            .and_then(|wanted| self.with_identity(current, wanted))
        {
            return Some(id);
        }
        // Files whose host and port are not known (no host session).
        let key = hosted_key(&file);
        let at = |state: &FileState| {
            (wanted.is_none() || file_identity(state).is_none())
                && hosted_key(&state.context.file) == key
        };
        if at(current) {
            Some(current.id)
        } else {
            self.peers
                .iter()
                .find(|(_, f)| at(&f.state))
                .map(|(id, _)| *id)
        }
    }

    pub fn select(
        &mut self,
        id: u64,
        provider: &mut Box<dyn DataProvider>,
        s: &mut Shared,
    ) -> bool {
        if id == s.file.id {
            return true;
        }
        let Some(mut next) = self.peers.remove(&id) else {
            return false;
        };
        std::mem::swap(provider, &mut next.provider);
        std::mem::swap(&mut s.file, &mut next.state);
        self.peers.insert(next.state.id, next);
        provider.use_window(s.file.window.id());
        true
    }
    pub fn close_all(&mut self) {
        for (_, mut file) in std::mem::take(&mut self.peers) {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file.provider.close()));
        }
    }
}
impl Drop for OpenFiles {
    fn drop(&mut self) {
        self.close_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::session::Endpoint;

    fn state(id: u64, file: &str, endpoint: Option<(&str, u16)>) -> FileState {
        let mut state = Shared::new(Default::default()).file;
        state.id = id;
        state.context.file = file.into();
        state.endpoint = endpoint.map(|(host, port)| Endpoint {
            host: host.into(),
            port,
            file: file.into(),
            account: "Admin".into(),
            local: None,
        });
        state
    }

    fn peer(files: &mut OpenFiles, state: FileState) {
        let provider: Box<dyn DataProvider> = Box::new(om_data::mock::MockProvider::new());
        files.peers.insert(state.id, OpenFile { provider, state });
    }

    /// The registry's identity of a `FileId` (#976): `Peer` on this host
    /// and on another is two files, found by their host, port and name; a
    /// plain name means the current file's host, an `fmnet` name its own;
    /// a file whose host is not known is found by name.
    #[test]
    fn open_files_are_told_apart_by_host_port_and_name() {
        let root = state(0, "Root.fmp12", Some(("Here.invalid", 5003)));
        let mut files = OpenFiles::default();
        peer(&mut files, state(1, "Peer", Some(("other.invalid", 5100))));
        peer(
            &mut files,
            state(2, "Peer.fmp12", Some(("here.invalid", 5003))),
        );
        peer(&mut files, state(3, "Loose", None));
        assert_eq!(
            files.identity(&root, 1),
            Some(FileIdentity::hosted("other.invalid", 5100, "Peer"))
        );
        assert_eq!(
            files.identity(&root, 0),
            Some(FileIdentity::hosted("here.invalid", 5003, "Root"))
        );
        assert_eq!(files.identity(&root, 3), None);
        assert_eq!(files.identity(&root, 9), None);
        let there = FileIdentity::hosted("OTHER.invalid", 5100, "peer.FMP12");
        assert_eq!(files.with_identity(&root, &there), Some(1));
        assert_eq!(files.named(&root, "Peer"), Some(2));
        assert_eq!(
            files.named(&root, "fmnet:/other.invalid:5100/Peer"),
            Some(1)
        );
        assert_eq!(files.named(&root, "fmnet:/other.invalid:5101/Peer"), None);
        assert_eq!(files.named(&root, "Root"), Some(0));
        assert_eq!(files.named(&root, "Loose"), Some(3));
    }
}
