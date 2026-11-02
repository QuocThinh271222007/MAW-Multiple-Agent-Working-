//! `maw-coordination`: MAW's MCP server.
//!
//! * **Streamable HTTP** (primary): runs inside the main MAW process, bound to loopback only.
//!   Every request must carry `Authorization: Bearer <token>`; tokens are minted by MAW per agent
//!   session, so an agent's identity and role can never be self-declared. Browser requests (any
//!   `Origin` header) and non-loopback `Host` headers are rejected.
//! * **stdio**: for a user-launched standalone server (`maw mcp serve --stdio --as <identity>`).
//!
//! The tool logic lives in `maw_orchestrator::tools`; this crate is only transport + auth.

use anyhow::{Context, Result};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use maw_core::CoreError;
use maw_orchestrator::{tool_specs, Actor, Engine};
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const SERVER_NAME: &str = "maw-coordination";
/// Name under which agents see the server (tool names appear as `mcp__maw__maw_*` in Claude Code).
pub const MCP_SERVER_ID: &str = "maw";

const INSTRUCTIONS: &str = "MAW team coordination. Use these tools to read the goal and plan, create/claim/update tasks, report progress and evidence, message teammates and hand work off. \
Your identity and role are assigned by MAW. Work is complete only after verification with evidence; reporting that you are done is not enough.";

/// Where the acting identity comes from.
#[derive(Clone)]
enum ActorSource {
    /// Per-request, from the bearer token (HTTP).
    Request,
    /// Fixed by the operator who launched the server (stdio).
    Fixed(Actor),
}

#[derive(Clone)]
pub struct CoordinationHandler {
    engine: Arc<Engine>,
    source: ActorSource,
}

impl CoordinationHandler {
    fn actor(&self, ctx: &RequestContext<RoleServer>) -> Result<Actor, McpError> {
        match &self.source {
            ActorSource::Fixed(a) => Ok(a.clone()),
            ActorSource::Request => ctx
                .extensions
                .get::<http::request::Parts>()
                .and_then(|p| p.extensions.get::<Actor>().cloned())
                .ok_or_else(|| McpError::invalid_request("unauthenticated request", None)),
        }
    }
}

fn to_mcp_tools() -> Vec<Tool> {
    tool_specs()
        .into_iter()
        .map(|s| {
            let schema = match (s.schema)() {
                serde_json::Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            let mut t = Tool::new(s.name, s.description, Arc::new(schema));
            t.annotations = Some(ToolAnnotations::new().read_only(s.read_only));
            t
        })
        .collect()
}

impl ServerHandler for CoordinationHandler {
    fn get_info(&self) -> ServerConfig {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let actor = self.actor(&ctx)?;
        let role = actor.role();
        // Show each agent only the tools its role may call.
        let allowed: std::collections::HashSet<&'static str> = tool_specs()
            .into_iter()
            .filter(|s| {
                role.map(|r| s.roles.is_empty() || s.roles.contains(&r))
                    .unwrap_or(true)
            })
            .map(|s| s.name)
            .collect();
        let tools = to_mcp_tools()
            .into_iter()
            .filter(|t| allowed.contains(t.name.as_ref()))
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let actor = self.actor(&ctx)?;
        let engine = self.engine.clone();
        let name = request.name.to_string();
        let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
        // The store is synchronous SQLite: keep it off the async workers.
        let result = tokio::task::spawn_blocking(move || engine.call_tool(&actor, &name, args))
            .await
            .map_err(|e| McpError::internal_error(format!("tool task failed: {e}"), None))?;
        Ok(match result {
            Ok(v) => CallToolResult::structured(v),
            Err(e) => {
                if matches!(e, CoreError::Internal(_) | CoreError::Storage(_)) {
                    tracing::error!("tool error: {e}");
                }
                CallToolResult::structured_error(e.to_json())
            }
        }
        .into())
    }
}

/// A running HTTP server. Drop does not stop it; call [`McpHttpServer::stop`].
pub struct McpHttpServer {
    pub addr: SocketAddr,
    pub url: String,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl McpHttpServer {
    pub async fn stop(self) {
        self.cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.task).await;
    }
}

async fn require_bearer(engine: Arc<Engine>, mut req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .map(str::to_owned);
    let actor = match token {
        Some(t) => {
            let eng = engine.clone();
            tokio::task::spawn_blocking(move || eng.authenticate(&t))
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    match actor {
        Some(a) => {
            req.extensions_mut().insert(a);
            next.run(req).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            [(http::header::WWW_AUTHENTICATE, "Bearer realm=\"maw\"")],
            "invalid or missing MAW agent token",
        )
            .into_response(),
    }
}

/// Start the HTTP server. `bind` must be a loopback address (use port 0 for an ephemeral port).
pub async fn serve_http(engine: Arc<Engine>, bind: SocketAddr) -> Result<McpHttpServer> {
    if !bind.ip().is_loopback() {
        anyhow::bail!("refusing to bind the MCP server to non-loopback address {bind}; network exposure is disabled");
    }
    let cancel = CancellationToken::new();
    let handler = CoordinationHandler {
        engine: engine.clone(),
        source: ActorSource::Request,
    };
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(cancel.child_token())
        // loopback Host headers only (default) and reject any browser Origin outright
        .enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let eng = engine.clone();
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(move |req, next| {
            require_bearer(eng.clone(), req, next)
        }));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    let addr = listener.local_addr()?;
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { token.cancelled().await })
            .await;
    });
    Ok(McpHttpServer {
        addr,
        url: format!("http://{addr}/mcp"),
        cancel,
        task,
    })
}

/// Serve over stdio as a fixed, operator-chosen identity. Blocks until the client disconnects.
pub async fn serve_stdio(engine: Arc<Engine>, actor: Actor) -> Result<()> {
    use rmcp::ServiceExt;
    let handler = CoordinationHandler {
        engine,
        source: ActorSource::Fixed(actor),
    };
    let service = handler
        .serve(rmcp::transport::stdio())
        .await
        .context("starting stdio MCP server")?;
    service.waiting().await?;
    Ok(())
}
