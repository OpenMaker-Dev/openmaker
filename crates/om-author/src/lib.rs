//! Agent-facing authoring of FileMaker solutions (`docs/authoring.md`): a
//! service with versioned JSON commands ([`api`]) that inspects a hosted
//! file's metadata, gives a script as editable text, validates and plans
//! edits against the file's names, and applies a script step replacement
//! with conflict protection, reading the result back over a new
//! connection; it plans and adds stored fields to tables the same way, and
//! runs scripts on the host with Perform Script on Server. It also
//! analyses the file's design, reporting what it read: where an object is
//! used, references whose target was not found, objects no reference was
//! found for, and script call chains, from the reference graph
//! `om_data::design_refs` builds of the catalogs it downloads. The
//! `om-author` binary offers the same commands as a CLI
//! and as an MCP server over stdio ([`mcp`]); both run through
//! [`Service`]. Each command is described once in [`catalog`]: its id, CLI
//! name, MCP tool, parameters, effect and what it needs from the host.
//!
//! OpenMaker Server writes go through its authoring extension
//! (`om_protocol::authoring`), which checks the account, the locks and
//! the revision and saves the `.fmp12` file before it answers.
//! FileMaker Server 26 accepts native saves of existing script steps.
//! Running a script is
//! Perform Script on Server, which the host runs as the configured
//! account.

mod analysis;
pub mod api;
pub mod catalog;
pub mod config;
pub mod mcp;
pub mod refs;
pub mod service;
pub mod source;
pub mod target;

pub use api::{AuthorError, Command, ErrorKind, API_VERSION};
pub use catalog::CommandSpec;
pub use service::{revision_text, Service, SOURCE_FORMAT};
pub use target::{read_password, Target, DEFAULT_PORT};
