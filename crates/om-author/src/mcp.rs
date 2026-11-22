//! A Model Context Protocol server over stdio for the authoring service,
//! with the official Rust SDK (`rmcp`). Each tool is one command of
//! [`crate::catalog`] with the command's parameters as its arguments; it runs
//! through the same [`Service`] as the CLI, and its result is the same
//! JSON response, as structured content (an error is a tool result marked
//! as an error, so the caller sees why).
//!
//! Protocol revisions: 2026-07-28 (stateless `server/discover` and
//! per-request metadata) and 2025-11-25 (the `initialize` handshake), as
//! the SDK implements them; no others are offered. stdout carries protocol
//! messages only.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig,
    Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;

use crate::api::{self, AuthorError, Command, ErrorKind};
use crate::catalog::{self, CommandSpec, Effect};
use crate::service::{handle_offline, Service};

/// The protocol revisions this server answers.
pub const PROTOCOL_VERSIONS: [ProtocolVersion; 2] =
    [ProtocolVersion::V_2025_11_25, ProtocolVersion::V_2026_07_28];

/// A command's tool: its title, description and parameters' schema, with
/// hints from its effect. Reads and authoring edits are idempotent and
/// stay within the file. A script run may delete records or reach other
/// systems, and running it again runs it again, so clients must not retry
/// it on their own.
fn tool(spec: &CommandSpec) -> Tool {
    let runs = spec.effect == Effect::Runs;
    // An edit overwrites a field's value; choosing the same item again
    // changes nothing more.
    let destructive = runs || spec.effect == Effect::Edits;
    let annotations = ToolAnnotations::new()
        .read_only(!spec.writes())
        .destructive(destructive)
        .idempotent(!runs)
        .open_world(runs);
    Tool::new(spec.tool, spec.description, spec.params_schema())
        .with_title(spec.title)
        .with_annotations(annotations)
}

/// The MCP server.
#[derive(Clone)]
pub struct AuthorServer {
    service: Arc<Mutex<ServiceState>>,
}

enum ServiceState {
    Ready(Box<Service>),
    Pending(Box<dyn FnMut() -> Result<Service, String> + Send>),
}

impl ServiceState {
    fn handle(&mut self, command: &Command) -> Value {
        if let Some(response) = handle_offline(command) {
            return response;
        }
        match self {
            Self::Ready(service) => service.handle(command),
            Self::Pending(make) => match make() {
                Ok(mut service) => {
                    let response = service.handle(command);
                    *self = Self::Ready(Box::new(service));
                    response
                }
                Err(message) => api::response(
                    command.name(),
                    Err(AuthorError::new(ErrorKind::InvalidRequest, message)),
                ),
            },
        }
    }
}

impl AuthorServer {
    pub fn new(service: Service) -> Self {
        Self {
            service: Arc::new(Mutex::new(ServiceState::Ready(Box::new(service)))),
        }
    }

    /// Load the target and credentials only when a command needs the file.
    /// Discovery and offline commands remain available when configuration
    /// is missing or cannot yet be read.
    pub fn new_lazy(make: impl FnMut() -> Result<Service, String> + Send + 'static) -> Self {
        Self {
            service: Arc::new(Mutex::new(ServiceState::Pending(Box::new(make)))),
        }
    }

    /// Run a tool call's command, off the async runtime: the service does
    /// blocking network work.
    async fn call(&self, command: Command) -> Result<Value, ErrorData> {
        let service = Arc::clone(&self.service);
        tokio::task::spawn_blocking(move || {
            let mut service = service
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            service.handle(&command)
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("The command failed: {e}"), None))
    }
}

impl ServerHandler for AuthorServer {
    fn get_info(&self) -> ServerConfig {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("om-author", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Authoring tools for one FileMaker file hosted by OpenMaker Server: inspect its \
                 metadata, get a script as text, then validate, plan and apply an edit with the \
                 revision it was made to; plan and create a field in a table with the table's \
                 fields revision; get a layout, then plan and place a field on it with the \
                 layout's revision. openmaker_commands lists every command and what it \
                 needs.",
            )
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&PROTOCOL_VERSIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            catalog::ALL.iter().map(|spec| tool(spec)).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        catalog::by_tool(name).map(tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(spec) = catalog::by_tool(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("There is no tool “{}”.", request.name),
                None,
            ));
        };
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let response = match spec.parse(arguments) {
            Ok(command) => self.call(command).await?,
            // Arguments that do not fit the schema are the tool's error, for
            // the caller to correct.
            Err(error) => api::response(spec.id, Err(error)),
        };
        let result = if response["ok"] == Value::Bool(true) {
            CallToolResult::structured(response)
        } else {
            CallToolResult::structured_error(response)
        };
        Ok(result.into())
    }
}

/// Serve MCP on stdin and stdout until the client closes them.
pub fn serve_stdio(service: Service) -> Result<(), String> {
    serve(AuthorServer::new(service))
}

/// Serve discovery without requiring target configuration or credentials.
/// The factory is called for the first command that needs the file; a
/// configuration error is that tool call's error, not a closed session.
pub fn serve_stdio_lazy(
    make: impl FnMut() -> Result<Service, String> + Send + 'static,
) -> Result<(), String> {
    serve(AuthorServer::new_lazy(make))
}

fn serve(server: AuthorServer) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Cannot start the MCP server: {e}"))?;
    runtime.block_on(async {
        let running = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| format!("The MCP session did not start: {e}"))?;
        running
            .waiting()
            .await
            .map_err(|e| format!("The MCP session ended with an error: {e}"))?;
        Ok(())
    })
}
