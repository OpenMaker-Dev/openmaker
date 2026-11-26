//! Open File and the file/window boundary of external script calls.
use super::windows::{same_window_key, WindowKey};
use super::{ScriptEvent, Shared, WorkerHost};
use crate::scripting::{databases::OpenFile, session::Endpoint, SessionContext};
use om_data::{
    file_refs::{hosted_key, FilePath},
    runtime::*,
};

#[derive(Clone, Copy)]
enum OpenReason {
    File,
    Related { new_window: bool },
}

/// An account the user typed in this step for related file `.0` (a
/// sign-in name): the step's own open of that file is sent it, and nothing
/// keeps it after the step.
pub(super) type Typed<'a> = Option<(&'a str, &'a crate::scripting::SignIn)>;

impl WorkerHost<'_> {
    pub(super) fn open_failed(&mut self, mut error: om_data::DataError) -> HostError {
        // An unclassified transport/catalog refusal is an open failure (802),
        // not the runtime's internal -1 code, which would force an alert.
        if error.code.is_none() {
            error.code = Some(802);
        }
        self.failed(error)
    }

    /// Ask for an account for file `name`, which refused the account it
    /// was sent or was sent none (#214, #991), as FileMaker Pro's Open File
    /// asks (#214's FileMaker Pro run): each answer is one `attempt` (one
    /// Login), and a refused answer asks again with its reason. Cancel
    /// gives 1, or after a refused answer that refusal's code (212); with
    /// no one to answer, `refused` itself, without a Login [I: OpenMaker's
    /// choice]. These end the step. An answer that is not refused but does
    /// not open the file (none of that name there, no connection) is
    /// `Ok(Err)`, for the caller to go on as for any failed path.
    pub(super) fn ask_for_account<T>(
        &mut self,
        name: &str,
        refused: om_data::DataError,
        mut attempt: impl FnMut(&mut Self, &str, &str) -> Result<T, om_data::DataError>,
    ) -> HostResult<Result<T, om_data::DataError>> {
        use crate::scripting::{Answer, Prompt};
        let mut account = om_data::host::machine::user_name();
        let mut last: Option<om_data::DataError> = None;
        // Esc, Cmd-. or a closing window, pending before or while the
        // dialog was up: no account is sent, and the abort stays pending
        // for the runtime to act on.
        let stopped = |host: &mut Self| {
            host.connection_lost()
                || host.s.abort.load(std::sync::atomic::Ordering::SeqCst)
                || host.s.interrupted
                || host.s.halt
        };
        loop {
            if stopped(self) {
                return Err(self.open_failed(last.unwrap_or(refused)));
            }
            let prompt = Prompt::SignIn {
                file: om_data::file_refs::file_stem(name).to_owned(),
                account: account.clone(),
                error: last.as_ref().map(|e| e.message.clone()),
            };
            let answer = match self.ask(prompt) {
                Some(Answer::SignIn(Some(answer))) => answer,
                Some(Answer::SignIn(None)) => {
                    return Err(match last {
                        Some(error) => self.open_failed(error),
                        None => ErrorCode::CANCELED.into(),
                    })
                }
                _ => return Err(self.open_failed(last.unwrap_or(refused))),
            };
            if stopped(self) {
                return Err(ErrorCode::CANCELED.into());
            }
            account.clone_from(&answer.account);
            let credentials = (answer.account.as_str(), answer.password.as_str());
            match attempt(self, credentials.0, credentials.1) {
                Ok(done) => return Ok(Ok(done)),
                Err(error) if self.p().peer_refused(name, Some(credentials)) => {
                    last = Some(error);
                }
                Err(error) => return Ok(Err(error)),
            }
        }
    }

    /// The script's own file's schema, read again from its provider after
    /// a related file opened or a sign-in: the rest of the script reads the
    /// file's tables, as the window does.
    pub(super) fn reread_schema(&mut self) {
        self.s.file.schema = Some(self.p().calc_schema().ok().flatten());
        self.s.file.layouts = None;
        self.s.file.rendered.clear();
        *self.s.file.cache.borrow_mut() = Default::default();
        self.send(ScriptEvent::SchemaChanged);
    }

    /// [`Self::ask_for_account`] for desktop file `name`, opened with the
    /// account typed (`DataProvider::open_peer_as`). A file the first
    /// file's relationships show is signed in to there too, so their fields
    /// show it, as in FileMaker Pro.
    fn sign_in_peer(
        &mut self,
        name: &str,
        refused: om_data::DataError,
    ) -> HostResult<Result<Box<dyn om_data::DataProvider>, om_data::DataError>> {
        self.ask_for_account(name, refused, |host, account, password| {
            let provider = host.p().open_peer_as(name, account, password)?;
            let key = hosted_key(name);
            let related = host
                .p()
                .related_sign_ins()
                .into_iter()
                .find(|s| hosted_key(&s.file) == key);
            // One more Login, of the pair the file just admitted.
            if let Some(related) = related {
                if host
                    .p()
                    .sign_in_related(&related.file, account, password)
                    .is_ok()
                {
                    host.reread_schema();
                }
            }
            Ok(provider)
        })
    }

    /// Open `name`, a file the current file's data source names, and
    /// whether it refused the account it was sent (so another may be asked
    /// for). A file on another host or port, with a file of that host's
    /// open already, opens through that file: it is sent the account that
    /// host admitted, in that file's session there, as FileMaker Pro opens
    /// another file of a host in the session it has there
    /// (network-protocol.md 5.5); what that file answers stands, a failure
    /// to reach the host included, which asks for no account. Else through
    /// the current file: an account that host admitted for it, else its
    /// own once (`DataProvider::open_peer`). An account the user typed for
    /// the file in this step (`typed`) is sent instead.
    fn open_peer_file(
        &mut self,
        name: &str,
        typed: Typed<'_>,
    ) -> (
        Result<Box<dyn om_data::DataProvider>, om_data::DataError>,
        bool,
    ) {
        if let Some((_, sign_in)) = typed.filter(|(file, _)| hosted_key(file) == hosted_key(name)) {
            let credentials = (sign_in.account.as_str(), sign_in.password.as_str());
            let opened = self.p().open_peer_as(name, credentials.0, credentials.1);
            let refused = opened.is_err() && self.p().peer_refused(name, Some(credentials));
            return (opened, refused);
        }
        let current = self.s.file.id;
        let there = om_data::file_refs::fmnet_target(name)
            .and_then(|(host, port, _)| self.files.on_endpoint(&self.s.file, &host, port))
            .filter(|id| *id != current);
        if let Some(file) = there.and_then(|id| self.files.peers.get_mut(&id)) {
            let opened = file.provider.open_peer(name);
            let refused = opened.is_err() && file.provider.peer_refused(name, None);
            return (opened, refused);
        }
        let opened = self.p().open_peer(name);
        let refused = opened.is_err() && self.p().peer_refused(name, None);
        (opened, refused)
    }

    pub(super) fn switch_file(&mut self, id: FileId) -> HostResult {
        if !self.files.select(id, self.provider.get_mut(), self.s) {
            return Err(ErrorCode::FILE_MISSING.into());
        }
        self.sync_layout(true);
        Ok(())
    }

    pub(super) fn database_names(&mut self, source: FileSource<'_>) -> HostResult<Vec<String>> {
        if matches!(source, FileSource::Current) {
            return Ok(vec![self.s.file.context.file.clone()]);
        }
        let sources = self.p().database_sources().map_err(|e| self.failed(e))?;
        let paths = match source {
            FileSource::Current => return Ok(vec![self.s.file.context.file.clone()]),
            FileSource::Reference(node) => {
                let id = om_data::specs::data_source(node).ok_or(ErrorCode::INVALID)?;
                sources
                    .into_iter()
                    .find(|s| s.id == id)
                    .ok_or(ErrorCode::FILE_MISSING)?
                    .paths
            }
            FileSource::Path(text) => {
                if let Some(source) = sources.into_iter().find(|s| s.name == text) {
                    source.paths
                } else {
                    text.lines()
                        .flat_map(|s| s.split('\r'))
                        .filter(|s| !s.is_empty() && !s.starts_with('#'))
                        .filter_map(FilePath::calculated)
                        .collect()
                }
            }
        };
        self.hosted_database_names(paths)
    }

    /// The files `paths` name that the current file can open, in order: on
    /// its host by name; on another host or port, named
    /// `fmnet:/host:port/Name` ([`om_data::file_refs::fmnet_name`], #976),
    /// which the provider opens only with an account typed for it; or for
    /// a file on this computer by a plain name or a local path supplied by
    /// a file variable.
    fn hosted_database_names(&mut self, paths: Vec<FilePath>) -> HostResult<Vec<String>> {
        let host = self.s.file.context.host.clone();
        let port = self.p().server_port();
        let local = self.p().local_path().is_some();
        if let [path] = paths.as_slice() {
            if let Some(variable) = path.global_variable().filter(|_| local) {
                let value = self
                    .p()
                    .local_source_variable(variable)
                    .ok_or(ErrorCode(802))?
                    .to_owned();
                return if self.p().local_peer_path(&value).is_some() {
                    Ok(vec![value])
                } else {
                    Err(HostError::Unsupported)
                };
            }
        }
        let names: Vec<_> = paths
            .into_iter()
            .filter_map(|path| {
                if local {
                    return path.relative_name();
                }
                path.same_host_name(&host, port).or_else(|| {
                    let (there, there_port) = path.fmnet_endpoint()?;
                    om_data::file_refs::is_plain_name(&path.name)
                        .then(|| om_data::file_refs::fmnet_name(there, there_port, &path.name))
                })
            })
            .collect();
        if names.is_empty() {
            Err(HostError::Unsupported)
        } else {
            Ok(names)
        }
    }

    pub(super) fn open_database_from(
        &mut self,
        from: FileId,
        source: FileSource<'_>,
        hidden: bool,
    ) -> HostResult {
        let current = self.s.file.id;
        self.switch_file(from)?;
        let result = self.open_database(source, hidden).map(|_| ());
        self.switch_file(current)?;
        result
    }

    pub(super) fn close_database_from(
        &mut self,
        from: FileId,
        source: FileSource<'_>,
    ) -> HostResult {
        let current = self.s.file.id;
        self.switch_file(from)?;
        let result = self.close_database(source);
        // A named peer may have been closed. Restore a surviving context.
        if current == from || self.files.peers.contains_key(&current) {
            self.switch_file(current)?;
        }
        result
    }

    pub(super) fn open_database(
        &mut self,
        source: FileSource<'_>,
        hidden: bool,
    ) -> HostResult<FileId> {
        if matches!(source, FileSource::Current) {
            return Err(HostError::Unsupported);
        }
        let names = self.database_names(source)?;
        self.open_database_names(names, Some(hidden), OpenReason::File, None)
    }

    // Implicit script resolution preserves existing window visibility. Explicit
    // Open File applies its hidden option even when it reuses a session.
    fn open_database_names(
        &mut self,
        names: Vec<String>,
        hidden: Option<bool>,
        reason: OpenReason,
        typed: Typed<'_>,
    ) -> HostResult<FileId> {
        let mut error = HostError::Error(ErrorCode::FILE_MISSING);
        for name in names {
            if let Some(id) = self.already_open(&name) {
                let from = self.s.file.id;
                self.switch_file(id)?;
                if self.connection_lost() {
                    self.switch_file(from)?;
                    return Err(ErrorCode::UNAVAILABLE.into());
                }
                if let Some(hidden) = hidden {
                    self.s.file.window.state.hidden = hidden;
                    for window in &mut self.s.file.others {
                        window.state.hidden = hidden;
                    }
                    self.send_file_state();
                    if !hidden {
                        self.send(ScriptEvent::Viewport(egui::ViewportCommand::Focus));
                    }
                }
                self.switch_file(from)?;
                return Ok(id);
            }
            let (opened, refused) = self.open_peer_file(&name, typed);
            let mut provider = match opened {
                Ok(provider) => provider,
                // A file that refused the account sent it asks for another,
                // as FileMaker Pro's Open File does (#214): only Open File
                // (`hidden` given), since what other steps that first need
                // such a file do is unmeasured. A file on another host or
                // port (named `fmnet:/host:port/Name`) asks from any step
                // that first needs it, so a script calling one there goes on
                // with the account typed for it (#991), once that host
                // refused the one it was sent ([`Self::open_peer_file`]).
                Err(e)
                    if (hidden.is_some() || om_data::file_refs::fmnet_target(&name).is_some())
                        && refused =>
                {
                    match self.sign_in_peer(&name, e)? {
                        Ok(provider) => provider,
                        Err(e) => {
                            error = self.open_failed(e);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    error = self.open_failed(e);
                    continue;
                }
            };
            // The layouts the file's account may see (session-identity.md
            // 2.2).
            let access = provider.account_info().set_access;
            let shown = |id: u32| {
                access
                    .as_ref()
                    .is_none_or(|a| a.shows_layout(u64::from(id)))
            };
            // No script catalog: a script it runs is read alone
            // (`lookup_script`), as FileMaker Pro 26 reads one (#1053).
            let loaded = (|| {
                let schema = provider.calc_schema()?;
                let layouts = provider.layouts()?;
                // Its "Switch to layout", else its first layout, also when
                // the account may not see it: the window shows FileMaker
                // Pro's blank page there (session-identity.md 2.5, #696).
                let first = schema
                    .as_ref()
                    .and_then(|s| s.file_options.as_ref()?.default_layout())
                    .and_then(|id| u32::try_from(id).ok())
                    .filter(|id| layouts.iter().any(|l| l.id == *id))
                    .or_else(|| layouts.first().map(|l| l.id))
                    .ok_or_else(|| {
                        om_data::DataError::with_code(105, "No layout in the opened file.")
                    })?;
                Ok((schema, layouts, first))
            })();
            let (schema, layouts, first) = match loaded {
                Ok(loaded) => loaded,
                Err(e) => {
                    provider.close();
                    error = self.open_failed(e);
                    continue;
                }
            };
            self.pump(None);
            if self.connection_lost()
                || self.s.interrupted
                || self.s.abort.load(std::sync::atomic::Ordering::SeqCst)
                || self.p().close_requests().is_some_and(|c| c.pending())
            {
                provider.close();
                return Err(ErrorCode::CANCELED.into());
            }
            if let Some(e) = provider.connection_lost() {
                provider.close();
                return Err(self.open_failed(e));
            }
            let id = self.files.allocate_file();
            let window = self.files.allocate_window();
            let mut state = Shared::new(self.s.abort.clone()).file;
            state.id = id;
            // A file on this computer is the file at its path, by its own
            // name whatever name reached it; one on another host is that
            // host's file by its name there (#976).
            let local = provider.local_path();
            let elsewhere = om_data::file_refs::fmnet_target(&name);
            state.context = SessionContext {
                host: elsewhere.as_ref().map_or_else(
                    || self.s.file.context.host.clone(),
                    |(host, _, _)| host.clone(),
                ),
                file: match (&local, &elsewhere) {
                    (Some(path), _) => path
                        .file_name()
                        .map_or(name, |n| n.to_string_lossy().into_owned()),
                    (None, Some((_, _, file))) => file.clone(),
                    (None, None) => name,
                },
                account: provider
                    .session_account()
                    .unwrap_or_else(|| self.s.file.context.account.clone()),
                local: local.clone(),
            };
            // `open_peer` opened it on the opener's host and port (or at
            // its path on this computer), or on the other host and port
            // its name gives, with the account it was opened with: a
            // reconnect goes there too.
            state.endpoint = match &elsewhere {
                Some((host, port, _)) => Some(Endpoint {
                    host: host.clone(),
                    port: *port,
                    file: state.context.file.clone(),
                    account: state.context.account.clone(),
                    local: None,
                }),
                None => self.s.file.endpoint.as_ref().map(|opener| Endpoint {
                    host: opener.host.clone(),
                    port: opener.port,
                    file: state.context.file.clone(),
                    account: state.context.account.clone(),
                    local,
                }),
            };
            state.window.state.id = window;
            state.window.state.hidden = hidden.unwrap_or(true);
            state.window.state.layout = Some(first);
            state.window.original_layout = Some(first);
            state.schema = Some(schema);
            state.layouts = Some(layouts);
            state.catalog = None;
            provider.use_window(window);
            self.files.peers.insert(id, OpenFile { provider, state });
            let from = self.s.file.id;
            self.switch_file(id)?;
            self.send_file_state();
            if hidden == Some(false) {
                self.send(ScriptEvent::Viewport(egui::ViewportCommand::Focus));
            }
            // Same trigger order as the first desktop window. The runtime
            // stacks the last trigger first. GTRR enters its final layout
            // itself, and its new window suppresses file-opening triggers
            // (Claris Go to Related Record, Notes).
            // A hidden opening layout is never entered: it reads nothing
            // and runs no layout trigger, as in the first desktop window
            // [I].
            if matches!(reason, OpenReason::File) && shown(first) {
                self.after_layout_entered();
            }
            let options = self.schema().and_then(|s| s.file_options.clone());
            if let Some(options) =
                options.filter(|_| !matches!(reason, OpenReason::Related { new_window: true }))
            {
                for event in [TriggerEvent::WindowOpen, TriggerEvent::FirstWindowOpen] {
                    if let Some(script) = options.trigger(event.id()) {
                        let mut trigger = Trigger::new(event, id, script);
                        trigger.window = Some(u64::from(window));
                        trigger.from_step = true;
                        self.triggers.push(trigger);
                    }
                }
            }
            self.switch_file(from)?;
            return Ok(id);
        }
        Err(error)
    }

    fn send_file_state(&mut self) {
        let host_version = self.p().host_application_version().unwrap_or_default();
        let privileges = self.p().account_privileges();
        // Without it the window this builds has empty layout menus outside
        // Layout mode (#855).
        let layout_menu = self.p().layout_menu();
        self.send(ScriptEvent::FileOpened(Box::new(
            crate::scripting::OpenedFile {
                id: self.s.file.id,
                hidden: self.s.file.window.state.hidden,
                context: self.s.file.context.clone(),
                window: self.s.file.window.state.clone(),
                layouts: self.s.file.layouts.clone().unwrap_or_default(),
                layout_menu,
                catalog: self.s.file.catalog.clone(),
                schema: self.schema().cloned(),
                host_version,
                privileges,
            },
        )));
    }

    /// Catalog member `id` of `file` with its steps, to run it: from the
    /// session's whole catalog when it has read one, else read alone
    /// (`DataProvider::script`, #1053), once per session. `None` when the
    /// file has no such member or it could not be read: the runtime's
    /// error 104, as before.
    pub(super) fn lookup_script(
        &mut self,
        file: FileId,
        id: u64,
    ) -> Option<std::sync::Arc<om_data::Script>> {
        let from = self.s.file.id;
        self.switch_file(file).ok()?;
        let script = self.s.file.scripts.get(&id).cloned().or_else(|| {
            let script = match &self.s.file.catalog {
                Some(catalog) => catalog.scripts.iter().find(|s| s.id == id).cloned(),
                None => self.p().script(id).ok().flatten(),
            };
            script.map(std::sync::Arc::new)
        });
        if let Some(s) = &script {
            self.s.file.scripts.insert(id, s.clone());
        }
        self.switch_file(from).ok()?;
        script
    }

    /// Whether the account file `file` was opened with may run script
    /// `id` (session-identity.md 2.2); a file that cannot be reached runs
    /// nothing.
    pub(super) fn script_executable(&mut self, file: FileId, id: u64) -> bool {
        let from = self.s.file.id;
        if file == from {
            return self.runs_script(id);
        }
        if self.switch_file(file).is_err() {
            return false;
        }
        let runs = self.runs_script(id);
        self.switch_file(from).is_ok() && runs
    }

    /// The id of `file`'s script named `name` (Perform Script by name, a
    /// plug-in's script by name): from the session's whole catalog when it
    /// has read one. Else the calculation schema's list (the catalog's
    /// index on a hosted file, #1052) names a candidate, which its own
    /// member, read to run it anyway (`lookup_script`), must confirm: a
    /// script, still of that name. A separator the index lists as a script,
    /// a script renamed or added since the index was read, or one that
    /// could not be read, is looked up in the whole catalog read now, as
    /// before (#1053).
    pub(super) fn lookup_script_name(&mut self, file: FileId, name: &str) -> Option<u64> {
        let from = self.s.file.id;
        self.switch_file(file).ok()?;
        let named = |kind: om_data::ScriptKind, script: &str| {
            kind == om_data::ScriptKind::Script && script.eq_ignore_ascii_case(name)
        };
        let in_catalog = |catalog: &om_data::ScriptCatalog| {
            catalog
                .scripts
                .iter()
                .find(|s| named(s.kind, &s.name))
                .map(|s| s.id)
        };
        let result = match &self.s.file.catalog {
            Some(catalog) => in_catalog(catalog),
            None => {
                let candidate = self.schema().and_then(|schema| {
                    schema
                        .scripts()
                        .iter()
                        .find(|s| named(s.kind, &s.name))
                        .map(|s| s.id)
                });
                let confirmed = candidate.filter(|&id| {
                    self.lookup_script(file, id)
                        .is_some_and(|script| named(script.kind, &script.name))
                });
                match confirmed {
                    Some(id) => Some(id),
                    None => {
                        self.s.file.catalog = self.p().scripts().ok();
                        self.s.file.catalog.as_ref().and_then(in_catalog)
                    }
                }
            }
        };
        self.switch_file(from).ok()?;
        result
    }

    pub(super) fn resolve_database(
        &mut self,
        from: FileId,
        source: FileSource<'_>,
    ) -> HostResult<FileId> {
        let current = self.s.file.id;
        self.switch_file(from)?;
        let result = self
            .database_names(source)
            .and_then(|names| self.ensure_database_names(names));
        self.switch_file(current)?;
        result
    }

    fn ensure_database_names(&mut self, names: Vec<String>) -> HostResult<FileId> {
        // Reuse and opening must share one ordered search: an already-open
        // fallback cannot outrank an available earlier path.
        self.open_database_names(names, None, OpenReason::File, None)
    }

    pub(super) fn resolve_named_source(&mut self, from: FileId, name: &str) -> HostResult<FileId> {
        self.resolve_source(from, name, OpenReason::File, None)
    }

    /// `name`'s file for Go to Related Record; `typed`, an account the
    /// step signed in to its related file with ([`Typed`]).
    pub(super) fn resolve_related_source(
        &mut self,
        name: &str,
        new_window: bool,
        typed: Typed<'_>,
    ) -> HostResult<FileId> {
        self.resolve_source(
            self.s.file.id,
            name,
            OpenReason::Related { new_window },
            typed,
        )
    }

    fn resolve_source(
        &mut self,
        from: FileId,
        name: &str,
        reason: OpenReason,
        typed: Typed<'_>,
    ) -> HostResult<FileId> {
        let current = self.s.file.id;
        self.switch_file(from)?;
        let result = (|| {
            let sources = self.p().database_sources().map_err(|e| self.failed(e))?;
            let source = sources
                .into_iter()
                .find(|s| s.name == name)
                .ok_or(ErrorCode::FILE_MISSING)?;
            let names = self.hosted_database_names(source.paths)?;
            self.open_database_names(names, None, reason, typed)
        })();
        self.switch_file(current)?;
        result
    }

    pub(super) fn database_window(&mut self, file: FileId) -> HostResult<Option<WindowId>> {
        if file == self.s.file.id {
            return Ok(Some(u64::from(self.s.file.window.id())));
        }
        self.files
            .peers
            .get(&file)
            .map(|f| Some(u64::from(f.state.window.id())))
            .ok_or(ErrorCode::FILE_MISSING.into())
    }

    pub(super) fn select_database_window(&mut self, window: WindowId) -> HostResult {
        let window = u32::try_from(window).map_err(|_| ErrorCode::WINDOW_MISSING)?;
        let file = self
            .files
            .owner(&self.s.file, window)
            .ok_or(ErrorCode::WINDOW_MISSING)?;
        if file != self.s.file.id {
            self.flush_background();
            self.switch_file(file)?;
        }
        if window != self.s.file.window.id() {
            let index = self
                .s
                .file
                .others
                .iter()
                .position(|w| w.id() == window)
                .ok_or(ErrorCode::WINDOW_MISSING)?;
            self.make_current(index);
        }
        self.s.file.window.reenter();
        Ok(())
    }

    pub(super) fn close_database(&mut self, source: FileSource<'_>) -> HostResult {
        let target = match source {
            // Current is already a live identity, including when a local
            // file was opened through an alias in another folder.
            FileSource::Current => self.s.file.id,
            source => {
                let names = self.database_names(source)?;
                names
                    .iter()
                    .find_map(|name| self.already_open(name))
                    .ok_or(ErrorCode::FILE_MISSING)?
            }
        };
        // A file on this computer by its own name (local_path), which is
        // what its family's relationships know it by; a hosted one by its
        // host, port and name (`fmnet:/host:port/Name`), so that each
        // file's relationships are asked about this file, not one of the
        // same name on another host (#976).
        let (context, local) = if target == self.s.file.id {
            (&self.s.file.context, self.provider.get_mut().local_path())
        } else {
            let file = &self.files.peers[&target];
            (&file.state.context, file.provider.local_path())
        };
        let name = match (
            local.as_deref().and_then(std::path::Path::file_name),
            self.files.identity(&self.s.file, target),
        ) {
            (Some(name), _) => name.to_string_lossy().into_owned(),
            (None, Some(om_data::file_refs::FileIdentity::Hosted { host, port, .. })) => {
                om_data::file_refs::fmnet_name(&host, port, &context.file)
            }
            _ => context.file.clone(),
        };
        if target != self.s.file.id
            && self
                .p()
                .references_peer(&name)
                .map_err(|e| self.failed(e))?
        {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        for (id, file) in &mut self.files.peers {
            if *id != target
                && file
                    .provider
                    .references_peer(&name)
                    .map_err(super::data_error)?
            {
                return Err(ErrorCode::UNAVAILABLE.into());
            }
        }
        // Closing a file held by a script frame or transaction would strand
        // its caller or transaction. Refuse before saves or disconnection.
        if self.s.active_files.contains(&target)
            && (target != self.s.file.id || self.s.active_files.len() > 1)
        {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        if target == self.s.file.id {
            self.commit_window()?;
            self.s.halt = true;
            self.send(ScriptEvent::CloseFile);
            return Ok(());
        }
        let from = self.s.file.id;
        self.switch_file(target)?;
        // Save every window before removing any. A refusal retains the file.
        let windows: Vec<_> = std::iter::once(&self.s.file.window)
            .chain(&self.s.file.others)
            .map(|w| w.id())
            .collect();
        let result: HostResult = (|| {
            for window in windows {
                self.select_database_window(u64::from(window))?;
                self.commit_window()?;
            }
            Ok(())
        })();
        self.switch_file(from)?;
        result?;
        if let Some(mut file) = self.files.peers.remove(&target) {
            file.provider.close();
            self.s.closed_files.push(target);
            self.send(ScriptEvent::FileClosed(target));
        }
        Ok(())
    }

    /// The open file `name` names from the current file: on its host, by
    /// name; beside a file on this computer, by path with links followed,
    /// whatever name reached it (`DataProvider::local_path`).
    fn already_open(&mut self, name: &str) -> Option<FileId> {
        let Some(path) = self.p().local_peer_path(name) else {
            return self.files.named(&self.s.file, name);
        };
        let path = std::fs::canonicalize(path).ok()?;
        if self.p().local_path().as_deref() == Some(path.as_path()) {
            return Some(self.s.file.id);
        }
        self.files
            .peers
            .iter()
            .find(|(_, f)| f.provider.local_path().as_deref() == Some(path.as_path()))
            .map(|(id, _)| *id)
    }

    pub(super) fn named_database_window(&self, key: WindowKey, name: &str) -> Option<WindowId> {
        self.files.peers.values().find_map(|f| {
            std::iter::once(&f.state.window)
                .chain(f.state.others.iter().rev())
                .find(|w| {
                    !w.state.hidden
                        && match key {
                            WindowKey::ExactName => w.state.title.as_deref().map_or_else(
                                || hosted_key(&f.state.context.file) == hosted_key(name),
                                |title| title == name,
                            ),
                            WindowKey::Name => w.state.title.as_deref().map_or_else(
                                || hosted_key(&f.state.context.file) == hosted_key(name),
                                |title| same_window_key(title, name),
                            ),
                            WindowKey::Uuid => same_window_key(&w.state.uuid, name),
                        }
                })
                .map(|w| u64::from(w.id()))
        })
    }
}
