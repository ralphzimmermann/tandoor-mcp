mod common;

use common::{DockerEnvironment, TestEnvironment};
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_authentication_success() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    let env = TestEnvironment::new().await;
    assert!(
        env.is_ok(),
        "Should configure the client with a valid API token"
    );

    let env = env.unwrap();
    assert!(
        env.client.is_authenticated(),
        "Client should be authenticated"
    );
    assert!(
        env.client.get_token().is_some(),
        "Should have authentication token"
    );
}

#[tokio::test]
#[serial]
async fn test_invalid_token_is_rejected_by_tandoor() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    let base_url =
        std::env::var("TANDOOR_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());

    let mut client = mcp_tandoor::TandoorClient::new(base_url);
    client.set_token("tda_invalid_test_token".to_string());
    let error = client
        .get_keywords()
        .await
        .expect_err("Tandoor should reject an invalid API token");

    // Distinguish an auth rejection from Tandoor simply being unreachable.
    // Tandoor's OAuth2 layer answers an unknown Bearer token with 403
    // ("Authentication credentials were not provided or access is
    // forbidden"); a missing header yields 401.
    let message = error.to_string();
    assert!(
        message.contains("401") || message.contains("403"),
        "Expected an HTTP 401/403 from Tandoor, got: {message}"
    );
}

#[tokio::test]
#[serial]
async fn test_token_persistence() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    let env = TestEnvironment::new()
        .await
        .expect("Failed to create test environment");

    // Should be authenticated via shared token or environment variable
    assert!(env.client.is_authenticated(), "Should be authenticated");
    let token = env.client.get_token().map(|t| t.to_string());
    assert!(token.is_some(), "Should have authentication token");

    // Create a second client and verify token sharing works
    let env2 = TestEnvironment::new()
        .await
        .expect("Failed to create second test environment");

    assert!(
        env2.client.is_authenticated(),
        "Second client should be authenticated"
    );
    let token2 = env2.client.get_token().map(|t| t.to_string());
    assert!(token2.is_some(), "Second client should have token");

    // Both clients should have the same token (shared)
    assert_eq!(token, token2, "Both clients should share the same token");
}
