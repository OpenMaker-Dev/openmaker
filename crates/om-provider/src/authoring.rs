//! Authoring on the host (`docs/authoring.md`): whether this connection
//! may change scripts, add fields or place them on layouts, the scripts,
//! field catalogs and layouts as stored, and replacing a script's steps,
//! adding a field or placing one on a layout through OpenMaker Server's
//! authoring extension (`om_protocol::authoring`).
//!
//! The host must claim the extension first (CORBA's `_is_a`), which a host
//! without it answers `false`, as FileMaker Server 26.0.3 did for
//! OpenMaker's file list interface. Existing script steps can also be saved
//! through the native FileMaker Server path in `scripts.rs`. Manage Database
//! sends FileMaker Pro's own schema transaction and reads it back (`crate::schema`);
//! it asks [`NativeProvider::field_authoring`] whether the account may
//! change the design.

use std::collections::BTreeMap;

use om_data::{DataError, Script, ScriptCatalog};
use om_hbam::{serial, OwnedNode};
use om_protocol::authoring::{
    self, content_revision, status, Reply, Request, Revision, OP_CHANGE_PARTS, OP_CREATE_FIELD,
    OP_CREATE_LAYOUT, OP_DELETE_LAYOUT, OP_DELETE_OBJECTS, OP_DUPLICATE_LAYOUT, OP_FIELD_CONTROL,
    OP_LAYOUT_SETUP, OP_LAYOUT_SETUP_MARGINS, OP_LAYOUT_WIDTH, OP_PART_OPTIONS, OP_PLACE_FIELD,
    OP_REPLACE_SCRIPT_STEPS, OP_SAVE, OP_SET_OBJECT_FRAMES, OP_SET_TEXT,
};

use crate::{
    download, download_node, download_tree, download_value, failure, key_bytes, NativeProvider,
};

/// The script catalog `[17]`, its index `[17, 1]` and its definitions
/// `[17, 5]`; the layout catalog `[4]` and its members `[4, 5]`.
const SCRIPTS: u64 = 17;
const INDEX: u64 = 1;
const DEFINITIONS: u64 = 5;
const LAYOUTS: u64 = 4;
const LAYOUT_MEMBERS: u64 = 5;

/// Whether this connection may do an authoring operation on its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authoring {
    /// It can, with script definitions up to this size in their wire form.
    Available { max_definition_bytes: usize },
    /// The host offers authoring, but not to this account.
    Denied(String),
    /// The host does not offer it.
    Unavailable(String),
}

enum ScriptSave {
    Extension,
    Native,
}

/// A file's design catalogs as the host gave them, for the reference
/// graph (`om_data::design_refs`): one download of each of
/// [`om_data::design_refs::DESIGN_CATALOGS`] and of each table's field
/// catalog, and the catalogs the host would not give.
pub struct DesignCatalogs {
    downloads: Vec<crate::Download>,
    /// What the host refused or could not send, with its error.
    pub unavailable: Vec<om_data::design_refs::Unavailable>,
    /// The catalogs whose modification count `[cat, 1, fc]` differed
    /// between the start and the end of the read, or could not be read:
    /// the design may have changed while it was read
    /// (`DesignGraph::note_changed_during_read`).
    pub changed: Vec<String>,
}

impl DesignCatalogs {
    /// The catalogs as one key tree.
    pub fn tree(&self) -> Result<om_hbam::KeyTree<'_>, DataError> {
        download_tree(&self.downloads)
    }
}

/// The scripts as the host stores them: the catalog, each member's
/// definition node `[17, 5, S]`, by id, and the catalog's index `[17, 1]`.
#[derive(Clone, Debug, Default)]
pub struct ScriptDefinitions {
    pub catalog: ScriptCatalog,
    pub definitions: BTreeMap<u64, OwnedNode>,
    pub index: Option<OwnedNode>,
}

/// The layouts as the host stores them, from one download of `[4]`: the
/// members `[4, 5, L]` the account may see, by id, and the catalog's index
/// `[4, 1]` ([`NativeProvider::layout_catalog_snapshot`]).
#[derive(Clone, Debug, Default)]
pub struct LayoutCatalogSnapshot {
    pub members: BTreeMap<u64, OwnedNode>,
    pub index: Option<OwnedNode>,
}

/// Why a replacement did not happen, or may not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthoringError {
    /// The host refused it with `status` (`om_protocol::authoring::status`);
    /// nothing changed. A conflict names the script's or the table's
    /// fields' current revision.
    Refused {
        status: u16,
        message: String,
        current: Option<Revision>,
    },
    /// Not offered on this host or to this account; nothing was sent.
    Unavailable(String),
    /// The request was not sent; nothing changed.
    NotSent(String),
    /// An upload was attempted and the result could not be confirmed.
    /// Read the script again before making another edit.
    OutcomeUnknown(String),
}

impl std::fmt::Display for AuthoringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthoringError::Refused {
                status, message, ..
            } => write!(f, "{message} (status {status})"),
            AuthoringError::Unavailable(reason) | AuthoringError::NotSent(reason) => {
                f.write_str(reason)
            }
            AuthoringError::OutcomeUnknown(reason) => {
                write!(f, "The edit could not be confirmed: {reason}")
            }
        }
    }
}

impl std::error::Error for AuthoringError {}

impl NativeProvider {
    /// Whether this connection may replace the steps of an existing script.
    pub fn script_authoring(&mut self) -> Result<Authoring, DataError> {
        self.script_step_authoring()
            .map(|(availability, _)| availability)
    }

    /// Script Workspace's catalog editing requires both the extension and
    /// its disk-save confirmation.
    pub(crate) fn script_extension_authoring(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_REPLACE_SCRIPT_STEPS, "replacing script steps")
    }

    fn script_step_authoring(&mut self) -> Result<(Authoring, ScriptSave), DataError> {
        if self.uses_native_script_save()? {
            let availability = match &self.privileges.principal {
                Some(principal) if principal.is_full_access() => Authoring::Available {
                    max_definition_bytes: authoring::MAX_DEFINITION_BYTES,
                },
                _ => Authoring::Denied(
                    "Native script saves require a verified [Full Access] account.".into(),
                ),
            };
            return Ok((availability, ScriptSave::Native));
        }
        self.script_extension_authoring()
            .map(|grant| (grant, ScriptSave::Extension))
    }

    /// Whether this host uses the two-upload native path for step edits.
    /// A matching member revision alone cannot confirm such an edit.
    pub fn uses_native_script_save(&mut self) -> Result<bool, DataError> {
        self.require_open()?;
        let session = self.session.as_ref().expect("open session");
        let identity = session.identity();
        let filemaker_server = identity.exe_type == 0x22
            && identity.app_type == 0x03
            && identity.version_string.starts_with("Server 26.");
        Ok(filemaker_server && !self.has_authoring_extension()?)
    }

    /// Whether this connection may add fields to tables on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn field_authoring(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_CREATE_FIELD, "adding fields")
    }

    /// Whether this connection may place fields on layouts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn layout_authoring(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_PLACE_FIELD, "placing fields on layouts")
    }

    /// Whether this connection may add layouts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn layout_creation(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_CREATE_LAYOUT, "adding layouts")
    }

    /// Whether this connection may duplicate layouts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn layout_duplication(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_DUPLICATE_LAYOUT, "duplicating layouts")
    }

    /// Whether this connection may delete layouts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn layout_deletion(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_DELETE_LAYOUT, "deleting layouts")
    }

    /// Delete a layout as FileMaker Pro's Delete Layout does, if the layout
    /// catalog's index is still at revision `expected`. The host checks the
    /// account, the layout and its lock, releases its library uses and
    /// saves the file before it answers.
    pub fn delete_layout_on_host(
        &mut self,
        delete: authoring::DeleteLayout,
    ) -> Result<authoring::Deleted, AuthoringError> {
        match self.layout_deletion() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::DeleteLayout(delete))? {
            Reply::Deleted(deleted) => Ok(deleted),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement deleting layouts.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the deletion with something else".into(),
            )),
        }
    }

    /// Whether this connection may move and resize layout objects on its
    /// host, as [`NativeProvider::script_authoring`].
    pub fn object_editing(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_SET_OBJECT_FRAMES, "moving and resizing layout objects")
    }

    /// Whether this connection may delete layout objects on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn object_deletion(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_DELETE_OBJECTS, "deleting layout objects")
    }

    /// Whether the host confirms saves (`Request::Save`), as
    /// [`NativeProvider::script_authoring`]: a catalog save asks before it
    /// uploads anything, so a host that cannot confirm one is never changed.
    pub fn save_confirming(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_SAVE, "confirming that the file on disk holds a change")
    }

    /// Whether this connection may do `operation` (an `OP_` bit of
    /// `om_protocol::authoring`), described as `what`.
    fn authoring(&mut self, operation: u32, what: &str) -> Result<Authoring, DataError> {
        if !self.has_authoring_extension()? {
            return Ok(Authoring::Unavailable(format!(
                "The host (“{}”) does not offer OpenMaker's authoring extension.",
                self.host_version
            )));
        }
        self.describe_authoring(operation, what)
    }

    fn has_authoring_extension(&mut self) -> Result<bool, DataError> {
        self.require_open()?;
        self.session
            .as_mut()
            .expect("open session")
            .is_a(authoring::REPO_ID)
            .map_err(|e| failure("ask the host for authoring", e))
    }

    fn describe_authoring(&mut self, operation: u32, what: &str) -> Result<Authoring, DataError> {
        let reply = self
            .authoring_request(&Request::Describe)
            .map_err(|e| DataError::new(e.to_string()))?;
        Ok(match reply {
            Reply::Described(d) if d.operations & operation == 0 => {
                Authoring::Unavailable(format!("This OpenMaker Server does not offer {what}."))
            }
            Reply::Described(d) if !d.can_author => Authoring::Denied(format!(
                "The account “{}” may not change scripts or the schema on this server.",
                self.account_name
            )),
            Reply::Described(d) => Authoring::Available {
                max_definition_bytes: d.max_definition_bytes as usize,
            },
            Reply::NotSupported { .. } => {
                Authoring::Unavailable("This OpenMaker Server predates authoring.".into())
            }
            Reply::Refused { message, .. } => Authoring::Denied(message),
            Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => {
                return Err(DataError::new(
                    "The host answered a description request with an edit.",
                ))
            }
        })
    }

    /// Table `table`'s field catalog `[T, 3]` as stored on the host, read
    /// now; `None` when the table has none.
    pub fn field_catalog(&mut self, table: u64) -> Result<Option<OwnedNode>, DataError> {
        self.require_open()?;
        let path = vec![key_bytes(table), vec![3]];
        let Some(downloaded) = download_node(self.session.as_mut().expect("open session"), path)?
        else {
            return Ok(None);
        };
        let tree = download_tree(std::slice::from_ref(&downloaded))?;
        Ok(tree.at(&[table, 3]).map(|catalog| catalog.to_owned_node()))
    }

    /// Add a field to table `table` if its fields are still at revision
    /// `expected` (`om_protocol::authoring::catalog_revision`). The host
    /// checks the account, the locks, the revision and the name, allocates
    /// the field's id and saves the file before it answers.
    pub fn create_field(
        &mut self,
        create: authoring::CreateField,
    ) -> Result<authoring::Created, AuthoringError> {
        match self.field_authoring() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        let request = Request::CreateField(create);
        match self.authoring_request(&request)? {
            Reply::Created(created) => Ok(created),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement adding fields.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the new field with something else".into(),
            )),
        }
    }

    /// The layout members from one catalog download. Names, definitions
    /// and revision tokens derived from these members describe the same
    /// observation even if another editor changes a layout immediately after.
    /// A layout the account's custom set gives no access is left out;
    /// folders, their ends and separators stay (session-identity.md 2.2).
    pub fn layout_members(&mut self) -> Result<BTreeMap<u64, OwnedNode>, DataError> {
        Ok(self.layout_catalog_snapshot()?.members)
    }

    /// [`Self::layout_members`] and the catalog's index `[4, 1]` from the
    /// same download, so a layout's words read from its member, else from
    /// its index entry (`om_data::layout_catalog::layout_word`, #977),
    /// describe one observation.
    pub fn layout_catalog_snapshot(&mut self) -> Result<LayoutCatalogSnapshot, DataError> {
        self.require_open()?;
        let Some(downloaded) = download_node(
            self.session.as_mut().expect("open session"),
            vec![vec![LAYOUTS as u8]],
        )?
        else {
            return Ok(LayoutCatalogSnapshot::default());
        };
        let tree = download_tree(std::slice::from_ref(&downloaded))?;
        let members = tree
            .at(&[LAYOUTS, LAYOUT_MEMBERS])
            .into_iter()
            .flat_map(|members| &members.children)
            .filter_map(|member| Some((member.id()?, member.to_owned_node())))
            .filter(|(id, _)| {
                om_data::meta::layout_flags(&tree, *id) & (1 | 2 | 0x10) != 0
                    || self.shows_layout(*id)
            })
            .collect();
        Ok(LayoutCatalogSnapshot {
            members,
            index: tree.at(&[LAYOUTS, 1]).map(|index| index.to_owned_node()),
        })
    }

    /// Layout `layout`'s catalog member `[4, 5, L]` as stored on the host,
    /// read now: its definition, parts, objects, object attributes and
    /// indexes. `None` when the file has no such layout; refused with 9 when
    /// the account's custom set gives it no access.
    pub fn layout_member(&mut self, layout: u64) -> Result<Option<OwnedNode>, DataError> {
        self.require_open()?;
        if !self.shows_layout(layout) {
            return Err(DataError::with_code(
                9,
                "This account's privilege set gives no access to that layout.",
            ));
        }
        let path = vec![
            vec![LAYOUTS as u8],
            vec![LAYOUT_MEMBERS as u8],
            key_bytes(layout),
        ];
        let Some(downloaded) = download_node(self.session.as_mut().expect("open session"), path)?
        else {
            return Ok(None);
        };
        let tree = download_tree(std::slice::from_ref(&downloaded))?;
        Ok(tree
            .at(&[LAYOUTS, LAYOUT_MEMBERS, layout])
            .map(|member| member.to_owned_node()))
    }

    /// Place a field on a layout if the layout is still at revision
    /// `expected` (`om_protocol::authoring::layout_revision` of its member).
    /// The host checks the account, the layout's lock, the revision, the
    /// field and the frames, allocates the objects' keys and saves the file
    /// before it answers.
    pub fn place_field(
        &mut self,
        place: authoring::PlaceField,
    ) -> Result<authoring::Placed, AuthoringError> {
        match self.layout_authoring() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        let request = Request::PlaceField(place);
        match self.authoring_request(&request)? {
            Reply::Placed(placed) => Ok(placed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement placing fields on layouts.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the placement with something else".into(),
            )),
        }
    }

    /// The layout catalog's index `[4, 1]` as stored on the host, read now;
    /// `None` when the file has none.
    pub fn layout_index(&mut self) -> Result<Option<OwnedNode>, DataError> {
        self.require_open()?;
        let path = vec![vec![LAYOUTS as u8], vec![1]];
        let Some(downloaded) = download_node(self.session.as_mut().expect("open session"), path)?
        else {
            return Ok(None);
        };
        let tree = download_tree(std::slice::from_ref(&downloaded))?;
        Ok(tree.at(&[LAYOUTS, 1]).map(|index| index.to_owned_node()))
    }

    /// Add a layout if the layout catalog's index is still at revision
    /// `expected`. The host checks the account, the name and the
    /// occurrence, allocates the layout's id and saves the file before it
    /// answers.
    pub fn create_layout_on_host(
        &mut self,
        create: authoring::CreateLayout,
    ) -> Result<authoring::LayoutCreated, AuthoringError> {
        self.add_layout(
            OP_CREATE_LAYOUT,
            "adding layouts",
            Request::CreateLayout(create),
        )
    }

    /// Copy a layout as FileMaker Pro's Duplicate Layout does, if the
    /// layout catalog's index is still at revision `expected`. The host
    /// checks the account and the layout, allocates the copy's id, counts
    /// the library items it shows and saves the file before it answers.
    pub fn duplicate_layout_on_host(
        &mut self,
        duplicate: authoring::DuplicateLayout,
    ) -> Result<authoring::LayoutCreated, AuthoringError> {
        self.add_layout(
            OP_DUPLICATE_LAYOUT,
            "duplicating layouts",
            Request::DuplicateLayout(duplicate),
        )
    }

    /// Send `request`, which adds a layout with operation `operation`
    /// (`what`), if the host offers it; the reply is
    /// [`Reply::LayoutCreated`].
    fn add_layout(
        &mut self,
        operation: u32,
        what: &str,
        request: Request,
    ) -> Result<authoring::LayoutCreated, AuthoringError> {
        match self.authoring(operation, what) {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&request)? {
            Reply::LayoutCreated(created) => Ok(created),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: format!("The host does not implement {what}."),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the new layout with something else".into(),
            )),
        }
    }

    /// Move or resize objects of a layout if the layout is still at
    /// revision `expected` (`om_protocol::authoring::layout_revision` of its
    /// member). The host checks the account, the layout's lock, the
    /// revision and the frames and saves the file before it answers.
    pub fn set_object_frames(
        &mut self,
        set: authoring::SetObjectFrames,
    ) -> Result<authoring::Framed, AuthoringError> {
        match self.object_editing() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        let request = Request::SetObjectFrames(set);
        match self.authoring_request(&request)? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement moving layout objects.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the moved objects with something else".into(),
            )),
        }
    }

    /// Whether this connection may set parts' options on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn part_options(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_PART_OPTIONS, "setting parts' options")
    }

    /// Whether this connection may add, delete and resize parts on its
    /// host, as [`NativeProvider::script_authoring`].
    pub fn parts_editing(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_CHANGE_PARTS, "changing layouts' parts")
    }

    /// Add, delete or resize a part if its layout is still at revision
    /// `expected` ([`authoring::ChangeParts`]). The host checks the account,
    /// the layout and its lock, and saves the file before it answers.
    pub fn parts_on_host(
        &mut self,
        change: authoring::ChangeParts,
    ) -> Result<authoring::Framed, AuthoringError> {
        match self.parts_editing() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::ChangeParts(change))? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement changing parts.".into(),
                current: None,
            }),
            _ => Err(AuthoringError::OutcomeUnknown(
                "the host answered the change of parts with something else".into(),
            )),
        }
    }

    /// Whether this connection may retype layout texts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn text_editing(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_SET_TEXT, "changing layout texts")
    }

    /// Show a text object with new words if its layout is still at
    /// revision `expected`. The host checks the account, the layout and its
    /// lock and the text, and saves the file before it answers.
    pub fn text_on_host(
        &mut self,
        set: authoring::SetText,
    ) -> Result<authoring::Framed, AuthoringError> {
        match self.text_editing() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::SetText(set))? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement changing a layout's texts.".into(),
                current: None,
            }),
            _ => Err(AuthoringError::OutcomeUnknown(
                "the host answered the text with something else".into(),
            )),
        }
    }

    /// Set Part Setup's options for one part of a layout if the layout is
    /// still at revision `expected`. The host checks the account, the
    /// layout and its lock, the part and the break field, and saves the
    /// file before it answers.
    pub fn part_options_on_host(
        &mut self,
        set: authoring::PartOptions,
    ) -> Result<authoring::Framed, AuthoringError> {
        match self.part_options() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::PartOptions(set))? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement Part Setup's options.".into(),
                current: None,
            }),
            _ => Err(AuthoringError::OutcomeUnknown(
                "the host answered the part's options with something else".into(),
            )),
        }
    }

    /// Whether this connection may change field controls on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn field_controls(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_FIELD_CONTROL, "changing field controls")
    }

    /// Show a field object with another control if its layout is still at
    /// revision `expected`. The host checks the account, the layout and its
    /// lock, the field and the value list, and saves the file before it
    /// answers.
    pub fn field_control_on_host(
        &mut self,
        set: authoring::FieldControl,
    ) -> Result<authoring::Framed, AuthoringError> {
        match self.field_controls() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::FieldControl(set))? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement field controls.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the field control with something else".into(),
            )),
        }
    }

    /// Whether this connection may set layouts' widths on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn layout_widths(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_LAYOUT_WIDTH, "setting layouts' widths")
    }

    /// Set a layout's width if its member is still at revision `expected`.
    /// The host checks the account, the layout and its lock and saves the
    /// file before it answers.
    pub fn layout_width_on_host(
        &mut self,
        set: authoring::LayoutWidth,
    ) -> Result<authoring::Framed, AuthoringError> {
        match self.layout_widths() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::LayoutWidth(set))? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement setting a layout's width.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the layout's width with something else".into(),
            )),
        }
    }

    /// Whether this connection may set up layouts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn layout_setting(&mut self) -> Result<Authoring, DataError> {
        self.authoring(OP_LAYOUT_SETUP, "setting up layouts")
    }

    /// Whether this connection may set up layouts with their fixed page
    /// margins on its host (operation 23), as
    /// [`NativeProvider::script_authoring`].
    pub fn layout_margin_setting(&mut self) -> Result<Authoring, DataError> {
        self.authoring(
            OP_LAYOUT_SETUP_MARGINS,
            "setting layouts' fixed page margins",
        )
    }

    /// Set a layout's name and views as Layout Setup does if its member is
    /// still at revision `expected`. The host checks the account, the
    /// layout and its lock and saves the file before it answers.
    pub fn layout_setup_on_host(
        &mut self,
        setup: authoring::LayoutSetup,
    ) -> Result<authoring::Framed, AuthoringError> {
        // With margins, operation 23, which a host may lack.
        let available = match setup.margins {
            Some(_) => self.layout_margin_setting(),
            None => self.layout_setting(),
        };
        match available {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::LayoutSetup(setup))? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement Layout Setup.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered Layout Setup with something else".into(),
            )),
        }
    }

    /// Whether this connection may change value lists' custom values
    /// (authoring operation 24, #869).
    pub fn value_list_editing(&mut self) -> Result<Authoring, DataError> {
        self.authoring(
            om_protocol::authoring::OP_VALUE_LIST_VALUES,
            "changing value lists",
        )
    }

    /// Value list `list`'s catalog member `[21, 5, V]` as stored on the
    /// host, read now. `None` when the file has no such list; refused with
    /// 9 when the account's custom set gives it no access.
    pub fn value_list_member(&mut self, list: u64) -> Result<Option<OwnedNode>, DataError> {
        self.require_open()?;
        if !self.shows_value_list(list) {
            return Err(DataError::with_code(
                9,
                "This account's privilege set gives no access to that value list.",
            ));
        }
        let catalog = om_data::meta::VALUE_LISTS;
        let path = vec![
            key_bytes(catalog[0]),
            key_bytes(catalog[1]),
            key_bytes(list),
        ];
        let Some(downloaded) = download_node(self.session.as_mut().expect("open session"), path)?
        else {
            return Ok(None);
        };
        let tree = download_tree(std::slice::from_ref(&downloaded))?;
        Ok(tree
            .at(&[catalog[0], catalog[1], list])
            .map(|member| member.to_owned_node()))
    }

    /// Set custom value list `set.list`'s values if its member is still at
    /// revision `set.expected`. The host checks the account, the list, its
    /// lock and the revision, and saves the file before it answers.
    pub fn value_list_values_on_host(
        &mut self,
        set: authoring::ValueListValues,
    ) -> Result<authoring::Framed, AuthoringError> {
        match self.value_list_editing() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&Request::ValueListValues(set))? {
            Reply::Framed(framed) => Ok(framed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement changing value lists.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the value list change with something else".into(),
            )),
        }
    }

    /// Delete objects of a layout if the layout is still at revision
    /// `expected` (`om_protocol::authoring::layout_revision` of its
    /// member). The host checks the account, the layout's lock, the
    /// revision and the objects and saves the file before it answers.
    pub fn delete_objects(
        &mut self,
        delete: authoring::DeleteObjects,
    ) -> Result<authoring::Deleted, AuthoringError> {
        match self.object_deletion() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        let request = Request::DeleteObjects(delete);
        match self.authoring_request(&request)? {
            Reply::Deleted(deleted) => Ok(deleted),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement deleting layout objects.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the deletion with something else".into(),
            )),
        }
    }

    /// Place a button that performs a script on a layout
    /// ([`authoring::PlaceButton`], #226), where the host offers it.
    pub fn place_button(
        &mut self,
        place: authoring::PlaceButton,
    ) -> Result<authoring::Placed, AuthoringError> {
        self.place_object(
            authoring::OP_PLACE_BUTTON,
            "buttons",
            Request::PlaceButton(place),
        )
    }

    /// Place a button whose action is one script step
    /// ([`authoring::PlaceButtonStep`]), where the host offers it.
    pub fn place_button_step(
        &mut self,
        place: authoring::PlaceButtonStep,
    ) -> Result<authoring::Placed, AuthoringError> {
        self.place_object(
            authoring::OP_PLACE_BUTTON_STEP,
            "buttons",
            Request::PlaceButtonStep(place),
        )
    }

    /// Place a text on a layout's body on its own ([`authoring::PlaceText`],
    /// the Text tool), where the host offers it.
    pub fn place_text(
        &mut self,
        place: authoring::PlaceText,
    ) -> Result<authoring::Placed, AuthoringError> {
        self.place_object(authoring::OP_PLACE_TEXT, "texts", Request::PlaceText(place))
    }

    /// Whether this connection may place texts on layouts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn text_placement(&mut self) -> Result<Authoring, DataError> {
        self.authoring(authoring::OP_PLACE_TEXT, "placing texts on layouts")
    }

    /// Place a portal on a layout's body ([`authoring::PlacePortal`],
    /// operation 21), where the host offers it. The reply's `field_object`
    /// is the portal's key; its fields have the keys after it.
    pub fn place_portal(
        &mut self,
        place: authoring::PlacePortal,
    ) -> Result<authoring::Placed, AuthoringError> {
        self.place_object(
            authoring::OP_PLACE_PORTAL,
            "portals",
            Request::PlacePortal(place),
        )
    }

    /// Whether this connection may place portals on layouts on its host, as
    /// [`NativeProvider::script_authoring`].
    pub fn portal_placement(&mut self) -> Result<Authoring, DataError> {
        self.authoring(authoring::OP_PLACE_PORTAL, "placing portals on layouts")
    }

    /// Send a placement of `what` ("buttons") that operation `operation`
    /// makes, if the host offers it; the reply is [`Reply::Placed`].
    fn place_object(
        &mut self,
        operation: u32,
        what: &str,
        request: Request,
    ) -> Result<authoring::Placed, AuthoringError> {
        match self.authoring(operation, &format!("placing {what} on layouts")) {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        match self.authoring_request(&request)? {
            Reply::Placed(placed) => Ok(placed),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: format!("The host does not implement placing {what} on layouts."),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered the placement with something else".into(),
            )),
        }
    }

    /// The file's design catalogs as stored on the host, read now: each
    /// catalog the reference graph reads, whole, and each table's field
    /// catalog. A catalog the host does not have is left out; one it does
    /// not send is listed in [`DesignCatalogs::unavailable`] with the
    /// host's error. No record or account is read: of the security
    /// catalog, only the privilege sets `[23, 2]`, which hold no
    /// credential.
    pub fn design_catalogs(&mut self) -> Result<DesignCatalogs, DataError> {
        self.require_open()?;
        let session = self.session.as_mut().expect("open session");
        let mut downloads = Vec::new();
        let mut unavailable = Vec::new();
        // Each catalog index's modification count, read before the catalogs
        // and again after (schema-path.md 8): FileMaker Pro reads the same
        // count to tell whether its copy of a catalog is current.
        let mut counts: Vec<(Vec<u64>, String, ModCount)> = Vec::new();
        for (path, what) in om_data::design_refs::DESIGN_CATALOGS {
            for (index, what) in catalog_indexes(path, what) {
                let count = mod_count(session, &index);
                counts.push((index, what, count));
            }
        }
        for (path, what) in om_data::design_refs::DESIGN_CATALOGS {
            get(
                session,
                path.to_vec(),
                what.to_owned(),
                &mut downloads,
                &mut unavailable,
            );
        }
        // The tables the schema catalog `[3]` lists, by their member keys
        // alone, then their fields. Nothing here is defaulted: a member
        // list or member key that does not read is a failure of this
        // download, reported with the catalogs not given, since the fields
        // of a table it hides were never asked for.
        let mut tables = Vec::new();
        if let Some(schema) = downloads.iter().find(|d| d.path == [key_bytes(3)]) {
            let tree = download_tree(std::slice::from_ref(schema))?;
            match tree.at(&[3, 16, 5]) {
                Some(list) if list.value.is_none_or(<[u8]>::is_empty) => {
                    for member in &list.children {
                        let deleted =
                            member.children.is_empty() && member.value.is_none_or(<[u8]>::is_empty);
                        match member.id() {
                            Some(0) => {}
                            Some(_) if deleted => {}
                            Some(id) => {
                                let name = member
                                    .get(0x10)
                                    .and_then(om_hbam::Node::text)
                                    .and_then(Result::ok)
                                    .unwrap_or_else(|| format!("table {id}"));
                                tables.push((id, name));
                            }
                            None => unavailable.push(om_data::design_refs::Unavailable {
                                path: vec![3, 16, 5],
                                what: format!("the fields of base table {}", member.segment),
                                reason: "the table's key does not read, so they were not asked \
                                         for"
                                .into(),
                            }),
                        }
                    }
                }
                Some(_) => unavailable.push(om_data::design_refs::Unavailable {
                    path: vec![3, 16, 5],
                    what: "the fields of every base table".into(),
                    reason: "the base tables' member list does not read, so no table's fields \
                             were asked for"
                        .into(),
                }),
                None => {}
            }
        }
        for (table, name) in tables {
            let what = format!("the fields of table “{name}”");
            let count = mod_count(session, &[table, 3]);
            counts.push((vec![table, 3], what.clone(), count));
            get(
                session,
                vec![table, 3],
                what,
                &mut downloads,
                &mut unavailable,
            );
        }
        // The counts again: a catalog whose count moved, or whose count
        // could not be read either time, may have changed while it was read.
        let mut changed = Vec::new();
        for (index, what, before) in counts {
            let after = mod_count(session, &index);
            match (before, after) {
                (Ok(before), Ok(after)) if before == after => {}
                (Ok(_), Ok(_)) => changed.push(format!("{what}: its modification count changed")),
                (Err(e), _) | (_, Err(e)) => changed.push(format!(
                    "{what}: its modification count could not be read ({e})"
                )),
            }
        }
        // A key tree is built in key order.
        downloads.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(DesignCatalogs {
            downloads,
            unavailable,
            changed,
        })
    }

    /// The scripts as stored on the host, read now.
    pub fn script_definitions(&mut self) -> Result<ScriptDefinitions, DataError> {
        self.require_open()?;
        let downloads = vec![download(
            self.session.as_mut().expect("open session"),
            &[SCRIPTS],
        )?];
        let read = Self::definitions_of(&downloads)?;
        // A calculation schema built from the catalog's index takes the
        // whole catalog now read (`calc/demand.rs`).
        if let Some(whole) = downloads.into_iter().next() {
            if let Err(error) = self.adopt_scripts(whole) {
                log::warn!("the calculation schema kept the script index: {error}");
            }
        }
        Ok(read)
    }

    /// The scripts, definitions and index of the downloaded catalog `[17]`.
    fn definitions_of(downloads: &[crate::Download]) -> Result<ScriptDefinitions, DataError> {
        let tree = download_tree(downloads)?;
        let catalog = ScriptCatalog::decode(&tree)
            .map_err(|error| failure("decode server scripts", error))?;
        let definitions = tree
            .at(&[SCRIPTS, DEFINITIONS])
            .map(|members| {
                members
                    .children
                    .iter()
                    .filter_map(|node| {
                        Some((serial::decode(node.segment.0)?, node.to_owned_node()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let index = tree
            .at(&[SCRIPTS, INDEX])
            .map(|index| index.to_owned_node());
        Ok(ScriptDefinitions {
            catalog,
            definitions,
            index,
        })
    }

    /// Save the steps of `edited`, a script as the host stored it at
    /// content revision `expected` (`stored`, its definition `[17, 5, S]`
    /// as read) with its steps changed: the shared step save of
    /// `om-author`'s apply and the desktop's Script Workspace. The new
    /// definition is `stored` with `edited`'s step list and step data
    /// ([`Script::with_steps_in`]), so the name, options, UUIDs and unknown
    /// keys stay as the host has them.
    ///
    /// Refused before anything is sent: a definition of another script,
    /// and one no longer at `expected` or whose modification count (`fc`,
    /// schema-path.md 8.1; every save of the script advances it) is not the
    /// one `edited` was read with: a conflict naming the revision it is at.
    /// So a draft kept from an older read cannot be saved over a later one
    /// even when `expected` was read later. Steps the script already has
    /// are not sent: the answer is unchanged. Otherwise [`NativeProvider::replace_script_steps`] sends
    /// them via the extension, or uses the native FileMaker Server save and
    /// reads the written nodes back under the locks.
    pub fn save_script_steps(
        &mut self,
        edited: &Script,
        stored: &OwnedNode,
        expected: Revision,
    ) -> Result<authoring::Replaced, AuthoringError> {
        if serial::decode(&stored.segment) != Some(edited.id) {
            return Err(AuthoringError::NotSent(format!(
                "The stored definition is not script {}'s.",
                edited.id
            )));
        }
        let current = content_revision(stored);
        let count = |node: Option<&OwnedNode>| node.and_then(|n| n.value.clone());
        let read_with = count(
            edited
                .metadata
                .iter()
                .find(|n| n.segment == authoring::MOD_COUNT),
        );
        if current != expected || read_with != count(stored.child(authoring::MOD_COUNT)) {
            return Err(AuthoringError::Refused {
                status: status::CONFLICT,
                message: "The script is no longer at the revision it was read at: it was \
                          changed since."
                    .into(),
                current: Some(current),
            });
        }
        let definition = edited.with_steps_in(stored);
        if content_revision(&definition) == current {
            return Ok(authoring::Replaced {
                changed: false,
                revision: current,
            });
        }
        match self
            .script_step_authoring()
            .map_err(|e| AuthoringError::NotSent(e.message))?
        {
            (Authoring::Available { .. }, ScriptSave::Native) => {
                self.save_native_script_steps(edited, stored)
            }
            (Authoring::Available { .. }, ScriptSave::Extension) => {
                self.send_script_replacement(edited.id, expected, definition)
            }
            (Authoring::Denied(reason) | Authoring::Unavailable(reason), _) => {
                Err(AuthoringError::Unavailable(reason))
            }
        }
    }

    /// Replace script `script`'s steps with those of `definition` (its
    /// whole new node `[17, 5, S]`, every other child as stored), if the
    /// script is still at content revision `expected`. The host checks the
    /// account, the locks and the revision and saves the file before it
    /// answers.
    pub fn replace_script_steps(
        &mut self,
        script: u64,
        expected: Revision,
        definition: OwnedNode,
    ) -> Result<authoring::Replaced, AuthoringError> {
        match self.script_extension_authoring() {
            Ok(Authoring::Available { .. }) => {}
            Ok(Authoring::Denied(reason) | Authoring::Unavailable(reason)) => {
                return Err(AuthoringError::Unavailable(reason))
            }
            Err(e) => return Err(AuthoringError::NotSent(e.to_string())),
        }
        self.send_script_replacement(script, expected, definition)
    }

    fn send_script_replacement(
        &mut self,
        script: u64,
        expected: Revision,
        definition: OwnedNode,
    ) -> Result<authoring::Replaced, AuthoringError> {
        let request = Request::ReplaceScriptSteps(authoring::ReplaceScriptSteps {
            script,
            expected,
            definition,
        });
        match self.authoring_request(&request)? {
            Reply::Replaced(replaced) => Ok(replaced),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement script authoring.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Security(_)
            | Reply::Saved => Err(AuthoringError::OutcomeUnknown(
                "the host answered a replacement with something else".into(),
            )),
        }
    }

    /// Have the host write the file to disk now and say whether the disk
    /// holds every change: after op13 uploads (a catalog save), whose reply
    /// says nothing about the disk (schema-path.md 3.3).
    pub fn confirm_saved(&mut self) -> Result<(), AuthoringError> {
        match self.authoring_request(&Request::Save)? {
            Reply::Saved => Ok(()),
            Reply::Refused {
                status,
                message,
                current,
            } => Err(AuthoringError::Refused {
                status,
                message,
                current,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement confirming a save.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Security(_) => Err(AuthoringError::OutcomeUnknown(
                "the host answered a save with something else".into(),
            )),
        }
    }

    /// Send one authoring request. A failure before it went out is
    /// [`AuthoringError::NotSent`]; after, [`AuthoringError::OutcomeUnknown`].
    pub(crate) fn authoring_request(&mut self, request: &Request) -> Result<Reply, AuthoringError> {
        self.require_open()
            .map_err(|e| AuthoringError::NotSent(e.to_string()))?;
        let txn = self.new_transaction();
        let session = self.session.as_mut().expect("open session");
        if let Some(reason) = session.connection_lost() {
            return Err(AuthoringError::NotSent(format!(
                "the connection to the host was lost ({reason})"
            )));
        }
        let file_id = session
            .opened_file()
            .ok_or_else(|| AuthoringError::NotSent("No hosted file is open.".into()))?
            .reply
            .file_id;
        let transaction =
            authoring::build_request(txn, file_id, request).map_err(AuthoringError::NotSent)?;
        let result = session
            .perform(&transaction)
            .map_err(|e| AuthoringError::OutcomeUnknown(e.to_string()))?;
        match authoring::parse_reply(request, &result) {
            // The host wrote the change and could not confirm it is on disk.
            Ok(Reply::Refused {
                status: authoring::status::OUTCOME_UNKNOWN,
                message,
                ..
            }) => Err(AuthoringError::OutcomeUnknown(message)),
            Ok(reply) => Ok(reply),
            Err(e) => Err(AuthoringError::OutcomeUnknown(format!(
                "unreadable reply: {e}"
            ))),
        }
    }
}

/// Download the catalog at `path` into `downloads`; a failure is recorded
/// in `unavailable`, and an absent node holds nothing.
fn get(
    session: &mut om_session::Session,
    path: Vec<u64>,
    what: String,
    downloads: &mut Vec<crate::Download>,
    unavailable: &mut Vec<om_data::design_refs::Unavailable>,
) {
    match download_node(session, path.iter().map(|k| key_bytes(*k)).collect()) {
        Ok(Some(download)) => downloads.push(download),
        Ok(None) => {}
        Err(e) => unavailable.push(om_data::design_refs::Unavailable {
            path,
            what,
            reason: e.to_string(),
        }),
    }
}

/// The catalogs within the design catalog at `path` whose index `[.., 1]`
/// keeps a modification count: the schema's base tables, occurrences and
/// relationships for `[3]`, the catalog itself otherwise. The file's
/// status `[2]` has none.
fn catalog_indexes(path: &[u64], what: &str) -> Vec<(Vec<u64>, String)> {
    match path {
        [2] => Vec::new(),
        [3] => vec![
            (vec![3, 16], "the base tables".into()),
            (vec![3, 17], "the table occurrences".into()),
            (vec![3, 0xfb], "the relationships".into()),
        ],
        _ => vec![(path.to_vec(), what.to_owned())],
    }
}

/// A catalog's modification count as read: its stored bytes, `None` when
/// it has none yet, or why it could not be read.
type ModCount = Result<Option<Vec<u8>>, String>;

/// A catalog's modification count `[cat, 1, fc]` (schema-path.md 8.1), as
/// stored; `None` when it has none yet.
fn mod_count(
    session: &mut om_session::Session,
    catalog: &[u64],
) -> Result<Option<Vec<u8>>, String> {
    let mut path: Vec<Vec<u8>> = catalog.iter().map(|k| key_bytes(*k)).collect();
    path.push(vec![1]);
    path.push(vec![0xfc]);
    download_value(session, path).map_err(|e| e.to_string())
}
