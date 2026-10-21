//! Transport-level tests for MCP discovery and tool metadata.
//!
//! These tests intentionally use an unreachable Tandoor URL. Protocol
//! discovery and tool listing must remain testable without Docker or Tandoor.

use mcp_tandoor::{mcp_router, TandoorMcpServer};
use reqwest::{Client, Response};
use serde_json::{json, Value};

const MODERN_VERSION: &str = "2026-07-28";

async fn spawn_server() -> (Client, String, tokio::task::JoinHandle<()>) {
    let server = TandoorMcpServer::new_with_credentials(
        "http://127.0.0.1:9".to_string(),
        "protocol-test".to_string(),
        "protocol-test".to_string(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener
        .local_addr()
        .expect("test listener should have an address");
    let router = mcp_router(server, Vec::new());
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("test MCP server should run");
    });

    (Client::new(), format!("http://{address}/mcp"), task)
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MODERN_VERSION,
        "io.modelcontextprotocol/clientInfo": {
            "name": "protocol-test",
            "version": "1.0.0"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

async fn modern_post(client: &Client, url: &str, method: &str, params: Value) -> Response {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", MODERN_VERSION)
        .header("Mcp-Method", method)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        }));
    if let Some(name) = name {
        request = request.header("Mcp-Name", name);
    }
    request.send().await.expect("MCP request should complete")
}

async fn response_json(response: Response) -> Value {
    let text = response.text().await.expect("response body should load");
    if text.trim_start().starts_with('{') {
        return serde_json::from_str(&text).expect("response should be JSON");
    }

    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .expect("SSE response should contain a data event");
    serde_json::from_str(data).expect("SSE data should be JSON")
}

#[tokio::test]
async fn chatgpt_discovery_and_tools_are_stateless_and_annotated() {
    let (client, url, server_task) = spawn_server().await;

    let discover = modern_post(
        &client,
        &url,
        "server/discover",
        json!({ "_meta": modern_meta() }),
    )
    .await;
    assert!(discover.status().is_success());
    let discover = response_json(discover).await;
    let result = &discover["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"][0], MODERN_VERSION);
    assert!(result["supportedVersions"]
        .as_array()
        .expect("supportedVersions should be an array")
        .iter()
        .any(|version| version == "2025-11-25"));
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "tandoor"
    );
    assert!(
        result["instructions"]
            .as_str()
            .expect("instructions should be present")
            .len()
            >= 512
    );
    assert_eq!(result["ttlMs"], 300_000);
    assert_eq!(result["cacheScope"], "public");

    let list_params = json!({ "_meta": modern_meta() });
    let first =
        response_json(modern_post(&client, &url, "tools/list", list_params.clone()).await).await;
    let second = response_json(modern_post(&client, &url, "tools/list", list_params).await).await;
    let first_tools = first["result"]["tools"]
        .as_array()
        .expect("tools/list should return tools");
    let second_tools = second["result"]["tools"]
        .as_array()
        .expect("tools/list should return tools");
    assert_eq!(first_tools.len(), 18);
    assert_eq!(
        first_tools, second_tools,
        "tool order must be deterministic"
    );

    let annotation = |name: &str| {
        first_tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))["annotations"]
            .clone()
    };
    assert_eq!(annotation("search_recipes")["readOnlyHint"], true);
    assert_eq!(annotation("create_recipe")["destructiveHint"], false);
    assert_eq!(annotation("delete_meal_plan")["destructiveHint"], true);
    assert_eq!(annotation("update_pantry")["idempotentHint"], true);

    let call = modern_post(
        &client,
        &url,
        "tools/call",
        json!({
            "_meta": modern_meta(),
            "name": "search_recipes",
            "arguments": { "query": "soup", "limit": 1 }
        }),
    )
    .await;
    assert!(call.status().is_success());
    let call = response_json(call).await;
    assert_eq!(call["result"]["isError"], true);

    server_task.abort();
}

#[tokio::test]
async fn transport_rejects_bad_origin_and_protocol_mismatch() {
    let (client, url, server_task) = spawn_server().await;

    let bad_origin = client
        .post(&url)
        .header("Origin", "https://evil.example")
        .json(&json!({}))
        .send()
        .await
        .expect("origin test request should complete");
    assert_eq!(bad_origin.status(), reqwest::StatusCode::FORBIDDEN);

    let mismatch = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", MODERN_VERSION)
        .header("Mcp-Method", "server/discover")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2025-11-25",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }))
        .send()
        .await
        .expect("mismatch request should complete");
    assert_eq!(mismatch.status(), reqwest::StatusCode::BAD_REQUEST);

    server_task.abort();
}

#[tokio::test]
async fn legacy_initialize_still_succeeds_on_streamable_http() {
    let (client, url, server_task) = spawn_server().await;
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "legacy-test", "version": "1.0.0" }
            }
        }))
        .send()
        .await
        .expect("legacy initialize should complete");
    assert!(response.status().is_success());
    let response = response_json(response).await;
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");

    server_task.abort();
}
