mod common;

use common::{DockerEnvironment, TestEnvironment};
use pretty_assertions::assert_eq;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_search_recipes_no_query() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    let env = TestEnvironment::new()
        .await
        .expect("Failed to create test environment");

    // Search without query should return all recipes
    let result = env
        .client
        .search_recipes(None, Some(5), None, None, None, None, None)
        .await;

    assert!(result.is_ok(), "Search should succeed");
    let response = result.unwrap();
    assert!(response.results.len() <= 5, "Should respect limit");
}

#[tokio::test]
#[serial]
async fn test_search_recipes_with_query() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    let env = TestEnvironment::new()
        .await
        .expect("Failed to create test environment");

    // Search with a specific query
    let result = env
        .client
        .search_recipes(Some("pasta"), Some(5), None, None, None, None, None)
        .await;

    assert!(result.is_ok(), "Search should succeed");
    let response = result.unwrap();
    assert!(response.results.len() <= 5, "Should respect limit");
}

#[tokio::test]
#[serial]
async fn test_create_and_retrieve_recipe() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    let env = TestEnvironment::new()
        .await
        .expect("Failed to create test environment");

    // Create a test recipe
    let test_name = format!("Test Recipe {}", chrono::Utc::now().timestamp());
    let create_request = mcp_tandoor::client::types::CreateRecipeRequest {
        name: test_name.clone(),
        internal: true,
        description: Some("A test recipe created by integration tests".to_string()),
        servings: Some(4),
        servings_text: Some("Makes four portions".to_string()),
        working_time: 30,
        waiting_time: 15,
        source_url: Some("https://example.com/test-recipe".to_string()),
        show_ingredient_overview: Some(true),
        nutrition: Some(mcp_tandoor::client::types::NutritionRequest {
            id: None,
            calories: 400.0,
            carbohydrates: 50.0,
            fats: 10.0,
            proteins: 20.0,
            source: Some("Integration test".to_string()),
        }),
        properties: None,
        keywords: vec![mcp_tandoor::client::types::CreateKeywordRequest {
            name: "test".to_string(),
        }],
        steps: vec![
            mcp_tandoor::client::types::CreateStepRequest {
                name: Some("Preparation".to_string()),
                instruction: "Prepare all ingredients".to_string(),
                ingredients: vec![],
                time: Some(10),
                order: 1,
            },
            mcp_tandoor::client::types::CreateStepRequest {
                name: Some("Cooking".to_string()),
                instruction: "Cook everything together".to_string(),
                ingredients: vec![],
                time: Some(20),
                order: 2,
            },
        ],
    };

    let create_result = env.client.create_recipe(create_request).await;
    assert!(create_result.is_ok(), "Recipe creation should succeed");

    let created_recipe = create_result.unwrap();
    assert_eq!(created_recipe.name, test_name, "Recipe name should match");
    assert!(created_recipe.internal, "Created recipe should be internal");
    assert_eq!(created_recipe.servings, Some(4), "Servings should match");

    // Retrieve the created recipe
    let get_result = env.client.get_recipe(created_recipe.id).await;
    assert!(get_result.is_ok(), "Should retrieve recipe successfully");

    let retrieved_recipe = get_result.unwrap();
    assert_eq!(
        retrieved_recipe.id, created_recipe.id,
        "Recipe ID should match"
    );
    assert_eq!(retrieved_recipe.name, test_name, "Recipe name should match");
    assert_eq!(retrieved_recipe.steps.len(), 2, "Should have 2 steps");
    assert_eq!(
        retrieved_recipe.servings_text.as_deref(),
        Some("Makes four portions")
    );
    assert_eq!(
        retrieved_recipe.source_url.as_deref(),
        Some("https://example.com/test-recipe")
    );
    let nutrition = retrieved_recipe.nutrition.expect("nutrition should exist");
    assert_eq!(nutrition.carbohydrates, Some(50.0));
    assert_eq!(nutrition.source.as_deref(), Some("Integration test"));

    let updated = env
        .client
        .update_recipe(
            created_recipe.id,
            serde_json::json!({
                "servings_text": "Makes eight portions",
                "nutrition": {
                    "id": nutrition.id,
                    "calories": 420,
                    "carbohydrates": 55,
                    "fats": 11,
                    "proteins": 21,
                    "source": "Updated integration test"
                }
            }),
        )
        .await
        .expect("recipe update should succeed");
    assert_eq!(
        updated.servings_text.as_deref(),
        Some("Makes eight portions")
    );
    assert_eq!(
        updated
            .nutrition
            .as_ref()
            .and_then(|value| value.source.as_deref()),
        Some("Updated integration test")
    );
}

#[tokio::test]
#[serial]
async fn test_get_recipe_not_found() {
    common::init_test_logging();
    DockerEnvironment::ensure_running().expect("Docker environment not running");

    let env = TestEnvironment::new()
        .await
        .expect("Failed to create test environment");

    // Try to get a recipe with an ID that doesn't exist
    let result = env.client.get_recipe(999999).await;

    assert!(result.is_err(), "Should fail for non-existent recipe");
}
