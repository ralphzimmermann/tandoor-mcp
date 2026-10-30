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

    // Simple test - just verify the server was created successfully
    println!("MCP server initialized successfully");
}
