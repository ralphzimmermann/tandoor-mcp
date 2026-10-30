mod common;

use common::DockerEnvironment;
use mcp_tandoor::server::TandoorMcpServer;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_mcp_server_initialization() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    // Set up MCP server with the token minted by scripts/test.sh or CI.
    let base_url =
        std::env::var("TANDOOR_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
    let token = std::env::var("TANDOOR_AUTH_TOKEN")
        .expect("TANDOOR_AUTH_TOKEN is required for integration tests");
    let server = TandoorMcpServer::new(base_url);
    server
        .set_global_auth_token(token)
        .await
        .expect("Should set token successfully");

    // Exercise the same startup validation that main.rs runs before it opens
    // the MCP listener, so the real Tandoor request path is covered.
    server
        .test_api_access()
        .await
        .expect("Tandoor should accept the configured API token");
    println!("MCP server initialized and validated its token against Tandoor");
}
