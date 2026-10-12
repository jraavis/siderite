//! The MCP server: read-only tools from a fixed table, each call bounded
//! in time and result size and stopped when the client cancels it.

use super::limits::Limits;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::model::{ContentBlock, Implementation, InitializeResult, JsonObject};
use rmcp::model::{ListToolsResult, PaginatedRequestParams};
use rmcp::model::{ServerCapabilities, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// What a tool returns: a JSON value, or a message for the client.
pub(crate) type Outcome = Result<Value, String>;

/// A tool's arguments.
type Args = JsonObject;

/// A running tool call.
pub(crate) type Running = Pin<Box<dyn Future<Output = Outcome> + Send>>;

type Run = Arc<dyn Fn(JsonObject) -> Running + Send + Sync>;

/// One read-only tool.
#[derive(Clone)]
pub(crate) struct ToolSpec {
    /// Its name.
    pub name: &'static str,
    /// What it does.
    pub description: &'static str,
    /// The JSON Schema of its arguments (an object schema).
    pub input: Value,
    run: Run,
}

impl ToolSpec {
    /// A tool named `name`, run by `run`.
    pub(crate) fn new<F, Fut>(
        name: &'static str,
        description: &'static str,
        input: Value,
        run: F,
    ) -> Self
    where
        F: Fn(JsonObject) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Outcome> + Send + 'static,
    {
        let run: Run = Arc::new(move |args| Box::pin(run(args)));
        Self {
            name,
            description,
            input,
            run,
        }
    }

    fn describe(&self) -> Tool {
        let schema = match &self.input {
            Value::Object(map) => map.clone(),
            _ => JsonObject::new(),
        };
        let schema = Arc::new(schema);
        let mut tool = Tool::new(self.name, self.description, schema);
        let mut hints = ToolAnnotations::default();
        hints.read_only_hint = Some(true);
        hints.destructive_hint = Some(false);
        hints.open_world_hint = Some(false);
        tool.annotations = Some(hints);
        tool
    }
}

/// The server.
#[derive(Clone)]
pub(crate) struct Server {
    tools: Arc<Vec<ToolSpec>>,
    limits: Limits,
}

impl Server {
    /// A server offering `tools` within `limits`.
    pub(crate) fn new(tools: Vec<ToolSpec>, limits: Limits) -> Self {
        Self {
            tools: Arc::new(tools),
            limits,
        }
    }

    /// Run `tool` with `args`, bounded in time and result size.
    async fn bounded(&self, tool: &ToolSpec, args: Args) -> CallToolResult {
        let timeout = self.limits.call_timeout;
        let outcome = tokio::time::timeout(timeout, (tool.run)(args)).await;
        let value = match outcome {
            Ok(Ok(value)) => value,
            Ok(Err(message)) => return failure(&message),
            Err(_) => {
                let secs = timeout.as_secs();
                return failure(&format!("the call took longer than {secs}s"));
            }
        };
        let size = value.to_string().len();
        if size > self.limits.max_result {
            let max = self.limits.max_result;
            let msg = format!("the result is {size} bytes, over the {max}");
            let msg = format!("{msg} limit");
            return failure(&msg);
        }
        CallToolResult::structured(value)
    }
}

/// A tool-level error, redacted.
fn failure(message: &str) -> CallToolResult {
    let text = crate::report::redact_text(message);
    CallToolResult::error(vec![ContentBlock::text(text)])
}

const INSTRUCTIONS: &str = "Read-only siderite tools. They describe the \
     framework version this server ships with; none of them writes files, \
     runs commands or connects to databases.";

impl ServerHandler for Server {
    fn get_info(&self) -> InitializeResult {
        let tools = ServerCapabilities::builder().enable_tools();
        let capabilities = tools.build();
        let version = env!("CARGO_PKG_VERSION");
        let implementation = Implementation::new("siderite", version);
        InitializeResult::new(capabilities)
            .with_server_info(implementation)
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.tools.iter().map(ToolSpec::describe).collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let found = self.tools.iter().find(|t| t.name == request.name);
        let Some(tool) = found else {
            let msg = format!("unknown tool `{}`", request.name);
            return Err(ErrorData::invalid_params(msg, None));
        };
        let args = request.arguments.unwrap_or_default();
        tokio::select! {
            () = context.ct.cancelled() => {
                Err(ErrorData::internal_error("cancelled", None))
            }
            result = self.bounded(tool, args) => Ok(result.into()),
        }
    }
}
