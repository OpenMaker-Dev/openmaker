//! Native script reads and saves, and Perform Script on Server
//! (script-runtime.md 11) through the session.
//!
//! The Script Workspace saves a script's steps through the shared step
//! save (`authoring.rs`, `NativeProvider::save_script_steps`), which
//! `om-author`'s apply makes too: on a host that offers OpenMaker's
//! authoring extension to this account, at the revision the scripts were
//! last read at, then read back.
//!
//! Its catalog changes (new, duplicated, renamed, moved and deleted
//! scripts, and the two script options) are saved with FileMaker Pro's own
//! script save (scripts.md, "Saving scripts to a host"), as Manage
//! Database saves the schema (`schema.rs`): the index `[17, 1]` and each
//! member locked, the members (a deleted one emptied) and then the index
//! upgraded, uploaded (op13) and released with attention 2, then the host
//! asked to confirm that the file on disk holds them (the authoring
//! extension's Save: op13's reply says nothing about the disk) and the
//! catalog read back: the save is reported only when the host stores what
//! was sent. What is sent is planned by `om_data::script_catalog`, on the
//! catalog read under the index's lock; an index another client changed
//! since the workspace read the scripts is not saved over (306). New
//! folders and separators are refused there.
//!
//! Agent edits can also save existing scripts on FileMaker Server by the
//! native transaction below. Script Workspace still requires the extension.
use std::time::Duration;

use om_data::calc::{Ctx, Date, Locale, Value};
use om_data::runtime::{ErrorCode, ServerScriptCall, ServerScriptResult};
use om_data::schema_draft::Stamp;
use om_data::script_catalog::{self, CatalogSave};
use om_data::{DataError, Script, ScriptCatalog, ServerScriptCallback};
use om_hbam::{serial, OwnedNode};
use om_protocol::authoring::{catalog_revision, content_revision, status, Revision};
use om_protocol::write::{LockReply, LOCK_WRITE};

use crate::authoring::{Authoring, AuthoringError, ScriptDefinitions};
use crate::{failure, NativeProvider};

/// The exclusive lock a member or the index is upgraded to for its upload,
/// and the attention its release sends (schema-path.md 4.2, 9).
const LOCK_EXCLUSIVE: u16 = 10;
const ATTENTION_CATALOG: u32 = 0x2;
/// FileMaker's errors: 3 unsupported, 301 "Record is in use by another
/// user", 306 "Record modification ID does not match".
const UNSUPPORTED: i32 = 3;
const IN_USE: i32 = 301;
const CHANGED_SINCE: i32 = 306;
/// Said with every refused or failed catalog save.
const UNCHANGED: &str = "Nothing was saved.";

/// The index path `[17, 1]` and a member's `[17, 5, S]`.
fn index_path() -> Vec<Vec<u8>> {
    vec![vec![script_catalog::SCRIPTS], vec![script_catalog::INDEX]]
}

fn member_path(segment: &[u8]) -> Vec<Vec<u8>> {
    vec![
        vec![script_catalog::SCRIPTS],
        vec![script_catalog::MEMBERS],
        segment.to_vec(),
    ]
}

/// The revision of the index `[17, 1]` as stored, its modification stamps
/// left out (a step save stamps it, and changes nothing a catalog change
/// is made on); a file without one has the revision of an empty node.
fn index_revision(scripts: &ScriptDefinitions) -> Revision {
    match &scripts.index {
        Some(index) => catalog_revision(index),
        None => catalog_revision(&OwnedNode::default()),
    }
}

/// One node a catalog save sends: a member or the index uploaded whole,
/// or a deleted member emptied (`node` `None`), and how an error names it.
struct NodeUpload<'a> {
    path: Vec<Vec<u8>>,
    node: Option<&'a OwnedNode>,
    what: String,
}

/// Why a catalog save failed: before anything was uploaded, or after.
enum Failure {
    NotSent(DataError),
    Partial(DataError),
}

/// A host error code as the script runtime numbers it: FileMaker's
/// unknown error `0xffff` is -1.
fn error_code(code: u16) -> ErrorCode {
    ErrorCode(if code == u16::MAX {
        -1
    } else {
        i32::from(code)
    })
}

/// The parameter as the text op30 carries (the runtime passes text
/// already; anything else is shown in en-US).
fn parameter_text(parameter: Option<&Value>) -> String {
    match parameter {
        None => String::new(),
        Some(Value::Text(text)) => text.as_str().to_owned(),
        Some(other) => {
            let locale = Locale::en_us();
            let today = Date::from_ymd(2001, 1, 1).expect("a valid date");
            other.to_text(&Ctx::new(&locale, today)).into_string()
        }
    }
}

impl NativeProvider {
    /// Perform Script on Server, waiting or not (11.1-11.3).
    pub(crate) fn server_script(
        &mut self,
        call: &ServerScriptCall,
    ) -> Result<ServerScriptResult, DataError> {
        self.require_open()?;
        let session = self.session.as_mut().expect("open session");
        let reply = session
            .perform_script_on_server(
                call.script,
                &parameter_text(call.parameter.as_ref()),
                call.wait,
            )
            .map_err(|e| failure("perform script on server", e))?;
        // The script's changes came with the reply; pages read from now on
        // see them.
        self.invalidate_calc_notifications();
        Ok(ServerScriptResult {
            result: call.wait.then(|| Value::text(reply.result)),
            error: error_code(reply.error),
        })
    }

    /// Perform Script on Server with Callback (11.4).
    pub(crate) fn server_script_with_callback(
        &mut self,
        call: &ServerScriptCall,
        callback: u32,
    ) -> Result<ErrorCode, DataError> {
        self.require_open()?;
        let session = self.session.as_mut().expect("open session");
        let reply = session
            .perform_script_on_server_with_callback(
                call.script,
                &parameter_text(call.parameter.as_ref()),
                callback,
            )
            .map_err(|e| failure("perform script on server", e))?;
        Ok(error_code(reply.error))
    }

    /// The callback results the host sent, waiting up to `wait` for one.
    pub(crate) fn callback_results(
        &mut self,
        wait: Duration,
    ) -> Result<Vec<ServerScriptCallback>, DataError> {
        self.require_open()?;
        let session = self.session.as_mut().expect("open session");
        session
            .poll(wait)
            .map_err(|e| failure("read script results from the server", e))?;
        Ok(session
            .take_psos_results()
            .into_iter()
            .map(|r| ServerScriptCallback {
                callback: r.callback,
                error: error_code(r.error),
                result: r.result,
            })
            .collect())
    }

    /// The scripts as stored now, with the revision of each recorded for
    /// its step save; writable only where this connection may save steps.
    pub(crate) fn read_scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        let stored = self.script_definitions()?;
        self.script_revisions = stored
            .definitions
            .iter()
            .map(|(id, definition)| (*id, content_revision(definition)))
            .collect();
        Ok(self.as_writable(stored.catalog))
    }

    /// Catalog member `id` with its steps (`DataProvider::script`): its
    /// definition `[17, 5, S]` alone, one request, as FileMaker Pro 26
    /// reads the script a file trigger runs (network-protocol.md 5.5,
    /// #1053). `None` when the host has no such member.
    pub(crate) fn read_script(&mut self, id: u64) -> Result<Option<Script>, DataError> {
        self.require_open()?;
        let segment = serial::encode(id);
        let session = self.session.as_mut().expect("open session");
        let Some(member) = crate::download_node(session, member_path(&segment))? else {
            return Ok(None);
        };
        let tree = crate::download_tree(std::slice::from_ref(&member))?;
        let node = tree
            .at(&[
                u64::from(script_catalog::SCRIPTS),
                u64::from(script_catalog::MEMBERS),
            ])
            .and_then(|members| members.children.iter().find(|n| n.segment.0 == segment))
            .ok_or_else(|| DataError::new(format!("The server sent no script {id}.")))?;
        Script::decode(node)
            .map(Some)
            .map_err(|error| failure("decode server script", error))
    }

    /// The scripts as [`NativeProvider::read_scripts`] reads them, for the
    /// Script Workspace: its catalog changes are made on this catalog.
    pub(crate) fn read_scripts_for_editing(&mut self) -> Result<ScriptCatalog, DataError> {
        let stored = self.script_definitions()?;
        self.script_revisions = stored
            .definitions
            .iter()
            .map(|(id, definition)| (*id, content_revision(definition)))
            .collect();
        self.script_edit_base = Some(index_revision(&stored));
        Ok(self.as_writable(stored.catalog))
    }

    /// `catalog`, writable where this connection may save scripts, else
    /// with the reason.
    fn as_writable(&mut self, mut catalog: ScriptCatalog) -> ScriptCatalog {
        if self.read_only() {
            catalog.writable = false;
            catalog.unavailable_reason = Some(
                "Scripts can be run and drafted here, not saved. This file is open read-only."
                    .into(),
            );
            return catalog;
        }
        match self.script_saving() {
            Authoring::Available { .. } => {
                catalog.writable = true;
                catalog.unavailable_reason = None;
            }
            Authoring::Denied(reason) | Authoring::Unavailable(reason) => {
                catalog.writable = false;
                catalog.unavailable_reason = Some(format!(
                    "Scripts can be run and drafted here, not saved. {reason}"
                ));
            }
        }
        catalog
    }

    /// Whether this connection may save script steps: asked once per
    /// connection; a failure to ask is no.
    fn script_saving(&mut self) -> Authoring {
        if let Some(known) = &self.script_saving {
            return known.clone();
        }
        match self.script_extension_authoring() {
            Ok(answer) => {
                self.script_saving = Some(answer.clone());
                answer
            }
            Err(error) => Authoring::Unavailable(format!(
                "Whether the host saves scripts could not be asked: {}",
                error.message
            )),
        }
    }

    /// Replace only the steps of a script through FileMaker's catalog save.
    /// Acquire both exclusive locks and serialize both nodes before either
    /// upload so a lock refusal or an invalid payload cannot split the save.
    pub(crate) fn save_native_script_steps(
        &mut self,
        edited: &Script,
        base: &OwnedNode,
    ) -> Result<om_protocol::authoring::Replaced, AuthoringError> {
        let invalid = |message| native_script_refusal(status::INVALID, message);
        if edited.kind != om_data::ScriptKind::Script {
            return Err(invalid(
                "Only the steps of an existing script can be saved.".into(),
            ));
        }
        edited
            .check_structure()
            .map_err(|e| invalid(e.to_string()))?;
        let key = serial::encode(edited.id);
        let member_path = [
            &[script_catalog::SCRIPTS][..],
            &[script_catalog::MEMBERS],
            &key,
        ];
        let index_path = [&[script_catalog::SCRIPTS][..], &[script_catalog::INDEX]];
        let member_txn = self.new_transaction();
        let index_txn = self.new_transaction();
        let paths: [(&[&[u8]], u32); 2] = [(&member_path, member_txn), (&index_path, index_txn)];
        let mut locks = [0; 2];
        let mut attention = [0; 2];
        let result = (|| {
            let session = self.session.as_mut().expect("open session");
            for (i, (path, txn)) in paths.iter().copied().enumerate() {
                let reply = session.lock(path, txn, true);
                // A malformed grant may still have acquired a lock.
                locks[i] = LOCK_WRITE;
                native_script_grant(reply)?;
            }
            let current = native_script_node(session, &member_path)?.ok_or_else(|| {
                native_script_refusal(status::SCRIPT_MISSING, "The script was deleted.".into())
            })?;
            let revision = content_revision(&current);
            if revision != content_revision(base)
                || current.child(om_protocol::authoring::MOD_COUNT)
                    != base.child(om_protocol::authoring::MOD_COUNT)
            {
                return Err(AuthoringError::Refused {
                    status: status::CONFLICT,
                    message: "The script changed while the edit was being prepared.".into(),
                    current: Some(revision),
                });
            }
            let mut index = native_script_node(session, &index_path)?
                .filter(|index| index.child(&[0]).and_then(|node| node.value.as_ref()).is_some())
                .ok_or_else(|| AuthoringError::Unavailable("The script catalog lacks the serial key FileMaker Server requires when uploading its index.".into()))?;
            let mut member = edited.with_steps_in(&current);
            let revision = content_revision(&member);
            let stamp = Stamp {
                user: om_data::host::machine::user_name(),
                account: self.account_name.clone(),
                time: session
                    .host_timestamp()
                    .map_err(|e| AuthoringError::NotSent(e.to_string()))?,
            };
            script_catalog::stamp(&mut member, &stamp, false);
            script_catalog::stamp(&mut index, &stamp, false);
            if !member
                .to_wire()
                .is_some_and(|wire| wire.len() <= om_protocol::authoring::MAX_DEFINITION_BYTES)
                || index.to_wire().is_none()
            {
                return Err(invalid(
                    "The script or its index is too large to upload.".into(),
                ));
            }
            for (i, (path, txn)) in paths.iter().copied().enumerate() {
                native_script_grant(session.upgrade_lock(path, txn, LOCK_WRITE, LOCK_EXCLUSIVE))?;
                locks[i] = LOCK_EXCLUSIVE;
            }
            for (i, (node, (path, _))) in [&member, &index].into_iter().zip(paths).enumerate() {
                attention[i] = ATTENTION_CATALOG;
                session
                    .upload_to_host(path, node)
                    .map_err(|e| AuthoringError::OutcomeUnknown(e.to_string()))?;
                let read = native_script_node(session, path)
                    .map_err(|e| AuthoringError::OutcomeUnknown(e.to_string()))?;
                if read.as_ref() != Some(node) {
                    let what = if i == 0 { "script" } else { "script index" };
                    return Err(AuthoringError::OutcomeUnknown(format!(
                        "The host's read-back of the {what} differed from the upload."
                    )));
                }
            }
            Ok(om_protocol::authoring::Replaced {
                changed: true,
                revision,
            })
        })();
        let session = self.session.as_mut().expect("open session");
        let mut release_error = None;
        for (i, (path, txn)) in paths.into_iter().enumerate() {
            if locks[i] != 0 {
                if let Err(error) = session.unlock(path, txn, locks[i], attention[i]) {
                    release_error.get_or_insert(error);
                }
                if release_error.is_some() || result.is_err() {
                    // Also release a lock whose granted type could not be read.
                    let _ = session.kill_all_locks(txn);
                }
            }
        }
        result.and_then(|saved| match release_error {
            Some(error) => Err(AuthoringError::OutcomeUnknown(format!(
                "Both nodes were read back, but the host's lock release could not be confirmed ({error})."
            ))),
            None => Ok(saved),
        })
    }

    /// Save `edited`'s steps (`DataProvider::save_script`), when the host
    /// still stores the script at the revision the scripts were last read
    /// at, and read it back: the answer is the script as stored, whose
    /// revision the next save expects. Its name and kind are the catalog's,
    /// never saved here. A refusal, a conflict or a lost answer is an
    /// error, and the draft stays the caller's.
    pub(crate) fn save_edited_script(&mut self, edited: Script) -> Result<Script, DataError> {
        self.require_open()?;
        let Some(expected) = self.script_revisions.get(&edited.id).copied() else {
            return Err(DataError::new(
                "This script was not read on this connection: open the Script Workspace again.",
            ));
        };
        let stored = self.script_definitions()?;
        let (Some(definition), Some(current)) = (
            stored.definitions.get(&edited.id),
            stored.catalog.script(edited.id),
        ) else {
            return Err(DataError::new(format!(
                "The script “{}” is no longer on the host.",
                edited.name
            )));
        };
        if content_revision(definition) == expected
            && (current.name != edited.name || current.kind != edited.kind)
        {
            return Err(DataError::new(
                "Only a script's steps are saved here; its name is the host's.",
            ));
        }
        let definition = definition.clone();
        self.save_script_steps(&edited, &definition, expected)
            .map_err(|error| step_save_error(error, &edited.name))?;
        let stored = self.script_definitions().map_err(|error| {
            DataError::new(format!(
                "The host saved the script “{}”, but reading it back failed: {} Open the \
                 Script Workspace again to see it.",
                edited.name, error.message
            ))
        })?;
        let (Some(definition), Some(script)) = (
            stored.definitions.get(&edited.id),
            stored.catalog.script(edited.id),
        ) else {
            return Err(DataError::new(format!(
                "The host saved the script “{}”, but it is no longer there.",
                edited.name
            )));
        };
        self.script_revisions
            .insert(edited.id, content_revision(definition));
        Ok(script.clone())
    }

    /// Save the Script Workspace's catalog changes
    /// (`DataProvider::save_script_catalog`): see the module's doc. The
    /// answer is the catalog as the host stores it afterwards, a new
    /// member under the id the host's index gave it.
    pub(crate) fn save_catalog(
        &mut self,
        requested: ScriptCatalog,
    ) -> Result<ScriptCatalog, DataError> {
        self.require_open()?;
        let refused = |reason: &str| {
            DataError::with_code(
                UNSUPPORTED,
                format!("Scripts can be run and drafted here, not saved. {reason}"),
            )
        };
        if self.read_only() {
            return Err(refused("This file is open read-only."));
        }
        match self.script_saving() {
            Authoring::Available { .. } => {}
            Authoring::Denied(reason) | Authoring::Unavailable(reason) => {
                return Err(refused(&reason))
            }
        }
        // The save is confirmed after the uploads: a host that cannot
        // confirm one is refused before anything is sent.
        match self.save_confirming() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(refused(&reason))
            }
            Err(error) => return Err(error),
        }
        let Some(base) = self.script_edit_base else {
            return Err(DataError::new(
                "The scripts were not read for editing on this connection: open the Script \
                 Workspace again.",
            ));
        };
        let index = index_path();
        let index_txn = self.new_transaction();
        self.lock_script_path(&index, index_txn)
            .map_err(|e| DataError {
                message: format!("{} {UNCHANGED}", e.message),
                ..e
            })?;
        let mut member_locks = Vec::new();
        let outcome = self.send_catalog(&requested, base, index_txn, &mut member_locks);
        let session = self.session.as_mut().expect("open session");
        // Whatever the upload released already, nothing stays locked.
        for (path, txn) in member_locks.iter().chain([(index, index_txn)].iter()) {
            let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let _ = session.unlock(&refs, *txn, LOCK_WRITE, 0);
        }
        let save = match outcome {
            Ok(Some(save)) => save,
            Ok(None) => {
                let stored = self.script_definitions()?;
                return Ok(self.as_writable(stored.catalog));
            }
            Err(Failure::NotSent(error)) => return Err(error),
            Err(Failure::Partial(error)) => {
                // The workspace's catalog is no longer the host's.
                self.script_edit_base = None;
                return Err(error);
            }
        };
        // op13's reply says nothing about the disk: the host says whether
        // the file on disk holds the uploads.
        if let Err(error) = self.confirm_saved() {
            self.script_edit_base = None;
            return Err(DataError::new(format!(
                "The host took the scripts, but did not confirm that the file on disk holds \
                 them: {error} Open the Script Workspace again to see what it has."
            )));
        }
        let stored = self.script_definitions().map_err(|error| {
            self.script_edit_base = None;
            DataError::new(format!(
                "The host was sent the scripts, but reading them back failed: {} Open the \
                 Script Workspace again to see what it stored.",
                error.message
            ))
        })?;
        let gone = |segment: &Vec<u8>| {
            serial::decode(segment).is_some_and(|id| {
                stored.catalog.scripts.iter().all(|s| s.id != id)
                    && stored
                        .definitions
                        .get(&id)
                        .is_none_or(|node| node.children.is_empty() && node.value.is_none())
            })
        };
        let kept = save.members.iter().all(|member| {
            serial::decode(&member.segment)
                .and_then(|id| stored.definitions.get(&id))
                .is_some_and(|node| content_revision(node) == content_revision(member))
        }) && save.deleted.iter().all(gone)
            && stored
                .index
                .as_ref()
                .is_some_and(|index| catalog_revision(index) == catalog_revision(&save.index));
        if !kept {
            self.script_edit_base = None;
            return Err(DataError::new(
                "The host did not store the scripts as they were sent. Open the Script \
                 Workspace again to see what it has.",
            ));
        }
        self.script_revisions = stored
            .definitions
            .iter()
            .map(|(id, definition)| (*id, content_revision(definition)))
            .collect();
        self.script_edit_base = Some(index_revision(&stored));
        Ok(self.as_writable(stored.catalog))
    }

    /// Plan the save on the catalog as stored under the index's lock, and
    /// send it in FileMaker Pro's order (scripts.md, "Saving scripts to a
    /// host"): each member locked, then upgraded, uploaded and released
    /// with attention 2, then the index the same way. The members' locks
    /// are added to `member_locks` as they are taken.
    fn send_catalog(
        &mut self,
        requested: &ScriptCatalog,
        base: Revision,
        index_txn: u32,
        member_locks: &mut Vec<(Vec<Vec<u8>>, u32)>,
    ) -> Result<Option<CatalogSave>, Failure> {
        let not_sent = |e: DataError| {
            Failure::NotSent(DataError {
                message: format!("{} {UNCHANGED}", e.message),
                ..e
            })
        };
        let stored = self.script_definitions().map_err(not_sent)?;
        if index_revision(&stored) != base {
            return Err(Failure::NotSent(DataError::with_code(
                CHANGED_SINCE,
                format!(
                    "Another user changed the scripts since the Script Workspace read them. \
                     {UNCHANGED} Close the Script Workspace and open it again."
                ),
            )));
        }
        let time = self
            .session
            .as_mut()
            .expect("open session")
            .host_timestamp()
            .map_err(|e| {
                not_sent(DataError::new(format!(
                    "The host's clock could not be read: {e}."
                )))
            })?;
        let stamp = Stamp {
            user: om_data::host::machine::user_name(),
            account: self.account_name.clone(),
            time,
        };
        // A random (version 4) UUID, as every member UUID FileMaker wrote
        // in the starter files is.
        let mut failed = None;
        let mut uuid = || {
            let mut bytes = [0u8; 16];
            if let Err(e) = getrandom::getrandom(&mut bytes) {
                failed = Some(e);
            }
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            bytes
        };
        let planned = script_catalog::plan(
            stored.index.as_ref(),
            &stored.catalog,
            &stored.definitions,
            requested,
            &stamp,
            &mut uuid,
        );
        if let Some(e) = failed {
            return Err(not_sent(DataError::new(format!(
                "No random UUIDs could be made: {e}."
            ))));
        }
        let Some(save) = planned.map_err(Failure::NotSent)? else {
            return Ok(None);
        };
        // Each new or changed member uploaded whole, each deleted one
        // emptied, then the index.
        let mut uploads: Vec<NodeUpload<'_>> = Vec::new();
        for member in &save.members {
            let id = serial::decode(&member.segment).unwrap_or_default();
            uploads.push(NodeUpload {
                path: member_path(&member.segment),
                node: Some(member),
                what: format!("script {id}"),
            });
        }
        for segment in &save.deleted {
            let id = serial::decode(segment).unwrap_or_default();
            uploads.push(NodeUpload {
                path: member_path(segment),
                node: None,
                what: format!("the deletion of script {id}"),
            });
        }
        let mut txns = Vec::new();
        for upload in &uploads {
            let txn = self.new_transaction();
            self.lock_script_path(&upload.path, txn).map_err(not_sent)?;
            member_locks.push((upload.path.clone(), txn));
            txns.push(txn);
        }
        uploads.push(NodeUpload {
            path: index_path(),
            node: Some(&save.index),
            what: "the script index".to_owned(),
        });
        txns.push(index_txn);
        let mut sent: Vec<String> = Vec::new();
        for (upload, txn) in uploads.into_iter().zip(txns) {
            self.upload_script_node(&upload.path, upload.node, txn)
                .map_err(|(e, perhaps)| {
                    if perhaps {
                        sent.push(format!("perhaps {}", upload.what));
                    }
                    partial(&sent, e)
                })?;
            sent.push(upload.what);
        }
        Ok(Some(save))
    }

    /// Upgrade `txn`'s lock on `path`, upload `node` there whole (or empty
    /// the member, for `None`) and release the lock with attention 2. The
    /// error says whether the upload may have been stored.
    fn upload_script_node(
        &mut self,
        path: &[Vec<u8>],
        node: Option<&OwnedNode>,
        txn: u32,
    ) -> Result<(), (DataError, bool)> {
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let session = self.session.as_mut().expect("open session");
        match session.upgrade_lock(&refs, txn, LOCK_WRITE, LOCK_EXCLUSIVE) {
            Ok(LockReply::Granted { .. }) => {}
            Ok(other) => return Err((refused_lock(&other), false)),
            Err(e) => return Err((failure("upgrade a script lock", e), false)),
        }
        let uploaded = match node {
            Some(node) => session.upload_to_host(&refs, node),
            None => session.empty_on_host(&refs),
        };
        let _ = session.unlock(&refs, txn, LOCK_EXCLUSIVE, ATTENTION_CATALOG);
        uploaded.map_err(|e| (failure("upload the scripts", e), true))
    }

    /// Lock `path` for writing (type 9, with its modification count, as
    /// FileMaker Pro locks a script and the index): a lock held by another
    /// user is 301 naming them.
    fn lock_script_path(&mut self, path: &[Vec<u8>], txn: u32) -> Result<(), DataError> {
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let reply = self
            .session
            .as_mut()
            .expect("open session")
            .lock(&refs, txn, true)
            .map_err(|e| failure("lock the scripts", e))?;
        match reply {
            LockReply::Granted { .. } => Ok(()),
            other => Err(refused_lock(&other)),
        }
    }
}

fn native_script_node(
    session: &mut om_session::Session,
    path: &[&[u8]],
) -> Result<Option<OwnedNode>, AuthoringError> {
    let read_error = |e: DataError| AuthoringError::NotSent(e.message);
    let reply = session
        .download_from_host(path)
        .map_err(|e| AuthoringError::NotSent(e.to_string()))?;
    if !reply.value.is_empty() {
        return Err(AuthoringError::NotSent(
            "A script catalog node has an unexpected inline value.".into(),
        ));
    }
    let Some(download) =
        crate::node_from_reply(path.iter().map(|key| key.to_vec()).collect(), reply)
            .map_err(read_error)?
    else {
        return Ok(None);
    };
    let tree = crate::download_tree(std::slice::from_ref(&download)).map_err(read_error)?;
    tree.node(path)
        // The response starts inside the node. The path is implicit in
        // the slots, and its explicit node state was checked above.
        .map(|node| {
            Some(OwnedNode {
                push: true,
                ..node.to_owned_node()
            })
        })
        .ok_or_else(|| {
            AuthoringError::NotSent("The requested script catalog node was absent.".into())
        })
}

fn native_script_refusal(status: u16, message: String) -> AuthoringError {
    AuthoringError::Refused {
        status,
        message,
        current: None,
    }
}

fn native_script_grant(
    reply: Result<LockReply, om_session::SessionError>,
) -> Result<(), AuthoringError> {
    match reply.map_err(|e| AuthoringError::NotSent(e.to_string()))? {
        LockReply::Granted { .. } => Ok(()),
        reply @ (LockReply::MustWait
        | LockReply::Refused {
            status: om_protocol::write::LOCK_CONFLICT,
            ..
        }) => Err(native_script_refusal(
            status::IN_USE,
            refused_lock(&reply).message,
        )),
        LockReply::Refused { status, .. } => Err(native_script_refusal(
            status,
            format!("The host refused the script catalog lock (status {status})."),
        )),
    }
}

/// A lock the host did not grant (the caller says nothing was saved).
fn refused_lock(reply: &LockReply) -> DataError {
    let users = match reply {
        LockReply::Refused { conflict, .. } if !conflict.users.trim().is_empty() => {
            conflict.users.trim()
        }
        _ => "another user",
    };
    DataError::with_code(IN_USE, format!("The scripts are in use by {users}."))
}

/// A failure after `sent` went up: what the host may hold now.
fn partial(sent: &[String], error: DataError) -> Failure {
    if sent.is_empty() {
        return Failure::NotSent(DataError {
            message: format!("{} {UNCHANGED}", error.message),
            ..error
        });
    }
    Failure::Partial(DataError::new(format!(
        "The scripts were saved only in part ({} stored) before this failed: {} Close the \
         Script Workspace and open it again to see what the host has.",
        sent.join(", "),
        error.message
    )))
}

/// A step save's failure as the Script Workspace shows it.
fn step_save_error(error: AuthoringError, name: &str) -> DataError {
    DataError::new(match error {
        AuthoringError::Refused {
            status: status::CONFLICT,
            ..
        } => format!(
            "The script “{name}” was changed on the host since it was read, so these changes \
             were not saved. Close it without saving and open it again to see the host's \
             version."
        ),
        AuthoringError::Refused { message, .. } => message,
        AuthoringError::Unavailable(reason) | AuthoringError::NotSent(reason) => reason,
        AuthoringError::OutcomeUnknown(_) => {
            format!("{error} Open the Script Workspace again to see whether “{name}” was saved.")
        }
    })
}
