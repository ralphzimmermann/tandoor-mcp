//! # Tandoor MCP Server
//!
//! A Model Context Protocol (MCP) server that provides tools for interacting with Tandoor,
//! a recipe management system. This server allows AI assistants to search recipes, create
//! new recipes, manage shopping lists, and more through a standardized protocol.
//!
//! ## Environment Variables
//!
//! - `TANDOOR_BASE_URL`: Tandoor server URL (default: http://localhost:8080)
//! - `TANDOOR_AUTH_TOKEN`: Existing API token (preferred; avoids login throttling)
//! - `TANDOOR_USERNAME`: Tandoor username for authentication (default: admin)  
//! - `TANDOOR_PASSWORD`: Tandoor password for authentication (default: admin)
//! - `BIND_ADDR`: Address to bind the MCP server (default: 127.0.0.1:3001)
//! - `MCP_ALLOWED_HOSTS`: Comma-separated hosts accepted by DNS-rebinding protection
//! - `MCP_ALLOWED_ORIGINS`: Comma-separated browser origins allowed to call `/mcp`
//! - `RUST_LOG`: Logging level (info, debug, trace, etc.)

use mcp_tandoor::{mcp_router, server::TandoorMcpServer};
use std::{env, net::SocketAddr};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".to_string().into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let base_url =
        env::var("TANDOOR_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
    let username = env::var("TANDOOR_USERNAME").unwrap_or_else(|_| "admin".to_string());

    let password = env::var("TANDOOR_PASSWORD").unwrap_or_else(|_| "admin".to_string());

    let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3001".to_string());
    let allowed_hosts = env::var("MCP_ALLOWED_HOSTS")
        .unwrap_or_else(|_| "localhost,127.0.0.1,::1".to_string())
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .collect();
    let allowed_origins = env::var("MCP_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
        .collect();

    // Test authentication and validate token
    tracing::info!("Validating Tandoor credentials...");
    let test_server = TandoorMcpServer::new_with_credentials(
        base_url.clone(),
        username.clone(),
        password.clone(),
    );

    let authentication_result = if let Ok(token) = env::var("TANDOOR_AUTH_TOKEN") {
        tracing::info!("Using TANDOOR_AUTH_TOKEN");
        test_server.set_global_auth_token(token).await
    } else {
        test_server
            .authenticate(username.clone(), password.clone())
            .await
    };

    if let Err(e) = authentication_result {
        tracing::error!("Authentication failed: {}", e);
        tracing::error!("Please verify:");
        tracing::error!("  - TANDOOR_BASE_URL is correct: {}", base_url);
        tracing::error!("  - TANDOOR_USERNAME is correct: {}", username);
        tracing::error!("  - TANDOOR_PASSWORD is correct");
        tracing::error!("  - Tandoor server is running and accessible");
        tracing::error!("  - Check if you're being rate limited (wait before retrying)");
        std::process::exit(1);
    }

    // Test that we can actually use the token to make an API call
    tracing::info!("Testing API access with token...");
    match test_server.test_api_access().await {
        Ok(_) => {
            tracing::info!("API access test passed");
        }
        Err(e) => {
            tracing::warn!("API access test failed: {}", e);
            tracing::warn!("This may be due to Tandoor permission configuration.");
            tracing::warn!("The server will continue, but some functionality may be limited.");
            tracing::warn!(
                "If you encounter issues, check Tandoor space permissions and user roles."
            );
        }
    }

    tracing::info!("Successfully authenticated and validated API access with Tandoor");

    // Streamable HTTP uses one endpoint for discovery, tool listing, and calls.
    let bind: SocketAddr = bind_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local_addr = listener.local_addr()?;
    let app = mcp_router(test_server, allowed_hosts, allowed_origins);

    tracing::info!("Tandoor MCP Server listening on http://{local_addr}/mcp");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!("Failed to listen for shutdown signal: {error}");
    }
    tracing::info!("Shutting down...");
}
