//! Transport-level tests for MCP discovery and tool metadata.
//!
//! These tests intentionally use an unreachable Tandoor URL. Protocol
//! discovery and tool listing must remain testable without Docker or Tandoor.

use mcp_tandoor::{mcp_router, TandoorMcpServer};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const MODERN_VERSION: &str = "2026-07-28";

async fn spawn_server() -> (Client, String, tokio::task::JoinHandle<()>) {
    spawn_server_with_origins(Vec::new()).await
}

async fn spawn_server_with_origins(
    allowed_origins: Vec<String>,
) -> (Client, String, tokio::task::JoinHandle<()>) {
    let server = TandoorMcpServer::new("http://127.0.0.1:9".to_string());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener
        .local_addr()
        .expect("test listener should have an address");
    let router = mcp_router(
        server,
        vec!["localhost".to_string(), "127.0.0.1".to_string()],
        allowed_origins,
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("test MCP server should run");
    });

    (Client::new(), format!("http://{address}/mcp"), task)
}

#[tokio::test]
async fn allowlisted_browser_origin_receives_cors_headers() {
    let allowed_origin = "https://app.example";
    let (client, url, server_task) =
        spawn_server_with_origins(vec![allowed_origin.to_string()]).await;

    let preflight = client
        .request(reqwest::Method::OPTIONS, &url)
        .header("Origin", allowed_origin)
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "content-type,mcp-protocol-version",
        )
        .send()
        .await
        .expect("preflight should complete");
    assert_eq!(preflight.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        preflight
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some(allowed_origin)
    );
    assert!(preflight
        .headers()
        .get("access-control-allow-headers")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("mcp-protocol-version")));

    let response = client
        .post(&url)
        .header("Origin", allowed_origin)
        .json(&json!({}))
        .send()
        .await
        .expect("allowlisted-origin request should complete");
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some(allowed_origin)
    );

    server_task.abort();
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
        .or_else(|| params.get("uri").and_then(Value::as_str))
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
    assert!(result["capabilities"]["resources"].is_object());
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
    let expected: BTreeSet<_> = [
        "search_recipes",
        "get_recipe_details",
        "get_recipe_gallery",
        "get_recipe_image",
        "create_recipe",
        "import_recipe_from_url",
        "update_recipe",
        "find_unlinked_recipe_ingredients",
        "link_step_recipe",
        "unlink_step_recipe",
        "link_food_recipe",
        "unlink_food_recipe",
        "add_ingredient_header",
        "set_related_recipe_mealplan_inclusion",
        "set_recipe_image",
        "set_recipe_image_from_file",
        "remove_recipe_image",
        "delete_recipe",
        "get_recipe_books",
        "create_recipe_book",
        "update_recipe_book",
        "delete_recipe_book",
        "add_to_recipe_book",
        "remove_from_recipe_book",
        "get_recipe_book_entries",
        "add_to_shopping_list",
        "get_shopping_list",
        "update_shopping_list_item",
        "check_shopping_items",
        "remove_from_shopping_list",
        "clear_shopping_list",
        "add_recipe_to_shopping_list",
        "add_meal_plan_to_shopping_list",
        "get_shopping_list_recipes",
        "update_shopping_list_recipe",
        "remove_recipe_from_shopping_list",
        "get_shopping_lists",
        "create_shopping_list",
        "update_shopping_list",
        "delete_shopping_list",
        "get_supermarkets",
        "create_supermarket",
        "update_supermarket",
        "delete_supermarket",
        "get_supermarket_categories",
        "create_supermarket_category",
        "update_supermarket_category",
        "delete_supermarket_category",
        "set_food_category",
        "search_foods",
        "get_food",
        "create_food",
        "update_food",
        "update_pantry",
        "find_duplicate_foods",
        "merge_foods",
        "suggest_from_inventory",
        "get_unit_conversions",
        "get_meal_plans",
        "get_meal_plan_gallery",
        "create_meal_plan",
        "update_meal_plan",
        "delete_meal_plan",
        "get_meal_types",
        "get_keywords",
        "get_property_types",
        "get_units",
        "get_cook_log",
        "log_cooked_recipe",
    ]
    .into_iter()
    .collect();
    let actual: BTreeSet<_> = first_tools
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("every tool should have a name")
        })
        .collect();
    assert_eq!(actual, expected);
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
    assert_eq!(annotation("get_food")["readOnlyHint"], true);
    assert_eq!(annotation("create_food")["destructiveHint"], false);
    assert_eq!(annotation("update_food")["idempotentHint"], true);
    assert_eq!(annotation("update_pantry")["idempotentHint"], true);
    assert_eq!(annotation("merge_foods")["destructiveHint"], true);
    assert_eq!(annotation("get_recipe_books")["readOnlyHint"], true);
    assert_eq!(annotation("get_property_types")["readOnlyHint"], true);
    assert_eq!(
        annotation("find_unlinked_recipe_ingredients")["readOnlyHint"],
        true
    );
    assert_eq!(annotation("link_food_recipe")["destructiveHint"], false);
    assert_eq!(annotation("unlink_food_recipe")["destructiveHint"], true);
    assert_eq!(annotation("add_ingredient_header")["idempotentHint"], false);

    let tool = |name: &str| {
        first_tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))
    };
    assert!(tool("create_food")["inputSchema"]["properties"]["sourcing_info"].is_object());
    assert!(tool("update_food")["inputSchema"]["properties"]["sourcing_info"].is_object());
    assert!(tool("create_food")["inputSchema"]["properties"]
        .get("sourcing_info_markdown")
        .is_none());

    let gallery = first_tools
        .iter()
        .find(|tool| tool["name"] == "get_recipe_gallery")
        .expect("gallery tool should be present");
    assert_eq!(
        gallery["_meta"]["ui"]["resourceUri"],
        "ui://tandoor/recipe-gallery-v12.html"
    );
    let meal_gallery = first_tools
        .iter()
        .find(|tool| tool["name"] == "get_meal_plan_gallery")
        .expect("meal-plan gallery tool should be present");
    assert_eq!(
        meal_gallery["_meta"]["ui"]["resourceUri"],
        "ui://tandoor/meal-plan-gallery-v16.html"
    );

    let resources = response_json(
        modern_post(
            &client,
            &url,
            "resources/list",
            json!({ "_meta": modern_meta() }),
        )
        .await,
    )
    .await;
    assert_eq!(
        resources["result"]["resources"][0]["uri"],
        "ui://tandoor/recipe-gallery-v12.html"
    );
    assert_eq!(
        resources["result"]["resources"][1]["uri"],
        "ui://tandoor/meal-plan-gallery-v16.html"
    );

    let resource = response_json(
        modern_post(
            &client,
            &url,
            "resources/read",
            json!({
                "_meta": modern_meta(),
                "uri": "ui://tandoor/recipe-gallery-v12.html"
            }),
        )
        .await,
    )
    .await;
    assert_eq!(
        resource["result"]["contents"][0]["mimeType"],
        "text/html;profile=mcp-app"
    );
    assert!(resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("widget should contain HTML")
        .contains("tandoor-recipe-gallery"));
    assert!(resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("widget should contain HTML")
        .contains("protocolVersion: MCP_APPS_PROTOCOL_VERSION"));
    assert!(resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("widget should contain HTML")
        .contains("openai:set_globals"));
    assert!(resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("widget should contain HTML")
        .contains("setTimeout(initialize"));
    assert!(resource["result"]["contents"][0]["_meta"]["ui"]["csp"]["resourceDomains"].is_array());

    let meal_resource = response_json(
        modern_post(
            &client,
            &url,
            "resources/read",
            json!({
                "_meta": modern_meta(),
                "uri": "ui://tandoor/meal-plan-gallery-v16.html"
            }),
        )
        .await,
    )
    .await;
    assert_eq!(
        meal_resource["result"]["contents"][0]["mimeType"],
        "text/html;profile=mcp-app"
    );
    assert!(meal_resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("meal-plan widget should contain HTML")
        .contains("tandoor-meal-plan-gallery"));
    assert!(meal_resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("meal-plan widget should contain HTML")
        .contains("protocolVersion: MCP_APPS_PROTOCOL_VERSION"));
    assert!(meal_resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("meal-plan widget should contain HTML")
        .contains("openai:set_globals"));
    assert!(meal_resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("meal-plan widget should contain HTML")
        .contains("setTimeout(initialize"));
    assert!(meal_resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("meal-plan widget should contain HTML")
        .contains("ui/notifications/tool-input"));

    // A client holding a stale cached tool descriptor must still get the current widget.
    let stale_resource = response_json(
        modern_post(
            &client,
            &url,
            "resources/read",
            json!({
                "_meta": modern_meta(),
                "uri": "ui://tandoor/meal-plan-gallery-v8.html"
            }),
        )
        .await,
    )
    .await;
    assert_eq!(
        stale_resource["result"]["contents"][0]["uri"],
        "ui://tandoor/meal-plan-gallery-v8.html"
    );
    assert_eq!(
        stale_resource["result"]["contents"][0]["text"],
        meal_resource["result"]["contents"][0]["text"]
    );
    let unknown_resource = response_json(
        modern_post(
            &client,
            &url,
            "resources/read",
            json!({
                "_meta": modern_meta(),
                "uri": "ui://tandoor/other.html"
            }),
        )
        .await,
    )
    .await;
    assert!(unknown_resource["error"]["message"]
        .as_str()
        .expect("unknown resource should return an error")
        .contains("Unknown resource URI"));

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

    let bad_host = client
        .post(&url)
        .header("Host", "evil.example")
        .json(&json!({}))
        .send()
        .await
        .expect("host test request should complete");
    assert_eq!(bad_host.status(), reqwest::StatusCode::FORBIDDEN);

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
