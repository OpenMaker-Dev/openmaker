//! OpenMaker's authoring extension, the host's side
//! (`om_protocol::authoring`, `docs/authoring.md`): replacing a script's
//! steps, and adding a field to a table, each as one catalog edit that is
//! refused whole or saved to disk before it is answered.
//!
//! The edit is FileMaker Pro's script save (scripts.md, "Saving scripts to
//! a host") done by the host: the script `[17, 5, S]` and the catalog index
//! `[17, 1]` are locked exclusively (a client that has the script open, as
//! FileMaker Pro's Script Workspace does, holds a write lock on it, so the
//! edit is refused while it does), the stored content is compared with the
//! revision the client edited, the new definition gets the modification
//! stamp (schema-path.md 8.1), and the other clients are told as the op62
//! releases would tell them (attention 2, schema-path.md 9).
//!
//! A new field is Manage Database's save of one field (schema-path.md 2.3,
//! 5.5) done by the host: under the locks FileMaker Pro holds for it
//! (`[08]` and the table lock `[T, 05]` for writing, the field catalog
//! `[T, 03]` exclusively, so an open Manage Database or a record being
//! edited in the table refuses it), the catalog's revision is compared,
//! the field is planned into the catalog as stored (`om_data::field_catalog`:
//! the next id, name lists, display order, count) with fresh UUIDs and
//! stamps, and the catalog is replaced and saved in one journaled commit.
//! A new field holds no values yet, so no record work is deferred.
//!
//! A field placed on a layout is Layout mode's save of the layout `[4, 5,
//! L]` (schema-path.md 2.3) done by the host: under an exclusive lock on
//! the layout (a client holding it, as one editing it would, refuses the
//! placement), the layout's revision is compared, the field must be one of
//! the layout's table's with a value Browse can edit, and the objects are
//! planned into the layout as stored (`om_data::layout_placement`) with
//! fresh UUIDs and the layout's stamp, then saved in one journaled commit.
//!
//! A new layout is the save FileMaker makes when a layout is added
//! (layout-model.md 10.6) done by the host: under the schema lock `[08]` for
//! writing and an exclusive lock on the layout catalog's index `[4, 1]`, the
//! index's revision is compared, the occurrence must be one of the file's,
//! and the layout is planned into the catalog as stored
//! (`om_data::layout_catalog`: the next id, the name list, the display
//! order, the count, the layout's entry and its member, with the theme and
//! width of the layout the client names) with fresh UUIDs and stamps, then
//! the member and the index are saved in one journaled commit.
//!
//! Objects moved or resized in Layout mode are the layout's save done by the
//! host, as a placement is: under an exclusive lock on the layout, its
//! revision is compared, and each object's frame is planned into the layout
//! as stored (`om_data::layout_objects`: its bounds in the object store and
//! its object index entry) with the layout's stamp, in one journaled commit.
//!
//! Only accounts in the file's verified built-in [Full Access] set may
//! author; every other account is refused.

use std::collections::HashSet;
use std::sync::Arc;

use log::{info, warn};
use om_data::field_catalog::{self, NewField, Refusal, ScalarType};
use om_data::layout_catalog::{self, NewLayout};
use om_data::layout_objects::{self, ObjectFrame};
use om_data::layout_placement::{self, Label, Placement, PortalPlacement};
use om_data::library_uses;
use om_hbam::catalog::DataType;
use om_hbam::slot::uint_segment;
use om_hbam::tree::memory;
use om_hbam::{serial, KeySegment, OwnedNode};
use om_protocol::authoring::{
    self, catalog_revision, content_revision, status, ChangeParts, CreateField, CreateLayout,
    Created, DeleteLayout, DeleteObjects, Deleted, Described, DuplicateLayout, FieldControl, Frame,
    Framed, LayoutCreated, LayoutSetup, LayoutWidth, Operation, PartOptions, PlaceButton,
    PlaceButtonStep, PlaceField, PlacePortal, PlaceText, Placed, ReplaceScriptSteps, Replaced,
    Reply, Request, Revision, SetObjectFrames, SetText, ValueListValues, MAX_DEFINITION_BYTES,
    MOD_ACCOUNT, MOD_COUNT, MOD_TIME, MOD_USER, OP_CHANGE_PARTS, OP_CREATE_FIELD, OP_CREATE_LAYOUT,
    OP_DELETE_LAYOUT, OP_DELETE_OBJECTS, OP_DESCRIBE_SECURITY, OP_DUPLICATE_LAYOUT,
    OP_FIELD_CONTROL, OP_LAYOUT_SETUP, OP_LAYOUT_WIDTH, OP_PART_OPTIONS, OP_PLACE_BUTTON,
    OP_PLACE_BUTTON_STEP, OP_PLACE_FIELD, OP_PLACE_PORTAL, OP_PLACE_TEXT, OP_REPLACE_SCRIPT_STEPS,
    OP_SAVE, OP_SET_OBJECT_FRAMES, OP_SET_TEXT, OP_VALUE_LIST_VALUES,
};
use om_protocol::PerformError;
use om_scripts::{Script, ScriptKind};

use crate::config::Privileges;
use crate::connection::{host_time_key, Shared};
use crate::hosting::{save_error, FileAccount, Hosted};
use crate::locks::LockUser;
use crate::store::Store;
use crate::transfer::Refused;
use crate::uploads::Charge;
use crate::writes::{self, Change};

/// The script catalog `[17]`: its index `01`, the definitions under `05`,
/// and in a definition the step list `04` and step data `05`.
const SCRIPTS: u8 = 17;
const INDEX: u8 = 1;
const DEFINITIONS: u8 = 5;
const STEP_LIST: u8 = 4;
const STEP_DATA: u8 = 5;

/// A table's fields `[T, 03]` and records `[T, 05]`, and the path Manage
/// Database locks while it is open, `[08]` (schema-path.md 4).
const FIELDS: u8 = 3;
const RECORDS: u8 = 5;
const SCHEMA_LOCK: u8 = 8;
/// The layout catalog `[4]` and its members `[4, 5]`, and a layout's
/// object attributes `[4, 5, L, 05]` (layout-model.md 5.5).
const LAYOUTS: u8 = 4;
const LAYOUT_MEMBERS: u8 = 5;
/// The value-list catalog's members `[21, 5]`.
const VALUE_LIST_MEMBERS: u8 = 5;
const OBJECT_ATTRIBUTES: u8 = 5;
/// The theme catalog `[6]` and its members `[6, 5, T]` (layout-model.md
/// 4.1).
const THEMES: u8 = 6;
const THEME_MEMBERS: u8 = 5;
/// The value list catalog `[21]`, its members under `[21, 5]`.
const VALUE_LISTS: u8 = 0x21;
/// A catalog member's UUIDs (schema-path.md 5.2).
const UNIQUE_ID: u8 = 0xd8;
const SOURCE_ID: u8 = 0xd9;

/// The write lock Manage Database takes on `[08]` and `[T, 05]`, and the
/// exclusive lock FileMaker Pro upgrades a catalog to while it saves it
/// (schema-path.md 4).
const LOCK_WRITE: u16 = 9;
const LOCK_EXCLUSIVE: u16 = 10;

/// The attention code of a released catalog (`kDBAttnCatalog`,
/// schema-path.md 9).
const ATTENTION_CATALOG: u32 = 2;

/// The transaction the host's locks for an edit are taken under, apart
/// from any of the client's own (OpenMaker's choice).
const AUTHORING_TXN: u32 = u32::MAX;

/// Who sent the request.
pub(crate) struct Caller<'a> {
    /// The file the request names, if this session opened it.
    pub file: Option<&'a Arc<Hosted>>,
    /// The file's account the session logged in as, as it was at the
    /// Login (FileMaker keeps a session's privileges until it logs in
    /// again).
    pub account: Option<&'a FileAccount>,
    pub connection: u32,
    /// The client's session name: the holder others see while the edit
    /// holds its locks, and the stamp's user name.
    pub session_name: &'a str,
}

/// Answer an authoring request. The caller is checked for what the
/// request's operation needs before its parameters are read, so a caller
/// that may not send it is refused without its definition being read. Then
/// `reserve` reserves memory for the request (charged to the server's
/// budget for clients' uploads, `crate::uploads`), a new definition is read
/// into its tree within it, and past it the request is refused before
/// anything is applied.
pub(crate) fn perform(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    txn: &[u8],
    reserve: impl FnOnce() -> Charge,
) -> Result<Vec<u8>, PerformError> {
    let authorization = match authoring::request_operation(txn)? {
        Operation::Unknown { version, operation } => {
            return Ok(authoring::build_reply(&unsupported(version, operation)))
        }
        Operation::Describe => authorized(caller, false),
        Operation::DescribeSecurity => file_full_access(caller),
        Operation::ReplaceScriptSteps
        | Operation::CreateField
        | Operation::PlaceField
        | Operation::CreateLayout
        | Operation::PlaceButton
        | Operation::PlaceButtonStep
        | Operation::SetObjectFrames
        | Operation::DeleteObjects
        | Operation::PlaceText
        | Operation::DuplicateLayout
        | Operation::DeleteLayout
        | Operation::LayoutSetup
        | Operation::LayoutWidth
        | Operation::FieldControl
        | Operation::PartOptions
        | Operation::ChangeParts
        | Operation::SetText
        | Operation::ValueListValues
        | Operation::PlacePortal
        | Operation::Save => authorized(caller, true),
    };
    let (file, signed_in) = match authorization {
        Ok(found) => found,
        Err(reply) => return Ok(authoring::build_reply(&reply)),
    };
    let account = signed_in.name.as_str();
    let mut reading = reserve();
    let Some(request) = authoring::parse_request_within(txn, reading.bytes())? else {
        warn!("{peer}: refusing an authoring request past what the client may hold");
        return Err(Refused.into());
    };
    // The definition's tree stays charged while the request is served.
    reading.shrink_to(match &request {
        Request::ReplaceScriptSteps(replace) => memory(&replace.definition),
        Request::PlaceButtonStep(place) => place.data.as_ref().map_or(0, memory),
        _ => 0,
    });
    let reply = match request {
        Request::Unknown { version, operation } => unsupported(version, operation),
        Request::Describe => Reply::Described(Described {
            operations: OP_REPLACE_SCRIPT_STEPS
                | OP_CREATE_FIELD
                | OP_PLACE_FIELD
                | OP_PLACE_BUTTON
                | OP_PLACE_BUTTON_STEP
                | OP_DESCRIBE_SECURITY
                | OP_CREATE_LAYOUT
                | OP_SET_OBJECT_FRAMES
                | OP_DELETE_OBJECTS
                | OP_PLACE_TEXT
                | OP_DUPLICATE_LAYOUT
                | OP_DELETE_LAYOUT
                | OP_LAYOUT_SETUP
                | OP_LAYOUT_WIDTH
                | OP_FIELD_CONTROL
                | OP_PART_OPTIONS
                | OP_CHANGE_PARTS
                | OP_SET_TEXT
                | OP_VALUE_LIST_VALUES
                | OP_PLACE_PORTAL
                | OP_SAVE,
            can_author: signed_in.privileges() == Privileges::FullAccess,
            max_definition_bytes: MAX_DEFINITION_BYTES as u32,
        }),
        Request::Save => save(peer, file),
        Request::DescribeSecurity => {
            let catalog = crate::security::catalog(file);
            // Counts only: names and keys stay out of the log.
            info!(
                "{peer}: security catalog of {:?}: {} accounts, {} privilege sets, {} extended \
                 privileges",
                file.name,
                catalog.accounts.len(),
                catalog.privilege_sets.len(),
                catalog.extended_privileges.len()
            );
            let reply = authoring::build_reply(&Reply::Security(catalog));
            if reply.len() > MAX_DEFINITION_BYTES {
                warn!(
                    "{peer}: refusing a security catalog of {} bytes",
                    reply.len()
                );
                return Ok(authoring::build_reply(&refused(
                    status::UNSUPPORTED,
                    format!(
                        "The file's security catalog is {} KB; one reply carries at most {} KB.",
                        reply.len().div_ceil(1024),
                        MAX_DEFINITION_BYTES / 1024
                    ),
                )));
            }
            return Ok(reply);
        }
        Request::ReplaceScriptSteps(request) => {
            // What decoding the definition takes is charged before it
            // runs, or the request is refused.
            if !reading.grow(decoding_cost(&request.definition)) {
                warn!("{peer}: refusing a script definition past what the client may hold");
                return Err(Refused.into());
            }
            replace_script_steps(shared, peer, caller, file, account, &request)
        }
        Request::CreateField(request) => {
            create_field(shared, peer, caller, file, account, &request)
        }
        Request::PlaceField(request) => place_field(shared, peer, caller, file, account, &request),
        Request::PlaceText(request) => place_text(shared, peer, caller, file, account, &request),
        Request::PlacePortal(request) => {
            place_portal(shared, peer, caller, file, account, &request)
        }
        Request::CreateLayout(request) => {
            create_layout(shared, peer, caller, file, account, &request)
        }
        Request::DuplicateLayout(request) => {
            duplicate_layout(shared, peer, caller, file, account, &request)
        }
        Request::DeleteLayout(request) => {
            delete_layout(shared, peer, caller, file, account, &request)
        }
        Request::LayoutSetup(request) => {
            layout_setup(shared, peer, caller, file, account, &request)
        }
        Request::LayoutWidth(request) => {
            layout_width(shared, peer, caller, file, account, &request)
        }
        Request::FieldControl(request) => {
            field_control(shared, peer, caller, file, account, &request)
        }
        Request::PartOptions(request) => {
            part_options(shared, peer, caller, file, account, &request)
        }
        Request::ChangeParts(request) => {
            change_parts(shared, peer, caller, file, account, &request)
        }
        Request::SetText(request) => set_text(shared, peer, caller, file, account, &request),
        Request::ValueListValues(request) => {
            value_list_values(shared, peer, caller, file, account, &request)
        }
        Request::PlaceButton(request) => {
            place_button(shared, peer, caller, file, account, &request)
        }
        Request::PlaceButtonStep(request) => {
            place_button_step(shared, peer, caller, file, account, &request)
        }
        Request::SetObjectFrames(request) => {
            set_object_frames(shared, peer, caller, file, account, &request)
        }
        Request::DeleteObjects(request) => {
            delete_objects(shared, peer, caller, file, account, &request)
        }
    };
    Ok(authoring::build_reply(&reply))
}

fn unsupported(version: u16, operation: u8) -> Reply {
    refused(
        status::UNSUPPORTED,
        format!(
            "This OpenMaker Server does not implement authoring version {version}, \
             operation {operation}."
        ),
    )
}

fn refused(status: u16, message: impl Into<String>) -> Reply {
    Reply::Refused {
        status,
        message: message.into(),
        current: None,
    }
}

/// The caller's file and account when the account is the file's own and in
/// the file's verified built-in [Full Access] set (docs/re/local-files.md
/// 2.3), as reading the file's security needs: the rule the server applies
/// to downloads of credential paths.
fn file_full_access<'a>(caller: &Caller<'a>) -> Result<(&'a Arc<Hosted>, &'a FileAccount), Reply> {
    let (file, account) = authorized(caller, false)?;
    if account.full_access() {
        Ok((file, account))
    } else {
        Err(refused(
            status::PERMISSION_DENIED,
            "Viewing the file's security needs an account of the file in its built-in \
             [Full Access] privilege set.",
        ))
    }
}

/// The caller's file and account, when it is logged in and, if
/// `to_author`, may change scripts.
fn authorized<'a>(
    caller: &Caller<'a>,
    to_author: bool,
) -> Result<(&'a Arc<Hosted>, &'a FileAccount), Reply> {
    let file = caller.file.ok_or_else(|| {
        refused(
            status::FILE_MISSING,
            "The file is not open in this session.",
        )
    })?;
    let account = caller
        .account
        .ok_or_else(|| refused(status::PERMISSION_DENIED, "Log in to the file first."))?;
    if to_author && account.privileges() != Privileges::FullAccess {
        return Err(refused(
            status::PERMISSION_DENIED,
            format!(
                "The account “{}” may not change scripts or the schema: only accounts in the \
                 file's [Full Access] privilege set may.",
                account.name
            ),
        ));
    }
    Ok((file, account))
}

/// A definition node decoded as a script, read through a borrowed view of
/// it rather than encoded to slots and read into a second tree.
fn decode(node: &OwnedNode) -> Result<Script, String> {
    Script::decode(&crate::store::view(node)).map_err(|e| e.0)
}

/// The memory decoding `definition` as a script ([`decode`]) takes beside
/// the definition itself: the view [`decode`] reads it through (less than
/// the definition's own [`memory`]), the children `Script::decode` keeps as
/// owned copies (at most that again), the assembled step list and a step
/// for each of its descriptors, and the step data each step names twice
/// (an owned copy and what `StepData::decode` keeps of it), once however
/// many steps name it, as `Script::decode` shares it (#149). Children are
/// found as `Script::decode` finds them. The map of data children it reads
/// the steps through (a few words a child) is dropped before it copies the
/// other children, so it fits in what those copies are charged.
fn decoding_cost(definition: &OwnedNode) -> usize {
    let tree = memory(definition);
    let mut cost = tree.saturating_mul(2);
    fn by_id(node: &OwnedNode, id: u64) -> Option<&OwnedNode> {
        node.children
            .iter()
            .find(|c| KeySegment(&c.segment).as_uint() == Some(id))
    }
    let Some(list) = by_id(definition, u64::from(STEP_LIST)).and_then(OwnedNode::assembled) else {
        return cost;
    };
    let data = by_id(definition, u64::from(STEP_DATA));
    // A data node many steps name is decoded, and charged, once.
    let mut measured: HashSet<&[u8]> = HashSet::new();
    cost = cost.saturating_add(list.len());
    for descriptor in list.chunks_exact(28) {
        let Ok(descriptor) = <&[u8; 28]>::try_from(descriptor) else {
            continue;
        };
        let step = om_scripts::ScriptStep::from_descriptor(descriptor);
        cost = cost.saturating_add(std::mem::size_of::<om_scripts::ScriptStep>());
        // The data children are in key order with no key twice (the reader
        // refuses anything else), so a binary search finds what
        // `Script::decode`'s scan finds.
        let named = step
            .data_key
            .segment()
            .and_then(|segment| data?.child(segment));
        if let Some(node) = named.filter(|node| measured.insert(node.segment.as_slice())) {
            cost = cost.saturating_add(memory(node).saturating_mul(2));
        }
    }
    cost
}

/// The first child, other than the steps and the stamp, that `new` does
/// not hold as `old` does.
fn other_change(old: &OwnedNode, new: &OwnedNode) -> Option<Vec<u8>> {
    let kept = |node: &OwnedNode| -> Vec<OwnedNode> {
        node.children
            .iter()
            .filter(|c| {
                !matches!(c.segment.as_slice(), [STEP_LIST] | [STEP_DATA])
                    && !authoring::is_stamp(&c.segment)
            })
            .cloned()
            .collect()
    };
    let (old, new) = (kept(old), kept(new));
    let differs = |a: &[OwnedNode], b: &[OwnedNode]| {
        a.iter().find(|c| !b.contains(c)).map(|c| c.segment.clone())
    };
    differs(&new, &old).or_else(|| differs(&old, &new))
}

/// The modification count of `member`'s stamp (0 without one).
fn stamp_count(member: &OwnedNode) -> u64 {
    member
        .child(MOD_COUNT)
        .and_then(|n| n.value.as_deref())
        .and_then(|v| serial::decode(v.get(1..)?))
        .unwrap_or(0)
}

/// `member` with the modification stamp of a save by `user` and `account`
/// at host time `time` (schema-path.md 8.1): the count `fc` raised by one
/// as a serial key (none counts as 0; a count at the largest stays, a
/// hostile file's, never a panic), the user name, the account, and the
/// date-time key `time` as a key string. That key's time part is a number
/// key as long as the seconds' digits, so the key is 8 bytes only from
/// 02:46:40 (10,000 seconds) on and shorter before.
pub(crate) fn stamped(mut member: OwnedNode, user: &str, account: &str, time: &[u8]) -> OwnedNode {
    let key = serial::encode(stamp_count(&member).saturating_add(1));
    let mut count = vec![key.len() as u8];
    count.extend(key);
    let mut when = vec![time.len() as u8];
    when.extend(time);
    member.set_child(OwnedNode::leaf(MOD_COUNT.to_vec(), count));
    member.set_child(om_scripts::text_node(MOD_USER, user));
    member.set_child(om_scripts::text_node(MOD_ACCOUNT, account));
    member.set_child(OwnedNode::leaf(MOD_TIME.to_vec(), when));
    member
}

fn replace_script_steps(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &ReplaceScriptSteps,
) -> Reply {
    let id = request.script;
    let key = serial::encode(id);
    if request.definition.segment != key {
        return refused(
            status::INVALID,
            format!("The definition sent is not script {id}'s."),
        );
    }
    match decode(&request.definition) {
        Ok(script) if script.kind == ScriptKind::Script => {
            if let Err(e) = script.check_structure() {
                return refused(status::INVALID, format!("The new steps do not nest: {e}"));
            }
        }
        Ok(_) => {
            return refused(
                status::INVALID,
                "The definition sent is a folder or separator, not a script.",
            )
        }
        Err(e) => {
            return refused(
                status::INVALID,
                format!("The definition sent is not a script: {e}"),
            )
        }
    }
    let definition_path = vec![vec![SCRIPTS], vec![DEFINITIONS], key.clone()];
    let index_path = vec![vec![SCRIPTS], vec![INDEX]];
    let mut store = hosted.write_store();
    let Some(current) = store.node(&[&[SCRIPTS], &[DEFINITIONS], &key]).cloned() else {
        return refused(
            status::SCRIPT_MISSING,
            format!("The file has no script {id}."),
        );
    };
    if !matches!(decode(&current), Ok(s) if s.kind == ScriptKind::Script) {
        return refused(
            status::INVALID,
            format!("Member {id} of the script catalog is not a script."),
        );
    }
    if let Some(segment) = other_change(&current, &request.definition) {
        return refused(
            status::INVALID,
            format!(
                "The definition sent changes key {segment:02x?} of script {id}; only its steps \
                 may change."
            ),
        );
    }
    let revision = content_revision(&request.definition);
    let current_revision = content_revision(&current);
    if current_revision == revision {
        return Reply::Replaced(Replaced {
            changed: false,
            revision,
        });
    }
    if current_revision != request.expected {
        return Reply::Refused {
            status: status::CONFLICT,
            message: format!("Script {id} was changed since the revision the edit was made to."),
            current: Some(current_revision),
        };
    }
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    {
        let mut locks = hosted.lock_table();
        let held = match locks.lock(&definition_path, user, LOCK_EXCLUSIVE, &holder) {
            Ok(()) => match locks.lock(&index_path, user, LOCK_EXCLUSIVE, &holder) {
                Ok(()) => Ok(()),
                Err(conflict) => {
                    locks.unlock(&definition_path, user, LOCK_EXCLUSIVE);
                    Err(conflict)
                }
            },
            Err(conflict) => Err(conflict),
        };
        if let Err(conflict) = held {
            return refused(
                status::IN_USE,
                format!(
                    "Script {id} or the script catalog is in use by {}.",
                    conflict.users
                ),
            );
        }
    }
    let time = host_time_key();
    let mut members = vec![(
        definition_path.clone(),
        stamped(
            request.definition.clone(),
            caller.session_name,
            account,
            &time,
        ),
    )];
    // FileMaker Pro saves the index with the same stamp; a file without
    // one (scripts listed by their definitions only) does not get a
    // partial one.
    let index = store.node(&[&[SCRIPTS], &[INDEX]]).cloned();
    if let Some(index) = &index {
        members.push((
            index_path.clone(),
            stamped(index.clone(), caller.session_name, account, &time),
        ));
    }
    let mut journal = store.begin_commit();
    store.replace_members_in(&mut journal, members);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    {
        let mut locks = hosted.lock_table();
        locks.unlock(&definition_path, user, LOCK_EXCLUSIVE);
        locks.unlock(&index_path, user, LOCK_EXCLUSIVE);
    }
    if let Err(e) = saved {
        warn!("{peer}: {:?}: script {id} not saved: {e}", hosted.name);
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); script {id} is unchanged."),
        );
    }
    let mut changes = vec![Change {
        path: definition_path,
        code: ATTENTION_CATALOG,
    }];
    if index.is_some() {
        changes.push(Change {
            path: index_path,
            code: ATTENTION_CATALOG,
        });
    }
    writes::notify(hosted, &shared.mailboxes, caller.connection, &changes);
    info!(
        "{peer}: {:?}: script {id}'s steps replaced by {account}",
        hosted.name
    );
    Reply::Replaced(Replaced {
        changed: true,
        revision,
    })
}

/// Write the file to disk now if it has changes the disk lacks (the
/// client's op13 uploads, whose save is only logged when it fails) and
/// answer whether the disk holds them all: a failed save keeps them in
/// memory, for the next save.
fn save(peer: &str, file: &Hosted) -> Reply {
    if file.read_only {
        return refused(
            status::UNSUPPORTED,
            "The file is open read-only: nothing is written to it.",
        );
    }
    file.save();
    if file.write_store().has_unsaved_changes() {
        warn!(
            "{peer}: {:?}: a save the client asked for failed",
            file.name
        );
        return refused(
            status::SAVE_FAILED[0],
            "The file could not be written to disk: its changes are kept in the host's memory \
             until a save succeeds.",
        );
    }
    Reply::Saved
}

/// Take `locks` (path, type) in order for `user`, or none of them: the
/// holders of the first that conflicts.
fn lock_all(
    hosted: &Hosted,
    user: LockUser,
    holder: &str,
    locks: &[(Vec<Vec<u8>>, u16)],
) -> Result<(), String> {
    let mut table = hosted.lock_table();
    for (n, (path, lock_type)) in locks.iter().enumerate() {
        if let Err(conflict) = table.lock(path, user, *lock_type, holder) {
            for (path, lock_type) in &locks[..n] {
                table.unlock(path, user, *lock_type);
            }
            return Err(conflict.users);
        }
    }
    Ok(())
}

fn unlock_all(hosted: &Hosted, user: LockUser, locks: &[(Vec<Vec<u8>>, u16)]) {
    let mut table = hosted.lock_table();
    for (path, lock_type) in locks {
        table.unlock(path, user, *lock_type);
    }
}

fn create_field(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &CreateField,
) -> Reply {
    let Some(data_type) = ScalarType::from_code(request.data_type) else {
        return refused(
            status::INVALID,
            format!(
                "Fields of data type {} cannot be created; the types are text (1), number (2), \
                 date (3), time (4) and timestamp (5).",
                request.data_type
            ),
        );
    };
    let field = NewField {
        name: request.name.clone(),
        data_type,
        comment: Some(request.comment.clone()).filter(|c| !c.is_empty()),
    };
    if let Err(refusal) = field_catalog::check_name(&field.name) {
        return refused(status::NAME_UNUSABLE, refusal.to_string());
    }
    let table = uint_segment(request.table);
    let catalog_path = vec![table.clone(), vec![FIELDS]];
    let mut store = hosted.write_store();
    if !store.has_table(&table) {
        return refused(
            status::TABLE_MISSING,
            format!("The file has no table {}.", request.table),
        );
    }
    let current = store.node(&[&table, &[FIELDS]]).cloned();
    let current_revision = catalog_revision(
        current
            .as_ref()
            .unwrap_or(&OwnedNode::list(vec![FIELDS], Vec::new())),
    );
    let conflict = || Reply::Refused {
        status: status::CONFLICT,
        message: format!(
            "The fields of table {} were changed since the revision the request was made to.",
            request.table
        ),
        current: Some(current_revision),
    };
    if let Some((id, member)) = current
        .as_ref()
        .and_then(|catalog| field_catalog::same_name(catalog, &field.name))
    {
        // Made by an earlier request whose answer was lost: nothing to do.
        if field_catalog::is_made_by(member, &field) && current_revision != request.expected {
            return Reply::Created(Created {
                changed: false,
                field: id,
                revision: current_revision,
            });
        }
    }
    if current_revision != request.expected {
        return conflict();
    }
    let planned = match field_catalog::plan(current.as_ref(), &field) {
        Ok(planned) => planned,
        Err(refusal) => {
            let code = match refusal {
                Refusal::Name(_) => status::NAME_UNUSABLE,
                Refusal::Exists { .. } => status::NAME_EXISTS,
                Refusal::Comment(_) => status::INVALID,
                Refusal::Unsupported(_) => status::UNSUPPORTED,
            };
            return refused(code, refusal.to_string());
        }
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [
        (vec![vec![SCHEMA_LOCK]], LOCK_WRITE),
        (vec![table.clone(), vec![RECORDS]], LOCK_WRITE),
        (catalog_path.clone(), LOCK_EXCLUSIVE),
    ];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::SCHEMA_IN_USE,
            format!(
                "The schema or a record of table {} is in use by {users}.",
                request.table
            ),
        );
    }
    // The new member's UUIDs and who made it when (no `fc`: it is new,
    // schema-path.md 8.1); the index's stamp, counted up.
    let time = host_time_key();
    let mut catalog = planned.catalog;
    let member_path: [&[u8]; 2] = [&[5], planned.key.as_slice()];
    if let Some(member) = catalog.at_mut(&member_path) {
        member.set_child(OwnedNode::leaf(
            vec![UNIQUE_ID],
            shared.random::<16>().to_vec(),
        ));
        member.set_child(OwnedNode::leaf(
            vec![SOURCE_ID],
            shared.random::<16>().to_vec(),
        ));
        let mut stamped = stamped(member.clone(), caller.session_name, account, &time);
        stamped.children.retain(|c| c.segment != MOD_COUNT);
        *member = stamped;
    }
    if let Some(index) = catalog.child(&[1]).cloned() {
        catalog.set_child(stamped(index, caller.session_name, account, &time));
    }
    let revision = catalog_revision(&catalog);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, catalog_path.clone(), catalog);
    let saved = hosted.save_commit(&mut store, journal);
    if saved.is_ok() {
        store.add_field_basis(&table, planned.field);
    }
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: field {:?} not added to table {}: {e}",
            hosted.name, field.name, request.table
        );
        return refused(
            save_error(&e),
            format!(
                "The file could not be saved ({e}); the fields of table {} are unchanged.",
                request.table
            ),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: catalog_path,
            code: ATTENTION_CATALOG,
        }],
    );
    info!(
        "{peer}: {:?}: field {:?} ({}) added to table {} as {} by {account}",
        hosted.name,
        field.name,
        data_type.name(),
        request.table,
        planned.field
    );
    Reply::Created(Created {
        changed: true,
        field: planned.field,
        revision,
    })
}

fn create_layout(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &CreateLayout,
) -> Reply {
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            "The file is open read-only; no layout was added.",
        );
    }
    if let Err(refusal) = layout_catalog::check_name(&request.name) {
        return refused(status::NAME_UNUSABLE, refusal.to_string());
    }
    let index_path = vec![vec![LAYOUTS], vec![INDEX]];
    let mut store = hosted.write_store();
    if !store
        .catalog()
        .table_occurrences
        .iter()
        .any(|o| o.id == request.occurrence)
    {
        return refused(
            status::TABLE_MISSING,
            format!("The file has no table occurrence {}.", request.occurrence),
        );
    }
    let like = (request.like != 0).then_some(request.like);
    let planned = {
        let Some(layouts) = store.node(&[&[LAYOUTS]]) else {
            return refused(
                status::UNSUPPORTED,
                "The file has no layout catalog [4] to add a layout to.",
            );
        };
        if let Some(like) = like {
            if layouts
                .at(&[&[LAYOUT_MEMBERS], &uint_segment(like)])
                .is_none()
            {
                return refused(
                    status::LAYOUT_MISSING,
                    format!("The file has no layout {like}."),
                );
            }
        }
        let current_revision = catalog_revision(
            layouts
                .child(&[INDEX])
                .unwrap_or(&OwnedNode::list(vec![INDEX], Vec::new())),
        );
        let Some((theme, width)) = layout_catalog::model(layouts, like) else {
            return refused(
                status::UNSUPPORTED,
                "No layout of the file has a theme for the new layout to take.",
            );
        };
        let layout = NewLayout {
            name: request.name.clone(),
            occurrence: request.occurrence,
            theme,
            width,
        };
        // Made by an earlier sending of this request, whose answer was
        // lost: its UUID is on the layout it made. Nothing to do.
        if let Some(made) = layout_catalog::members(layouts).iter().find(|m| {
            m.child(&[UNIQUE_ID]).and_then(|n| n.value.as_deref()) == Some(&request.uuid[..])
        }) {
            return Reply::LayoutCreated(LayoutCreated {
                changed: false,
                layout: om_hbam::slot::be_uint(&made.segment).unwrap_or(0),
                revision: current_revision,
            });
        }
        if current_revision != request.expected {
            return Reply::Refused {
                status: status::CONFLICT,
                message: "The file's layouts were changed since the revision the request was \
                          made to."
                    .into(),
                current: Some(current_revision),
            };
        }
        match layout_catalog::plan(layouts, &layout) {
            Ok(planned) => planned,
            Err(refusal) => {
                let code = match refusal {
                    layout_catalog::Refusal::Name(_) => status::NAME_UNUSABLE,
                    layout_catalog::Refusal::Unsupported(_) => status::UNSUPPORTED,
                };
                return refused(code, refusal.to_string());
            }
        }
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [
        (vec![vec![SCHEMA_LOCK]], LOCK_WRITE),
        (index_path.clone(), LOCK_EXCLUSIVE),
    ];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::SCHEMA_IN_USE,
            format!("The file's layouts are in use by {users}."),
        );
    }
    // The new member's UUIDs, the first the request's, and who made it
    // when (no `fc`: it is new, schema-path.md 8.1); the index's stamp,
    // counted up.
    let time = host_time_key();
    let mut member = planned.member;
    member.set_child(OwnedNode::leaf(vec![UNIQUE_ID], request.uuid.to_vec()));
    member.set_child(OwnedNode::leaf(
        vec![SOURCE_ID],
        shared.random::<16>().to_vec(),
    ));
    let mut member = stamped(member, caller.session_name, account, &time);
    member.children.retain(|c| c.segment != MOD_COUNT);
    let index = stamped(planned.index, caller.session_name, account, &time);
    let revision = catalog_revision(&index);
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], planned.key.clone()];
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path, member);
    store.replace_schema_in(&mut journal, index_path, index);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: layout {:?} not added: {e}",
            hosted.name, request.name
        );
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); no layout was added."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: vec![vec![LAYOUTS]],
            code: ATTENTION_CATALOG,
        }],
    );
    info!(
        "{peer}: {:?}: layout {:?} added as {} on occurrence {} by {account}",
        hosted.name, request.name, planned.layout, request.occurrence
    );
    Reply::LayoutCreated(LayoutCreated {
        changed: true,
        layout: planned.layout,
        revision,
    })
}

/// Set a layout's width: its member's definition planned with
/// `layout_placement::plan_width` and saved as an object edit is
/// ([`edit_objects`]), the member stamped.
fn layout_width(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &LayoutWidth,
) -> Reply {
    let width = request.width;
    let edit = ObjectEdit {
        layout: request.layout,
        expected: request.expected,
        objects: 0,
        done: "set to its new width",
        already: |member: &OwnedNode, _: &Store| layout_placement::already_width(member, width),
        plan: |member: &OwnedNode, _: &Store| {
            layout_placement::plan_width(member, width).map_err(placement_refused)
        },
    };
    match edit_objects(shared, peer, caller, hosted, account, edit) {
        Ok((changed, revision)) => Reply::Framed(Framed { changed, revision }),
        Err(reply) => reply,
    }
}

/// Operation 18: show a field object with another control. What the file
/// says of the object's field and of the value list it will show is read
/// first, for the refusal's status (at the request's revision only: at
/// another, the edit is already applied or a conflict), and again under the
/// store guard the plan is made and saved under (`layout_objects::plan_control`,
/// [`edit_objects`]), so a schema change in between cannot slip past; the
/// member is stamped.
fn field_control(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &FieldControl,
) -> Reply {
    let id = request.layout;
    {
        let store = hosted.write_store();
        let Some(member) = store.node(&[&[LAYOUTS], &[LAYOUT_MEMBERS], &uint_segment(id)]) else {
            return refused(
                status::LAYOUT_MISSING,
                format!("The file has no layout {id}."),
            );
        };
        // At another revision, `edit_objects` answers: already applied, or
        // a conflict (306), even when the object is gone since.
        if catalog_revision(member) == request.expected {
            if let Err((status, why)) = control_facts(&store, member, request) {
                return refused(status, why);
            }
        }
    }
    let (object, control, value_list) = (request.object, request.control, request.value_list);
    let edit = ObjectEdit {
        layout: id,
        expected: request.expected,
        objects: 1,
        done: "shown with its new control",
        already: |member: &OwnedNode, _: &Store| {
            layout_objects::already_control(member, object, control, value_list)
        },
        plan: |member: &OwnedNode, store: &Store| {
            let facts =
                control_facts(store, member, request).map_err(|(code, why)| refused(code, why))?;
            layout_objects::plan_control(member, object, (control, value_list), &facts)
                .map_err(placement_refused)
        },
    };
    match edit_objects(shared, peer, caller, hosted, account, edit) {
        Ok((changed, revision)) => Reply::Framed(Framed { changed, revision }),
        Err(reply) => reply,
    }
}

/// Operation 20: a text object's words, planned with
/// `layout_placement::plan_words` and saved as an object edit is
/// ([`edit_objects`]).
fn set_text(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &SetText,
) -> Reply {
    let (object, text) = (request.object, request.text.as_str());
    let edit = ObjectEdit {
        layout: request.layout,
        expected: request.expected,
        objects: 1,
        done: "given its new words",
        already: |member: &OwnedNode, _: &Store| {
            layout_placement::already_words(member, object, text)
        },
        plan: |member: &OwnedNode, _: &Store| {
            layout_placement::plan_words(member, object, text).map_err(placement_refused)
        },
    };
    match edit_objects(shared, peer, caller, hosted, account, edit) {
        Ok((changed, revision)) => Reply::Framed(Framed { changed, revision }),
        Err(reply) => reply,
    }
}

/// Operation 19: Part Setup's options for one part of a layout, planned
/// with `layout_placement::plan_part_options` and saved as an object edit
/// Add, delete or resize a layout's part (#227, operation 22), planned with
/// `layout_placement::plan_parts` and saved as an object edit is
/// ([`edit_objects`]).
fn change_parts(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &ChangeParts,
) -> Reply {
    use layout_placement::PartChange;
    let part_type = match i32::try_from(request.part_type) {
        Ok(t) => t,
        Err(_) => return refused(status::INVALID, "No part has that type."),
    };
    let change = match request.action {
        authoring::PARTS_ADD => PartChange::Add {
            part_type,
            size: request.size,
        },
        authoring::PARTS_DELETE => PartChange::Delete {
            part: request.part,
            part_type,
        },
        authoring::PARTS_RESIZE => PartChange::Resize {
            part: request.part,
            part_type,
            size: request.size,
        },
        other => {
            return refused(
                status::INVALID,
                format!("{other} is not a change of parts (1 add, 2 delete, 3 resize)."),
            )
        }
    };
    let edit = ObjectEdit {
        layout: request.layout,
        expected: request.expected,
        objects: 0,
        done: "given its parts",
        already: |member: &OwnedNode, _: &Store| layout_placement::already_parts(member, &change),
        plan: |member: &OwnedNode, _: &Store| {
            layout_placement::plan_parts(member, &change).map_err(placement_refused)
        },
    };
    match edit_objects(shared, peer, caller, hosted, account, edit) {
        Ok((changed, revision)) => Reply::Framed(Framed { changed, revision }),
        Err(reply) => reply,
    }
}

/// is ([`edit_objects`]). A sub-summary's break field is looked up under
/// the store guard the plan is saved under: the layout's own occurrence,
/// still on the table the client chose the field of, which has the field.
fn part_options(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &PartOptions,
) -> Reply {
    let field = request
        .break_field
        .map(|(occurrence, _, field)| (occurrence, field));
    let (part, options) = (
        request.part,
        (request.flags, request.page_break_after, field),
    );
    // The sort field the client chose, as the store has it now: the
    // layout's own occurrence, still on that table, which has the field.
    // Checked under the store guard, for the retry as for the save.
    let field_holds = |member: &OwnedNode, store: &Store| {
        let catalog = store.catalog();
        let own = layout_placement::definition(member)
            .ok()
            .and_then(|def| om_layout::occurrence_id(def.table_alias_key));
        request
            .break_field
            .is_some_and(|(occurrence, table, field)| {
                own == Some(occurrence)
                    && (catalog.table_occurrences.iter())
                        .find(|o| o.id == occurrence)
                        .and_then(|o| o.base_table())
                        .filter(|base| *base == table)
                        .and_then(|base| catalog.table(base))
                        .is_some_and(|table| table.field(field).is_some())
            })
    };
    let edit = ObjectEdit {
        layout: request.layout,
        expected: request.expected,
        objects: 0,
        done: "given its part's options",
        already: |member: &OwnedNode, store: &Store| {
            layout_placement::already_part_options(member, part, options)
                && (request.break_field.is_none() || field_holds(member, store))
        },
        plan: |member: &OwnedNode, store: &Store| {
            let exists = field_holds(member, store);
            layout_placement::plan_part_options(member, part, options, exists)
                .map_err(placement_refused)
        },
    };
    match edit_objects(shared, peer, caller, hosted, account, edit) {
        Ok((changed, revision)) => Reply::Framed(Framed { changed, revision }),
        Err(reply) => reply,
    }
}

/// What `store` says of field object `request.object` of `member` (its
/// layout's member): its field's spec and type, and whether the value list
/// it will show (the one asked for, or else the one it stores) exists. Or
/// the refusal's status and text.
fn control_facts(
    store: &Store,
    member: &OwnedNode,
    request: &FieldControl,
) -> Result<layout_objects::ControlFacts, (u16, String)> {
    let object = layout_placement::stored_objects(member)
        .ok()
        .and_then(|(_, objects)| objects.objects.into_iter().find(|o| o.id == request.object))
        .and_then(|o| o.field_obj);
    let Some((spec, stored_list)) =
        object.and_then(|field| Some((field.field_spec?, field.value_list_key)))
    else {
        return Err((
            status::INVALID,
            format!(
                "Layout {} has no field object {}.",
                request.layout, request.object
            ),
        ));
    };
    let catalog = store.catalog();
    let field = om_layout::occurrence_id(spec.table_key)
        .and_then(|occurrence| {
            catalog
                .table_occurrences
                .iter()
                .find(|o| o.id == occurrence)?
                .base_table()
        })
        .and_then(|table| catalog.table(table))
        .and_then(|table| table.field(spec.field_id()?));
    let Some(field) = field else {
        return Err((
            status::FIELD_MISSING,
            format!("Object {} shows no field of this file.", request.object),
        ));
    };
    let list = request.value_list.unwrap_or(stored_list);
    let value_list_exists = list == 0
        || serial::raw_of_id(list).is_some_and(|raw| {
            store
                .node(&[&[VALUE_LISTS], &[5], &uint_segment(raw)])
                .is_some()
        });
    Ok(layout_objects::ControlFacts {
        field: (spec.table_key, spec.field_key),
        date: field.data_type == DataType::Date,
        container: field.data_type == DataType::Container,
        value_list_exists,
    })
}

fn set_object_frames(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &SetObjectFrames,
) -> Reply {
    let frames: Vec<ObjectFrame> = request
        .frames
        .iter()
        .map(|(object, frame)| ObjectFrame {
            object: *object,
            frame: rect(frame),
        })
        .collect();
    let edit = ObjectEdit {
        layout: request.layout,
        expected: request.expected,
        objects: frames.len(),
        done: "moved or resized",
        already: |member: &OwnedNode, _: &Store| layout_objects::already_framed(member, &frames),
        plan: |member: &OwnedNode, _: &Store| {
            layout_objects::plan(member, &frames).map_err(placement_refused)
        },
    };
    match edit_objects(shared, peer, caller, hosted, account, edit) {
        Ok((changed, revision)) => Reply::Framed(Framed { changed, revision }),
        Err(reply) => reply,
    }
}

fn delete_objects(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &DeleteObjects,
) -> Reply {
    let edit = ObjectEdit {
        layout: request.layout,
        expected: request.expected,
        objects: request.objects.len(),
        done: "deleted",
        already: |member: &OwnedNode, _: &Store| {
            layout_objects::already_deleted(member, &request.objects)
        },
        plan: |member: &OwnedNode, _: &Store| {
            layout_objects::plan_deletion(member, &request.objects).map_err(placement_refused)
        },
    };
    match edit_objects(shared, peer, caller, hosted, account, edit) {
        Ok((changed, revision)) => Reply::Deleted(Deleted { changed, revision }),
        Err(reply) => reply,
    }
}

/// An edit of a layout's objects: the layout, the revision it was made
/// against, how many objects it changes and what it does to them (for the
/// log, "deleted"), whether a layout holds it already (`already`), and its
/// plan into a layout member as stored (`plan`).
struct ObjectEdit<A, P> {
    layout: u64,
    expected: Revision,
    objects: usize,
    done: &'static str,
    already: A,
    plan: P,
}

/// Plan an edit of layout `edit.layout`'s objects into its member as stored
/// and save it behind an exclusive lock on the member: whether it changed
/// the layout (false when the layout was not at the expected revision and
/// already held the edit, made by an earlier request) and the layout's
/// revision, or the refusal to answer. The retry check and the plan run
/// under the store guard the edit is saved under, and are given the store,
/// so what they read of the file holds; the plan answers a refusal with its
/// own status.
fn edit_objects<A, P>(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    edit: ObjectEdit<A, P>,
) -> Result<(bool, Revision), Reply>
where
    A: Fn(&OwnedNode, &Store) -> bool,
    P: Fn(&OwnedNode, &Store) -> Result<layout_objects::Planned, Reply>,
{
    let id = edit.layout;
    if hosted.read_only {
        return Err(refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is unchanged."),
        ));
    }
    let key = uint_segment(id);
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], key.clone()];
    let mut store = hosted.write_store();
    let Some(current) = store.node(&[&[LAYOUTS], &[LAYOUT_MEMBERS], &key]).cloned() else {
        return Err(refused(
            status::LAYOUT_MISSING,
            format!("The file has no layout {id}."),
        ));
    };
    let current_revision = catalog_revision(&current);
    if current_revision != edit.expected {
        if (edit.already)(&current, &store) {
            return Ok((false, current_revision));
        }
        return Err(Reply::Refused {
            status: status::CONFLICT,
            message: format!("Layout {id} was changed since the revision the request was made to."),
            current: Some(current_revision),
        });
    }
    let planned = (edit.plan)(&current, &store)?;
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [(member_path.clone(), LOCK_EXCLUSIVE)];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return Err(refused(
            status::LAYOUT_IN_USE,
            format!("Layout {id} is in use by {users}."),
        ));
    }
    let time = host_time_key();
    let member = stamped(planned.member, caller.session_name, account, &time);
    let revision = catalog_revision(&member);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path.clone(), member);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: objects of layout {id} not {}: {e}",
            hosted.name, edit.done
        );
        return Err(refused(
            save_error(&e),
            format!("The file could not be saved ({e}); layout {id} is unchanged."),
        ));
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: member_path,
            code: ATTENTION_CATALOG,
        }],
    );
    info!(
        "{peer}: {:?}: {} of layout {id} {} by {account}",
        hosted.name,
        match edit.objects {
            0 => "the definition".to_owned(),
            n => format!("{n} objects"),
        },
        edit.done
    );
    Ok((true, revision))
}

/// Set a layout's name, views and menu inclusion as Layout Setup does:
/// planned with `layout_catalog::plan_setup` and saved, the member and the
/// index stamped as FileMaker Pro's Layout Setup stamps them
/// (layout-model.md 10.11), in one commit under the catalog's write lock
/// and the index's and the member's exclusive locks.
fn layout_setup(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &LayoutSetup,
) -> Reply {
    let id = request.layout;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is unchanged."),
        );
    }
    let index_path = vec![vec![LAYOUTS], vec![INDEX]];
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], uint_segment(id)];
    let mut store = hosted.write_store();
    let planned = {
        let Some(layouts) = store.node(&[&[LAYOUTS]]) else {
            return refused(status::UNSUPPORTED, "The file has no layout catalog [4].");
        };
        let Some(member) = layouts.at(&[&[LAYOUT_MEMBERS], &uint_segment(id)]) else {
            return refused(
                status::LAYOUT_MISSING,
                format!("The file has no layout {id}."),
            );
        };
        let current = catalog_revision(member);
        let planned =
            layout_catalog::plan_setup(layouts, id, &request.name, request.flags, request.hidden);
        // A layout that holds the setup already answers it unchanged: an
        // earlier sending's, whose answer was lost, or the same values.
        if planned.as_ref().is_ok_and(|p| !p.changed) {
            return Reply::Framed(Framed {
                changed: false,
                revision: current,
            });
        }
        if current != request.expected {
            return Reply::Refused {
                status: status::CONFLICT,
                message: format!(
                    "Layout {id} was changed since the revision the request was made to."
                ),
                current: Some(current),
            };
        }
        match planned {
            Ok(planned) => planned,
            Err(refusal) => {
                let code = match refusal {
                    layout_catalog::Refusal::Name(_) => status::NAME_UNUSABLE,
                    layout_catalog::Refusal::Unsupported(_) => status::UNSUPPORTED,
                };
                return refused(code, refusal.to_string());
            }
        }
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [
        (vec![vec![SCHEMA_LOCK]], LOCK_WRITE),
        (index_path.clone(), LOCK_EXCLUSIVE),
        (member_path.clone(), LOCK_EXCLUSIVE),
    ];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::LAYOUT_IN_USE,
            format!("Layout {id} or the file's layouts are in use by {users}."),
        );
    }
    // Both stamps before the store changes; a count that cannot be raised
    // (only a damaged or hostile file has one) refuses with nothing written.
    if [&planned.member, &planned.index]
        .iter()
        .any(|n| stamp_count(n) == u64::MAX)
    {
        unlock_all(hosted, user, &locks);
        return refused(
            status::UNSUPPORTED,
            format!("Layout {id}'s or the layouts' modification count cannot be raised."),
        );
    }
    let time = host_time_key();
    let member = stamped(planned.member, caller.session_name, account, &time);
    let index = stamped(planned.index, caller.session_name, account, &time);
    let revision = catalog_revision(&member);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path, member);
    store.replace_schema_in(&mut journal, index_path, index);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!("{peer}: {:?}: layout {id} not set up: {e}", hosted.name);
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); layout {id} is unchanged."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: vec![vec![LAYOUTS]],
            code: ATTENTION_CATALOG,
        }],
    );
    info!("{peer}: {:?}: layout {id} set up by {account}", hosted.name);
    Reply::Framed(Framed {
        changed: true,
        revision,
    })
}

/// Edit Value List (#869): custom list `request.list`'s values, planned by
/// `om_data::value_lists::plan_values` from the catalog as stored, saved
/// behind the schema's write lock and the index's and the member's
/// exclusive locks, with both stamps, as FileMaker Pro 26.0.3 writes them
/// (file-metadata.md 6.5).
fn value_list_values(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &ValueListValues,
) -> Reply {
    use om_data::value_lists::{self, ValuesRefusal};
    let id = request.list;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; value list {id} is unchanged."),
        );
    }
    let index_path = vec![vec![VALUE_LISTS], vec![INDEX]];
    let member_path = vec![
        vec![VALUE_LISTS],
        vec![VALUE_LIST_MEMBERS],
        uint_segment(id),
    ];
    let mut store = hosted.write_store();
    let planned = {
        let Some(catalog) = store.node(&[&[VALUE_LISTS]]) else {
            return refused(
                status::VALUE_LIST_MISSING,
                "The file has no value-list catalog [21].",
            );
        };
        let planned = match value_lists::plan_values(catalog, id, &request.values) {
            Ok(planned) => planned,
            Err(refusal) => {
                let code = match refusal {
                    ValuesRefusal::Missing(_) => status::VALUE_LIST_MISSING,
                    ValuesRefusal::Unsupported(_) => status::UNSUPPORTED,
                    ValuesRefusal::Invalid(_) => status::INVALID,
                };
                return refused(code, refusal.to_string());
            }
        };
        let current = catalog_revision(&planned.member);
        // A list that holds the values already answers unchanged: an
        // earlier sending's, whose answer was lost, or the same values.
        if !planned.changed {
            return Reply::Framed(Framed {
                changed: false,
                revision: current,
            });
        }
        let stored = catalog
            .at(&[&[VALUE_LIST_MEMBERS], &uint_segment(id)])
            .map(catalog_revision);
        if stored != Some(request.expected) {
            return Reply::Refused {
                status: status::CONFLICT,
                message: format!(
                    "Value list {id} was changed since the revision the request was made to."
                ),
                current: stored,
            };
        }
        planned
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [
        (vec![vec![SCHEMA_LOCK]], LOCK_WRITE),
        (index_path.clone(), LOCK_EXCLUSIVE),
        (member_path.clone(), LOCK_EXCLUSIVE),
    ];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::IN_USE,
            format!("Value list {id} or the file's value lists are in use by {users}."),
        );
    }
    if [&planned.member, &planned.index]
        .iter()
        .any(|n| stamp_count(n) == u64::MAX)
    {
        unlock_all(hosted, user, &locks);
        return refused(
            status::UNSUPPORTED,
            format!("Value list {id}'s or the value lists' modification count cannot be raised."),
        );
    }
    let time = host_time_key();
    let member = stamped(planned.member, caller.session_name, account, &time);
    let index = stamped(planned.index, caller.session_name, account, &time);
    let revision = catalog_revision(&member);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path, member);
    store.replace_schema_in(&mut journal, index_path, index);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: value list {id} not changed: {e}",
            hosted.name
        );
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); value list {id} is unchanged."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: vec![vec![VALUE_LISTS]],
            code: ATTENTION_CATALOG,
        }],
    );
    info!(
        "{peer}: {:?}: value list {id} changed by {account}",
        hosted.name
    );
    Reply::Framed(Framed {
        changed: true,
        revision,
    })
}

fn rect(frame: &Frame) -> om_layout::Rect {
    om_layout::Rect {
        top: frame[0],
        left: frame[1],
        bottom: frame[2],
        right: frame[3],
        other: Vec::new(),
    }
}

fn place_field(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &PlaceField,
) -> Reply {
    let id = request.layout;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is unchanged."),
        );
    }
    let placement = Placement {
        occurrence: request.occurrence,
        field: request.field,
        frame: rect(&request.frame),
        label: request.label.as_ref().map(|(text, frame)| Label {
            text: text.clone(),
            frame: rect(frame),
        }),
    };
    if let Some(label) = &placement.label {
        if let Err(refusal) = layout_placement::check_label(&label.text) {
            return refused(status::INVALID, refusal.to_string());
        }
    }
    let key = uint_segment(id);
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], key.clone()];
    let mut store = hosted.write_store();
    let Some(current) = store.node(&[&[LAYOUTS], &[LAYOUT_MEMBERS], &key]).cloned() else {
        return refused(
            status::LAYOUT_MISSING,
            format!("The file has no layout {id}."),
        );
    };
    let current_revision = catalog_revision(&current);
    // The field: one of the layout's occurrence's table with a value
    // Browse edits as text in an edit box.
    let catalog = store.catalog();
    let table_id = catalog
        .table_occurrences
        .iter()
        .find(|o| o.id == request.occurrence)
        .and_then(|o| o.base_table());
    if table_id != Some(request.table) {
        return Reply::Refused {
            status: status::CONFLICT,
            message: format!(
                "Occurrence {} no longer shows the requested table {}. Read the layout again.",
                request.occurrence, request.table
            ),
            current: Some(current_revision),
        };
    }
    let table = table_id.and_then(|table| catalog.table(table));
    let Some(field) = table.and_then(|t| t.field(request.field)) else {
        return refused(
            status::FIELD_MISSING,
            format!(
                "Occurrence {} shows no table of this file with a field {}.",
                request.occurrence, request.field
            ),
        );
    };
    if matches!(field.data_type, DataType::Container | DataType::Unknown(_)) {
        return refused(
            status::INVALID,
            format!(
                "Field {} (“{}”) is not text, a number, a date, a time or a timestamp; this \
                 version places those in edit boxes only.",
                request.field, field.name
            ),
        );
    }
    if current_revision != request.expected {
        // Placed by an earlier request whose answer was lost: nothing to do.
        if let Some((field_object, label_object)) =
            layout_placement::already_placed(&current, &placement)
        {
            return Reply::Placed(Placed {
                changed: false,
                field_object,
                label_object: label_object.unwrap_or(0),
                revision: current_revision,
            });
        }
        return Reply::Refused {
            status: status::CONFLICT,
            message: format!("Layout {id} was changed since the revision the request was made to."),
            current: Some(current_revision),
        };
    }
    let planned = match layout_placement::plan(&current, &placement) {
        Ok(planned) => planned,
        Err(refusal) => {
            let code = match refusal {
                layout_placement::Refusal::Invalid(_) => status::INVALID,
                layout_placement::Refusal::Unsupported(_) => status::UNSUPPORTED,
            };
            return refused(code, refusal.to_string());
        }
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [(member_path.clone(), LOCK_EXCLUSIVE)];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::LAYOUT_IN_USE,
            format!("Layout {id} is in use by {users}."),
        );
    }
    // Each new object's UUIDs, and the layout's stamp.
    let time = host_time_key();
    let mut member = planned.member;
    for object in std::iter::once(planned.field_object).chain(planned.label_object) {
        if let Some(record) = member.at_mut(&[&[OBJECT_ATTRIBUTES], &serial::encode(object)]) {
            record.set_child(OwnedNode::leaf(
                vec![UNIQUE_ID],
                shared.random::<16>().to_vec(),
            ));
            record.set_child(OwnedNode::leaf(
                vec![SOURCE_ID],
                shared.random::<16>().to_vec(),
            ));
        }
    }
    let member = stamped(member, caller.session_name, account, &time);
    let revision = catalog_revision(&member);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path.clone(), member);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: field {} not placed on layout {id}: {e}",
            hosted.name, request.field
        );
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); layout {id} is unchanged."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: member_path,
            code: ATTENTION_CATALOG,
        }],
    );
    info!(
        "{peer}: {:?}: field {} placed on layout {id} as object {}{} by {account}",
        hosted.name,
        request.field,
        planned.field_object,
        planned
            .label_object
            .map(|label| format!(" with label object {label}"))
            .unwrap_or_default()
    );
    Reply::Placed(Placed {
        changed: true,
        field_object: planned.field_object,
        label_object: planned.label_object.unwrap_or(0),
        revision,
    })
}

/// The file library `[31]`: its members `[31, 5, N]` and a member's use
/// count `12` (layout-model.md 10.8).
const LIBRARY: u8 = 0x1f;
const LIBRARY_MEMBERS: u8 = 0x05;
const LIBRARY_USES: u8 = 0x12;
/// A layout's stored table view `[4, 5, L, 0d]` (file-metadata.md 7.1).
const TABLE_VIEW: u8 = 0x0d;

/// Copy a layout as FileMaker Pro's Duplicate Layout does: the copy's
/// member and index entries planned with `layout_catalog::plan_duplicate`,
/// the library items its objects show counted up, all stamped and saved in
/// one commit under the layouts' and the items' locks, or refused with
/// nothing written.
fn duplicate_layout(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &DuplicateLayout,
) -> Reply {
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            "The file is open read-only; no layout was added.",
        );
    }
    let index_path = vec![vec![LAYOUTS], vec![INDEX]];
    let mut store = hosted.write_store();
    let (duplicated, items) = {
        let Some(layouts) = store.node(&[&[LAYOUTS]]) else {
            return refused(
                status::UNSUPPORTED,
                "The file has no layout catalog [4] to copy a layout in.",
            );
        };
        let current_revision = catalog_revision(
            layouts
                .child(&[INDEX])
                .unwrap_or(&OwnedNode::list(vec![INDEX], Vec::new())),
        );
        // Made by an earlier sending of this request, whose answer was
        // lost: its UUID is on the copy. Nothing to do.
        if let Some(made) = layout_catalog::members(layouts).iter().find(|m| {
            m.child(&[UNIQUE_ID]).and_then(|n| n.value.as_deref()) == Some(&request.uuid[..])
        }) {
            return Reply::LayoutCreated(LayoutCreated {
                changed: false,
                layout: om_hbam::slot::be_uint(&made.segment).unwrap_or(0),
                revision: current_revision,
            });
        }
        if layouts
            .at(&[&[LAYOUT_MEMBERS], &uint_segment(request.layout)])
            .is_none()
        {
            return refused(
                status::LAYOUT_MISSING,
                format!("The file has no layout {}.", request.layout),
            );
        }
        if current_revision != request.expected {
            return Reply::Refused {
                status: status::CONFLICT,
                message: "The file's layouts were changed since the revision the request was \
                          made to."
                    .into(),
                current: Some(current_revision),
            };
        }
        let duplicated = match layout_catalog::plan_duplicate(layouts, request.layout) {
            Ok(duplicated) => duplicated,
            Err(refusal) => {
                let code = match refusal {
                    layout_catalog::Refusal::Name(_) => status::NAME_UNUSABLE,
                    layout_catalog::Refusal::Unsupported(_) => status::UNSUPPORTED,
                };
                return refused(code, refusal.to_string());
            }
        };
        // Each library item the copy shows: its use count raised by its new
        // uses, and its stamp count `fc` by as many, as FileMaker Pro's copy
        // raised them, keeping its user and time (layout-model.md 10.9).
        // Both are worked out, with overflow checked, before anything is
        // written.
        let mut items = Vec::with_capacity(duplicated.library.len());
        for (&item, &uses) in &duplicated.library {
            let key = serial::encode(item);
            let raised = store
                .node(&[&[LIBRARY], &[LIBRARY_MEMBERS], &key])
                .and_then(|member| {
                    let count = member.child(&[LIBRARY_USES])?.value.as_deref()?;
                    let count = u32::from_be_bytes(count.try_into().ok()?).checked_add(uses)?;
                    let stamp = match member.child(MOD_COUNT) {
                        Some(node) => serial::decode(node.value.as_deref()?.get(1..)?)?,
                        None => 0,
                    };
                    let stamp = serial::encode(stamp.checked_add(u64::from(uses))?);
                    let mut stamp_value = vec![u8::try_from(stamp.len()).ok()?];
                    stamp_value.extend(stamp);
                    let mut member = member.clone();
                    member.set_child(OwnedNode::leaf(
                        vec![LIBRARY_USES],
                        count.to_be_bytes().to_vec(),
                    ));
                    member.set_child(OwnedNode::leaf(MOD_COUNT.to_vec(), stamp_value));
                    Some(member)
                });
            let Some(member) = raised else {
                return refused(
                    status::UNSUPPORTED,
                    format!(
                        "The layout shows library item {item}, whose use count OpenMaker cannot \
                         read; no layout was added."
                    ),
                );
            };
            items.push((vec![vec![LIBRARY], vec![LIBRARY_MEMBERS], key], member));
        }
        (duplicated, items)
    };
    let planned = duplicated.planned;
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let mut locks = vec![
        (vec![vec![SCHEMA_LOCK]], LOCK_WRITE),
        (index_path.clone(), LOCK_EXCLUSIVE),
    ];
    locks.extend(
        items
            .iter()
            .map(|(path, ..)| (path.clone(), LOCK_EXCLUSIVE)),
    );
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::SCHEMA_IN_USE,
            format!("The file's layouts are in use by {users}."),
        );
    }
    // The copy's UUID, the request's; a new UUID for each object (they
    // keep their source UUIDs `d9`, as FileMaker Pro's copy does); its stamp
    // keeps the source's count, as FileMaker Pro's copy did, with who
    // copied it when. The index and each library item are counted up.
    let time = host_time_key();
    let mut member = planned.member;
    member.set_child(OwnedNode::leaf(vec![UNIQUE_ID], request.uuid.to_vec()));
    // The layout's objects, and those of its stored table view `0d`
    // (file-metadata.md 7.1), whose records may hold UUIDs too.
    for path in [
        vec![vec![OBJECT_ATTRIBUTES]],
        vec![vec![TABLE_VIEW], vec![OBJECT_ATTRIBUTES]],
    ] {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let Some(records) = member.at(&segments) else {
            continue;
        };
        let mut records = records.clone();
        for record in &mut records.children {
            if record.child(&[UNIQUE_ID]).is_some() {
                record.set_child(OwnedNode::leaf(
                    vec![UNIQUE_ID],
                    shared.random::<16>().to_vec(),
                ));
            }
        }
        member.put(&segments, records);
    }
    let count = member.child(MOD_COUNT).cloned();
    let mut member = stamped(member, caller.session_name, account, &time);
    match count {
        Some(count) => member.set_child(count),
        None => member.children.retain(|c| c.segment != MOD_COUNT),
    }
    let index = stamped(planned.index, caller.session_name, account, &time);
    let revision = catalog_revision(&index);
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], planned.key.clone()];
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path, member);
    store.replace_schema_in(&mut journal, index_path, index);
    for (path, item) in items {
        store.replace_schema_in(&mut journal, path, item);
    }
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: layout {} not copied: {e}",
            hosted.name, request.layout
        );
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); no layout was added."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[
            Change {
                path: vec![vec![LAYOUTS]],
                code: ATTENTION_CATALOG,
            },
            Change {
                path: vec![vec![LIBRARY]],
                code: ATTENTION_CATALOG,
            },
        ],
    );
    info!(
        "{peer}: {:?}: layout {} copied as {} by {account}",
        hosted.name, request.layout, planned.layout
    );
    Reply::LayoutCreated(LayoutCreated {
        changed: true,
        layout: planned.layout,
        revision,
    })
}

/// Delete a layout as FileMaker Pro's Delete Layout does: its member and
/// index entries planned with `layout_catalog::plan_delete`, the library
/// uses its objects release with `library_uses::release`, all stamped and
/// saved in one commit under the layouts', the member's and the items'
/// locks, or refused with nothing written.
fn delete_layout(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &DeleteLayout,
) -> Reply {
    let id = request.layout;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is kept."),
        );
    }
    let index_path = vec![vec![LAYOUTS], vec![INDEX]];
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], uint_segment(id)];
    let mut store = hosted.write_store();
    let (planned, released) = {
        let Some(layouts) = store.node(&[&[LAYOUTS]]) else {
            return refused(
                status::UNSUPPORTED,
                "The file has no layout catalog [4] to delete a layout from.",
            );
        };
        let current_revision = catalog_revision(
            layouts
                .child(&[INDEX])
                .unwrap_or(&OwnedNode::list(vec![INDEX], Vec::new())),
        );
        let exists = layouts
            .at(&[&[LAYOUT_MEMBERS], &uint_segment(id)])
            .is_some();
        if current_revision != request.expected {
            // Deleted by an earlier sending whose answer was lost: layout
            // ids are not issued again, so a missing one stays deleted.
            if !exists {
                return Reply::Deleted(Deleted {
                    changed: false,
                    revision: current_revision,
                });
            }
            return Reply::Refused {
                status: status::CONFLICT,
                message: "The file's layouts were changed since the revision the request was \
                          made to."
                    .into(),
                current: Some(current_revision),
            };
        }
        if !exists {
            return refused(
                status::LAYOUT_MISSING,
                format!("The file has no layout {id}."),
            );
        }
        let planned = match layout_catalog::plan_delete(layouts, id) {
            Ok(planned) => planned,
            Err(refusal) => return refused(status::UNSUPPORTED, refusal.to_string()),
        };
        let released = if planned.library.is_empty() {
            None
        } else {
            let Some(library) = store.node(&[&[LIBRARY]]) else {
                return refused(
                    status::UNSUPPORTED,
                    "The layout shows library items, but the file has no library [31].",
                );
            };
            match library_uses::release(library, &planned.library) {
                Ok(released) => Some(released),
                Err(refusal) => return refused(status::UNSUPPORTED, refusal.to_string()),
            }
        };
        (planned, released)
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let mut locks = vec![
        (vec![vec![SCHEMA_LOCK]], LOCK_WRITE),
        (index_path.clone(), LOCK_EXCLUSIVE),
        (member_path.clone(), LOCK_EXCLUSIVE),
    ];
    let library_index = vec![vec![LIBRARY], vec![INDEX]];
    if let Some(released) = &released {
        locks.extend(released.members.iter().map(|(key, _)| {
            (
                vec![vec![LIBRARY], vec![LIBRARY_MEMBERS], key.clone()],
                LOCK_EXCLUSIVE,
            )
        }));
        if released.index.is_some() {
            locks.push((library_index.clone(), LOCK_EXCLUSIVE));
        }
    }
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::LAYOUT_IN_USE,
            format!("Layout {id} or the file's layouts are in use by {users}."),
        );
    }
    let time = host_time_key();
    let index = stamped(planned.index, caller.session_name, account, &time);
    let revision = catalog_revision(&index);
    let mut journal = store.begin_commit();
    store.remove_schema_in(&mut journal, member_path);
    store.replace_schema_in(&mut journal, index_path, index);
    if let Some(released) = released {
        for (key, member) in released.members {
            let path = vec![vec![LIBRARY], vec![LIBRARY_MEMBERS], key];
            match member {
                Some(member) => store.replace_schema_in(&mut journal, path, member),
                None => store.remove_schema_in(&mut journal, path),
            }
        }
        if let Some(library) = released.index {
            let library = stamped(library, caller.session_name, account, &time);
            store.replace_schema_in(&mut journal, library_index, library);
        }
    }
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!("{peer}: {:?}: layout {id} not deleted: {e}", hosted.name);
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); layout {id} is kept."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[
            Change {
                path: vec![vec![LAYOUTS]],
                code: ATTENTION_CATALOG,
            },
            Change {
                path: vec![vec![LIBRARY]],
                code: ATTENTION_CATALOG,
            },
        ],
    );
    info!(
        "{peer}: {:?}: layout {id} deleted by {account}",
        hosted.name
    );
    Reply::Deleted(Deleted {
        changed: true,
        revision,
    })
}

/// Place a text on a layout's body on its own, as the Text tool does and
/// as [`place_field`] places a field's label: checked, planned
/// (`layout_placement::plan_text`), given its UUIDs, stamped and saved
/// under the layout's lock, or refused with nothing written.
fn place_text(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &PlaceText,
) -> Reply {
    let id = request.layout;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is unchanged."),
        );
    }
    let text = Label {
        text: request.text.clone(),
        frame: rect(&request.frame),
    };
    if let Err(refusal) = layout_placement::check_label(&text.text) {
        return refused(status::INVALID, refusal.to_string());
    }
    let key = uint_segment(id);
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], key.clone()];
    let mut store = hosted.write_store();
    let Some(current) = store.node(&[&[LAYOUTS], &[LAYOUT_MEMBERS], &key]).cloned() else {
        return refused(
            status::LAYOUT_MISSING,
            format!("The file has no layout {id}."),
        );
    };
    let current_revision = catalog_revision(&current);
    if current_revision != request.expected {
        // Placed by an earlier request whose answer was lost: nothing to do.
        if let Some(text_object) = layout_placement::already_placed_text(&current, &text) {
            return Reply::Placed(Placed {
                changed: false,
                field_object: text_object,
                label_object: 0,
                revision: current_revision,
            });
        }
        return Reply::Refused {
            status: status::CONFLICT,
            message: format!("Layout {id} was changed since the revision the request was made to."),
            current: Some(current_revision),
        };
    }
    let planned = match layout_placement::plan_text(&current, &text) {
        Ok(planned) => planned,
        Err(refusal) => {
            let code = match refusal {
                layout_placement::Refusal::Invalid(_) => status::INVALID,
                layout_placement::Refusal::Unsupported(_) => status::UNSUPPORTED,
            };
            return refused(code, refusal.to_string());
        }
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [(member_path.clone(), LOCK_EXCLUSIVE)];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::LAYOUT_IN_USE,
            format!("Layout {id} is in use by {users}."),
        );
    }
    // The text's UUIDs, and the layout's stamp.
    let time = host_time_key();
    let mut member = planned.member;
    if let Some(record) =
        member.at_mut(&[&[OBJECT_ATTRIBUTES], &serial::encode(planned.text_object)])
    {
        record.set_child(OwnedNode::leaf(
            vec![UNIQUE_ID],
            shared.random::<16>().to_vec(),
        ));
        record.set_child(OwnedNode::leaf(
            vec![SOURCE_ID],
            shared.random::<16>().to_vec(),
        ));
    }
    let member = stamped(member, caller.session_name, account, &time);
    let revision = catalog_revision(&member);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path.clone(), member);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: text not placed on layout {id}: {e}",
            hosted.name
        );
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); layout {id} is unchanged."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: member_path,
            code: ATTENTION_CATALOG,
        }],
    );
    // The text itself stays out of the log.
    info!(
        "{peer}: {:?}: a text placed on layout {id} as object {} by {account}",
        hosted.name, planned.text_object
    );
    Reply::Placed(Placed {
        changed: true,
        field_object: planned.text_object,
        label_object: 0,
        revision,
    })
}

/// Whether table occurrences `a` and `b` are joined in the file's
/// relationship graph `[3]`, directly or through other occurrences.
fn related(store: &Store, a: u64, b: u64) -> bool {
    let Some(graph) = store.node(&[&[3]]).cloned() else {
        return false;
    };
    let Some(wire) = OwnedNode::list(Vec::new(), vec![graph]).to_wire() else {
        return false;
    };
    let Ok(tree) = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&wire)) else {
        return false;
    };
    let relationships = om_data::meta::relationships(&tree);
    let mut reached = std::collections::HashSet::from([a]);
    let mut pending = vec![a];
    while let Some(occurrence) = pending.pop() {
        if occurrence == b {
            return true;
        }
        for relationship in &relationships {
            let other = if relationship.left == occurrence {
                relationship.right
            } else if relationship.right == occurrence {
                relationship.left
            } else {
                continue;
            };
            if reached.insert(other) {
                pending.push(other);
            }
        }
    }
    false
}

/// Place a portal on a layout's body (#227, operation 21), as FileMaker
/// Pro's Portal tool, Portal Setup and Add Fields to Portal do
/// (layout-model.md 10.17): its occurrence related to the layout's and its
/// fields checked, planned (`layout_placement::plan_portal`), its objects
/// given their UUIDs, stamped and saved under the layout's lock, or refused
/// with nothing written.
fn place_portal(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &PlacePortal,
) -> Reply {
    let id = request.layout;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is unchanged."),
        );
    }
    let placement = PortalPlacement {
        occurrence: request.occurrence,
        frame: rect(&request.frame),
        rows: request.rows,
        fields: request.fields.clone(),
    };
    let key = uint_segment(id);
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], key.clone()];
    let mut store = hosted.write_store();
    let Some(current) = store.node(&[&[LAYOUTS], &[LAYOUT_MEMBERS], &key]).cloned() else {
        return refused(
            status::LAYOUT_MISSING,
            format!("The file has no layout {id}."),
        );
    };
    let current_revision = catalog_revision(&current);
    // The fields: of the occurrence's table, with values Browse edits as
    // text in an edit box.
    let catalog = store.catalog();
    let table_id = catalog
        .table_occurrences
        .iter()
        .find(|o| o.id == request.occurrence)
        .and_then(|o| o.base_table());
    if table_id != Some(request.table) {
        return Reply::Refused {
            status: status::CONFLICT,
            message: format!(
                "Occurrence {} no longer shows the requested table {}. Read the layout again.",
                request.occurrence, request.table
            ),
            current: Some(current_revision),
        };
    }
    let table = table_id.and_then(|table| catalog.table(table));
    for &id in &request.fields {
        let Some(field) = table.and_then(|t| t.field(id)) else {
            return refused(
                status::FIELD_MISSING,
                format!(
                    "Occurrence {} shows no table of this file with a field {id}.",
                    request.occurrence
                ),
            );
        };
        if matches!(field.data_type, DataType::Container | DataType::Unknown(_)) {
            return refused(
                status::INVALID,
                format!(
                    "Field {id} (“{}”) is not text, a number, a date, a time or a timestamp; this \
                     version places those in edit boxes only.",
                    field.name
                ),
            );
        }
    }
    if current_revision != request.expected {
        // Placed by an earlier request whose answer was lost: nothing to do.
        if let Some((portal, _)) = layout_placement::already_placed_portal(&current, &placement) {
            return Reply::Placed(Placed {
                changed: false,
                field_object: portal,
                label_object: 0,
                revision: current_revision,
            });
        }
        return Reply::Refused {
            status: status::CONFLICT,
            message: format!("Layout {id} was changed since the revision the request was made to."),
            current: Some(current_revision),
        };
    }
    let own = layout_placement::definition(&current)
        .ok()
        .and_then(|def| om_layout::occurrence_id(def.table_alias_key));
    if let Some(own) = own.filter(|&own| own != request.occurrence) {
        if !related(&store, own, request.occurrence) {
            return refused(
                status::UNSUPPORTED,
                format!(
                    "Occurrence {} is not related to the layout's occurrence {own} in the \
                     relationship graph; this version places portals of related occurrences only.",
                    request.occurrence
                ),
            );
        }
    }
    let theme = layout_placement::theme_key(&current).and_then(|theme| {
        store
            .node(&[&[THEMES], &[THEME_MEMBERS]])?
            .children
            .iter()
            .find(|t| om_hbam::slot::be_uint(&t.segment) == Some(theme))
    });
    let planned = match layout_placement::plan_portal(&current, &placement, theme) {
        Ok(planned) => planned,
        Err(refusal) => return placement_refused(refusal),
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [(member_path.clone(), LOCK_EXCLUSIVE)];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::LAYOUT_IN_USE,
            format!("Layout {id} is in use by {users}."),
        );
    }
    // Each new object's UUIDs, and the layout's stamp.
    let time = host_time_key();
    let mut member = planned.member;
    for object in std::iter::once(planned.portal).chain(planned.fields.iter().copied()) {
        if let Some(record) = member.at_mut(&[&[OBJECT_ATTRIBUTES], &serial::encode(object)]) {
            record.set_child(OwnedNode::leaf(
                vec![UNIQUE_ID],
                shared.random::<16>().to_vec(),
            ));
            record.set_child(OwnedNode::leaf(
                vec![SOURCE_ID],
                shared.random::<16>().to_vec(),
            ));
        }
    }
    let member = stamped(member, caller.session_name, account, &time);
    let revision = catalog_revision(&member);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path.clone(), member);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: portal not placed on layout {id}: {e}",
            hosted.name
        );
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); layout {id} is unchanged."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: member_path,
            code: ATTENTION_CATALOG,
        }],
    );
    info!(
        "{peer}: {:?}: a portal of occurrence {} placed on layout {id} as object {} with {} \
         fields by {account}",
        hosted.name,
        request.occurrence,
        planned.portal,
        planned.fields.len()
    );
    Reply::Placed(Placed {
        changed: true,
        field_object: planned.portal,
        label_object: 0,
        revision,
    })
}

/// Place a button that performs a script on a layout's body (#226), as
/// [`place_field`] places a field: checked, planned
/// (`layout_placement::plan_button`), given its UUIDs, stamped and saved
/// under the layout's lock, or refused with nothing written.
fn place_button(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &PlaceButton,
) -> Reply {
    let id = request.layout;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is unchanged."),
        );
    }
    if let Err(refusal) = layout_placement::check_label(&request.label) {
        return refused(status::INVALID, refusal.to_string());
    }
    // The parameter's Polish stream is what scripts evaluate, and the
    // formatted stream is the text FileMaker Pro shows and edits: either
    // one that does not decode is refused rather than stored.
    let parameter = match &request.parameter {
        None => None,
        Some((formatted, polish)) => {
            let problem = match om_data::formula::polish::decode(polish) {
                Err(e) => Some(format!("{e:?}")),
                Ok(_) if !formatted.is_empty() => om_data::formula::formatted::decode(formatted)
                    .err()
                    .map(|e| format!("its text: {e:?}")),
                Ok(_) => None,
            };
            if let Some(problem) = problem {
                return refused(
                    status::INVALID,
                    format!("The script parameter is not a calculation this host reads: {problem}"),
                );
            }
            Some(om_scripts::Calculation::new(
                (!formatted.is_empty()).then(|| formatted.clone()),
                polish.clone(),
            ))
        }
    };
    let placement = layout_placement::ButtonPlacement {
        frame: rect(&request.frame),
        label: request.label.clone(),
        action: layout_placement::ButtonAction::PerformScript {
            script: request.script,
            parameter,
        },
    };
    place_planned_button(
        shared,
        peer,
        caller,
        hosted,
        account,
        id,
        &request.expected,
        &placement,
    )
}

/// Operation 8: a button whose action is one script step (Single Step),
/// as the Script Workspace stores a step; `check_button_step` decides
/// which steps a button may hold.
fn place_button_step(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    request: &PlaceButtonStep,
) -> Reply {
    let id = request.layout;
    if hosted.read_only {
        return refused(
            status::SAVE_FAILED[2],
            format!("The file is open read-only; layout {id} is unchanged."),
        );
    }
    if let Err(refusal) = layout_placement::check_label(&request.label) {
        return refused(status::INVALID, refusal.to_string());
    }
    let Some(step) = om_scripts::ScriptStep::from_layout(&om_layout::ScriptStep {
        id: i32::from(request.id),
        value: u32::from(request.value),
        flags: request.flags as i32,
        reserved: request.reserved.to_vec(),
        data: request.data.clone(),
        ..om_layout::ScriptStep::default()
    }) else {
        return refused(status::INVALID, "The button's step is not a script step.");
    };
    let placement = layout_placement::ButtonPlacement {
        frame: rect(&request.frame),
        label: request.label.clone(),
        action: layout_placement::ButtonAction::Step(step),
    };
    place_planned_button(
        shared,
        peer,
        caller,
        hosted,
        account,
        id,
        &request.expected,
        &placement,
    )
}

/// The status for a placement refusal.
fn refusal_status(refusal: &layout_placement::Refusal) -> u16 {
    match refusal {
        layout_placement::Refusal::Invalid(_) => status::INVALID,
        layout_placement::Refusal::Unsupported(_) => status::UNSUPPORTED,
    }
}

/// The reply refusing a placement or an object edit.
fn placement_refused(refusal: layout_placement::Refusal) -> Reply {
    refused(refusal_status(&refusal), refusal.to_string())
}

/// What operations 7 and 8 share once the request is read and its label
/// checked: the step (`ButtonAction::step`), the layout, the script a step names (one
/// of the file's scripts, not a folder or separator), the revision or a
/// retry's button, the plan, the lock, UUIDs, stamp and save.
#[allow(clippy::too_many_arguments)]
fn place_planned_button(
    shared: &Shared,
    peer: &str,
    caller: &Caller<'_>,
    hosted: &Arc<Hosted>,
    account: &str,
    id: u64,
    expected: &Revision,
    placement: &layout_placement::ButtonPlacement,
) -> Reply {
    let step = match placement.action.step() {
        Ok(step) => step,
        Err(refusal) => return refused(refusal_status(&refusal), refusal.to_string()),
    };
    let key = uint_segment(id);
    let member_path = vec![vec![LAYOUTS], vec![LAYOUT_MEMBERS], key.clone()];
    let mut store = hosted.write_store();
    let Some(current) = store.node(&[&[LAYOUTS], &[LAYOUT_MEMBERS], &key]).cloned() else {
        return refused(
            status::LAYOUT_MISSING,
            format!("The file has no layout {id}."),
        );
    };
    let current_revision = catalog_revision(&current);
    // A script the step names: one of the file's, not a folder or
    // separator.
    if let Some(key) = step.script_key() {
        let Some(script) = key.serial_id() else {
            return refused(status::INVALID, "The step's script key is not a script id.");
        };
        match store
            .node(&[&[SCRIPTS], &[DEFINITIONS], &serial::encode(script)])
            .map(decode)
        {
            Some(Ok(found)) if found.kind == ScriptKind::Script => {}
            Some(_) => {
                return refused(
                    status::INVALID,
                    format!("Member {script} of the script catalog is not a script."),
                )
            }
            None => {
                return refused(
                    status::SCRIPT_MISSING,
                    format!("The file has no script {script}."),
                )
            }
        }
    }
    let action = om_scripts::step_name(step.id()).unwrap_or("a step");
    if current_revision != *expected {
        // Placed by an earlier request whose answer was lost: nothing to do.
        if let Some(button) = layout_placement::already_placed_button(&current, placement) {
            return Reply::Placed(Placed {
                changed: false,
                field_object: button,
                label_object: 0,
                revision: current_revision,
            });
        }
        return Reply::Refused {
            status: status::CONFLICT,
            message: format!("Layout {id} was changed since the revision the request was made to."),
            current: Some(current_revision),
        };
    }
    let planned = match layout_placement::plan_button(&current, placement) {
        Ok(planned) => planned,
        Err(refusal) => return refused(refusal_status(&refusal), refusal.to_string()),
    };
    let user = LockUser {
        connection: caller.connection,
        txn: AUTHORING_TXN,
    };
    let holder = format!("{} ({account})", caller.session_name);
    let locks = [(member_path.clone(), LOCK_EXCLUSIVE)];
    if let Err(users) = lock_all(hosted, user, &holder, &locks) {
        return refused(
            status::LAYOUT_IN_USE,
            format!("Layout {id} is in use by {users}."),
        );
    }
    let time = host_time_key();
    let mut member = planned.member;
    if let Some(record) = member.at_mut(&[&[OBJECT_ATTRIBUTES], &serial::encode(planned.button)]) {
        record.set_child(OwnedNode::leaf(
            vec![UNIQUE_ID],
            shared.random::<16>().to_vec(),
        ));
        record.set_child(OwnedNode::leaf(
            vec![SOURCE_ID],
            shared.random::<16>().to_vec(),
        ));
    }
    let member = stamped(member, caller.session_name, account, &time);
    let revision = catalog_revision(&member);
    let mut journal = store.begin_commit();
    store.replace_schema_in(&mut journal, member_path.clone(), member);
    let saved = hosted.save_commit(&mut store, journal);
    drop(store);
    unlock_all(hosted, user, &locks);
    if let Err(e) = saved {
        warn!(
            "{peer}: {:?}: button ({action}) not placed on layout {id}: {e}",
            hosted.name
        );
        return refused(
            save_error(&e),
            format!("The file could not be saved ({e}); layout {id} is unchanged."),
        );
    }
    writes::notify(
        hosted,
        &shared.mailboxes,
        caller.connection,
        &[Change {
            path: member_path,
            code: ATTENTION_CATALOG,
        }],
    );
    info!(
        "{peer}: {:?}: button ({action}) placed on layout {id} as object {} by {account}",
        hosted.name, planned.button
    );
    Reply::Placed(Placed {
        changed: true,
        field_object: planned.button,
        label_object: 0,
        revision,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;
    use crate::config::{ServerConfig, ServerIdentity};
    use crate::{Identity, Server};

    #[test]
    fn stamps_count_up_from_none_and_keep_other_children() {
        let member = OwnedNode::list(
            vec![2],
            vec![
                OwnedNode::leaf(vec![4], vec![0; 28]),
                OwnedNode::leaf(vec![0x10], b"x".to_vec()),
            ],
        );
        let once = stamped(member.clone(), "OpenMaker", "Admin", &[1; 8]);
        assert_eq!(
            once.child(MOD_COUNT).unwrap().value.as_deref(),
            Some(&[1, 1][..])
        );
        assert_eq!(once.child(MOD_TIME).unwrap().value.as_ref().unwrap()[0], 8);
        // Five seconds after midnight the time part is one digit: a 6-byte
        // key, written with its own length.
        let midnight =
            om_collate::timestamp_key("11/13/2026 0:00:05", om_collate::DateOrder::MonthDayYear)
                .unwrap();
        assert_eq!(midnight.len(), 6);
        let late = stamped(member.clone(), "OpenMaker", "Admin", &midnight);
        let when = late.child(MOD_TIME).unwrap().value.clone().unwrap();
        assert_eq!((when[0], &when[1..]), (6, &midnight[..]));
        let twice = stamped(once.clone(), "OpenMaker", "Admin", &[1; 8]);
        assert_eq!(
            twice.child(MOD_COUNT).unwrap().value.as_deref(),
            Some(&[1, 2][..])
        );
        assert_eq!(other_change(&member, &twice), None);
        assert_eq!(content_revision(&member), content_revision(&twice));
        let mut renamed = twice.clone();
        renamed.set_child(OwnedNode::leaf(vec![0x10], b"y".to_vec()));
        assert_eq!(other_change(&twice, &renamed), Some(vec![0x10]));
        let mut steps = twice.clone();
        steps.set_child(OwnedNode::leaf(vec![4], vec![1; 28]));
        assert_eq!(other_change(&twice, &steps), None);
    }

    /// A step list naming one large data node many times decodes it once
    /// (#149), and decoding is charged for it once, beside the steps.
    #[test]
    fn decoding_is_charged_once_for_data_many_steps_name() {
        let mut step = om_scripts::ScriptStep::new(141);
        step.set_text(0, Some(&"x".repeat(1000)));
        let mut script = Script {
            id: 2,
            name: "Steps".into(),
            ..Default::default()
        };
        script.insert_steps(0, vec![step]).unwrap();
        let one = script.encode();
        let descriptor = one
            .child(&[STEP_LIST])
            .and_then(OwnedNode::assembled)
            .unwrap()
            .into_owned();
        let mut many = one.clone();
        many.set_child(OwnedNode::leaf(vec![STEP_LIST], descriptor.repeat(50)));
        let data = memory(&one.child(&[STEP_DATA]).unwrap().children[0]);
        let steps = 49 * (28 + std::mem::size_of::<om_scripts::ScriptStep>());
        assert!(decoding_cost(&many) >= decoding_cost(&one) + steps);
        // Charged per step, the data alone would add 49 x 2 times its size.
        assert!(decoding_cost(&many) < decoding_cost(&one) + 49 * data);
        let decoded = decode(&many).unwrap();
        assert_eq!(
            decoded.steps.iter().filter(|s| s.data.is_some()).count(),
            50
        );
    }

    /// A caller is checked for what the operation needs before its
    /// definition is read (#385): a caller that has not opened the file is
    /// refused for that, though the server has no memory left for its
    /// definition, and nothing is reserved for the request.
    #[test]
    fn callers_are_checked_before_a_definition_is_read() {
        let config = ServerConfig {
            identity: ServerIdentity {
                name: "openmaker-test".into(),
                version_string: "OpenMaker Server test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: Vec::new(),
            insecure_files: false,
            record_dir: None,
        };
        let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
        let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
        let shared = server.handle().unwrap().shared;
        shared.upload_memory.set_limit(0);
        let request = Request::ReplaceScriptSteps(ReplaceScriptSteps {
            script: 2,
            expected: [0; 32],
            definition: Script {
                id: 2,
                name: "Steps".into(),
                ..Default::default()
            }
            .encode(),
        });
        let txn = authoring::build_request(9, 1, &request).unwrap();
        let caller = Caller {
            file: None,
            account: None,
            connection: 1,
            session_name: "Connected only",
        };
        let reserved = Cell::new(false);
        let reply = perform(&shared, "peer", &caller, &txn, || {
            reserved.set(true);
            Charge::none(&shared.upload_memory, IpAddr::V4(Ipv4Addr::LOCALHOST))
        })
        .unwrap();
        assert!(matches!(
            authoring::parse_reply(&request, &reply).unwrap(),
            Reply::Refused {
                status: status::FILE_MISSING,
                ..
            }
        ));
        assert!(!reserved.get());
    }
}
