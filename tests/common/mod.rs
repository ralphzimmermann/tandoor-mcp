#![allow(dead_code)]
// Each integration-test crate imports this shared module independently, so
// helpers used by sibling test files appear unused in some test binaries.

use anyhow::Result;
use mcp_tandoor::TandoorClient;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

pub struct TestEnvironment {
    pub client: TandoorClient,
}

// Global token storage to share across all tests
static SHARED_TOKEN: OnceLock<String> = OnceLock::new();

impl TestEnvironment {
    pub async fn new() -> Result<Self> {
        let base_url = std::env::var("TANDOOR_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:8080".to_string());

        let mut client = TandoorClient::new(base_url.clone());

        // Check if we have a shared token first
        if let Some(token) = SHARED_TOKEN.get() {
            println!("Using shared token from previous test");
            client.set_token(token.clone());
            return Ok(Self { client });
        }

        // Integration tests require the token minted by scripts/test.sh or CI.
        if let Ok(token) = std::env::var("TANDOOR_AUTH_TOKEN") {
            anyhow::ensure!(
                !token.trim().is_empty(),
                "TANDOOR_AUTH_TOKEN is set but empty"
            );
            println!("Using token from TANDOOR_AUTH_TOKEN environment variable");
            client.set_token(token.clone());
            let _ = SHARED_TOKEN.set(token);
            return Ok(Self { client });
        }

        anyhow::bail!(
            "TANDOOR_AUTH_TOKEN is required for integration tests; \
             run ./scripts/test.sh or provide a token created under Tandoor Settings → API"
        )
    }
}

pub struct DockerEnvironment;

impl DockerEnvironment {
    pub fn is_running() -> bool {
        // CI and developers may provide an independently managed Tandoor
        // instance instead of this repository's Compose service.
        if std::env::var("TANDOOR_TEST_EXTERNAL_SERVER").as_deref() == Ok("true") {
            return true;
        }

        let output = Command::new("docker")
            .args(["compose", "ps", "-q", "web_recipes"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        match output {
            Ok(result) => !result.stdout.is_empty(),
            Err(_) => false,
        }
    }

    pub fn ensure_running() -> Result<()> {
        if !Self::is_running() {
            anyhow::bail!(
                "Tandoor Docker services are not running. Please run: ./scripts/test.sh up"
            );
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! setup_test {
    () => {
        async {
            use $crate::common::{DockerEnvironment, TestEnvironment};

            // Ensure Docker is running
            DockerEnvironment::ensure_running().expect("Docker environment check failed");

            // Set up test environment
            TestEnvironment::new()
                .await
                .expect("Failed to create test environment")
        }
    };
}

pub fn init_test_logging() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .is_test(true)
        .try_init();
}
