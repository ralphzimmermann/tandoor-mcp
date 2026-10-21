//! HTTP transport wiring for the MCP server.
//!
//! MCP 2026-07-28 uses a single, stateless-capable Streamable HTTP endpoint.
//! This module keeps that transport setup reusable so protocol tests do not
//! need to start or authenticate against a real Tandoor instance.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header::ORIGIN, StatusCode},
    middleware::{self, Next},
    response::Response,
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

use crate::server::TandoorMcpServer;

/// Build the application router with the MCP service mounted at `POST /mcp`.
///
/// `allowed_origins` contains exact serialized Origin values. Requests without
/// an Origin header are accepted because non-browser MCP clients, including
/// ChatGPT's connector, normally omit it.
pub fn mcp_router(server: TandoorMcpServer, allowed_origins: Vec<String>) -> Router {
    let config = StreamableHttpServerConfig::default().with_json_response(true);
    let service: StreamableHttpService<TandoorMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(server.clone()),
            LocalSessionManager::default().into(),
            config,
        );

    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(
            Arc::new(allowed_origins),
            validate_origin,
        ))
}

async fn validate_origin(
    State(allowed_origins): State<Arc<Vec<String>>>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(origin) = request.headers().get(ORIGIN) {
        let origin = origin.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
        if !allowed_origins.iter().any(|allowed| allowed == origin) {
            return Err(StatusCode::FORBIDDEN);
        }
    }

    Ok(next.run(request).await)
}
