//! HTTP transport wiring for the MCP server.
//!
//! MCP 2026-07-28 uses a single, stateless-capable Streamable HTTP endpoint.
//! This module keeps that transport setup reusable so protocol tests do not
//! need to start or authenticate against a real Tandoor instance.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{
        header::{
            ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ORIGIN, VARY,
        },
        Method, StatusCode,
    },
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
/// `allowed_hosts` contains hostnames or `host:port` authorities accepted by
/// rmcp's DNS-rebinding protection.
///
/// `allowed_origins` contains exact serialized Origin values. Requests without
/// an Origin header are accepted because non-browser MCP clients, including
/// ChatGPT's connector, normally omit it.
pub fn mcp_router(
    server: TandoorMcpServer,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
) -> Router {
    let config = StreamableHttpServerConfig::default().with_json_response(true);
    // rmcp treats an explicitly empty host allowlist as "allow all". Keep its
    // secure loopback defaults when configuration is empty or whitespace-only.
    let config = if allowed_hosts.is_empty() {
        config
    } else {
        config.with_allowed_hosts(allowed_hosts)
    };
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
    let origin = request.headers().get(ORIGIN).cloned();
    if let Some(origin) = origin.as_ref() {
        let origin = origin.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
        if !allowed_origins.iter().any(|allowed| allowed == origin) {
            return Err(StatusCode::FORBIDDEN);
        }
    }

    let mut response = if request.method() == Method::OPTIONS && origin.is_some() {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .expect("static CORS response must be valid")
    } else {
        next.run(request).await
    };

    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            "GET, POST, DELETE, OPTIONS"
                .parse()
                .expect("static CORS methods must be valid"),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            "authorization, content-type, accept, mcp-protocol-version, mcp-session-id, mcp-method, mcp-name"
                .parse()
                .expect("static CORS headers must be valid"),
        );
        headers.insert(
            ACCESS_CONTROL_EXPOSE_HEADERS,
            "mcp-session-id"
                .parse()
                .expect("static exposed headers must be valid"),
        );
        headers.insert(VARY, "Origin".parse().expect("static Vary must be valid"));
    }

    Ok(response)
}
