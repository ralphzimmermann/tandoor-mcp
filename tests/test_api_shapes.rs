//! Regression tests for Tandoor response shapes that have drifted over time.
//!
//! These fixtures mirror current production payloads (nested `created_by`, meal-plan
//! `from_date`/`to_date`, slim shopping-list foods) so serde stays compatible.

use mcp_tandoor::client::types::{
    ApiNamedReference, CookLog, CreateMealPlanRequest, MealPlan, PaginatedResponse, Recipe,
    ShoppingListEntry, Step,
};

#[test]
fn recipe_search_accepts_nested_created_by() {
    let json = r#"{
      "count": 1,
      "next": null,
      "previous": null,
      "results": [{
        "id": 1344,
        "name": "5-Minute Ketchup GF",
        "description": "desc",
        "image": null,
        "keywords": [{"id": 22, "label": "gluten-free"}],
        "working_time": 5,
        "waiting_time": 0,
        "created_by": {
          "id": 1,
          "username": "lovejump",
          "first_name": "",
          "last_name": "",
          "display_name": "lovejump",
          "is_staff": true,
          "is_superuser": true,
          "is_active": true
        },
        "created_at": "2026-10-11T12:07:46.614712+02:00",
        "updated_at": "2026-10-11T12:07:47.677656+02:00",
        "internal": true,
        "private": false,
        "servings": 4,
        "servings_text": "Makes 1 cup",
        "rating": null,
        "last_cooked": null,
        "new": false
      }]
    }"#;

    let parsed: PaginatedResponse<Recipe> = serde_json::from_str(json).expect("recipe search");
    assert_eq!(parsed.results[0].id, 1344);
    assert_eq!(parsed.results[0].created_by.as_ref().unwrap().id(), 1);
}

#[test]
fn recipe_detail_accepts_nutrition_and_recipe_properties() {
    let json = r#"{
      "id": 42,
      "name": "Property Soup",
      "description": null,
      "instructions": null,
      "servings": 2,
      "servings_text": "Makes one pot",
      "working_time": 10,
      "waiting_time": 20,
      "created_at": "2026-10-11T12:07:46Z",
      "updated_at": "2026-10-11T12:07:47Z",
      "internal": true,
      "source_url": "https://example.com/soup",
      "show_ingredient_overview": true,
      "steps": [],
      "keywords": [],
      "nutrition": {
        "id": 9,
        "calories": 250,
        "carbohydrates": 31.5,
        "fats": 8,
        "proteins": 12,
        "source": "Package labels"
      },
      "properties": [{
        "id": 4,
        "property_amount": null,
        "property_type": {
          "id": 7,
          "name": "Fiber",
          "unit": "g",
          "description": "Dietary fiber",
          "order": 1,
          "fdc_id": null
        }
      }]
    }"#;

    let recipe: Recipe = serde_json::from_str(json).expect("recipe detail");
    let nutrition = recipe.nutrition.expect("nutrition");
    assert_eq!(nutrition.carbohydrates, Some(31.5));
    assert_eq!(nutrition.source.as_deref(), Some("Package labels"));
    assert_eq!(recipe.properties[0].property_amount, None);
    assert_eq!(recipe.properties[0].property_type.name, "Fiber");
}

/// Production recipe 1647 ("Brownie GF") had a video attached to a step. Tandoor
/// serializes `step.file` as a nested `UserFile` object, which broke
/// `get_recipe_details` while the field was typed as a string.
#[test]
fn recipe_step_accepts_attached_file_object() {
    let json = r#"{
      "id": 8814,
      "name": "Grind, Mix and Shape",
      "instruction": "Grind the oats.",
      "ingredients": [],
      "instructions_markdown": "<p>Grind the oats.</p>",
      "time": 0,
      "order": 0,
      "show_as_header": false,
      "file": {
        "id": 22,
        "name": "IMG_6109.MOV",
        "file_download": "https://recipes.example/api/download-file/22/",
        "preview": "",
        "media_url": "https://assets.example/files/e361981f.MOV",
        "media_type": "video",
        "image_thumbnail": null,
        "image_gallery": null,
        "file_size_kb": 10109,
        "created_by": {"id": 1, "username": "lovejump", "display_name": "lovejump"},
        "created_at": "2026-10-24T19:30:59.947784+02:00"
      },
      "step_recipe": null,
      "step_recipe_data": null,
      "show_ingredients_table": true
    }"#;

    let parsed: Step = serde_json::from_str(json).expect("step with attached file");
    let file = parsed.file.expect("file is present");
    assert_eq!(file.id, 22);
    assert_eq!(file.name.as_deref(), Some("IMG_6109.MOV"));
    assert_eq!(file.media_type.as_deref(), Some("video"));

    let without_file = r#"{"id": 1, "name": "", "instruction": "Mix", "ingredients": [],
        "time": null, "order": 0, "file": null}"#;
    let parsed: Step = serde_json::from_str(without_file).expect("step without file");
    assert!(parsed.file.is_none());
}

#[test]
fn shopping_list_accepts_created_at_and_slim_food() {
    let json = r#"{
      "id": 846,
      "food": {
        "id": 1946,
        "name": "Onion",
        "plural_name": "Onion",
        "supermarket_category": null,
        "sourcing_info": "Available at the farmers market.",
        "sourcing_info_markdown": "<p>Available at the farmers market.</p>"
      },
      "unit": {"id": 45, "name": "tsp", "plural_name": "tsp", "description": null, "base_unit": null},
      "amount": 0.5,
      "order": 0,
      "checked": false,
      "created_by": {"id": 1, "username": "lovejump", "display_name": "lovejump"},
      "created_at": "2026-10-11T05:31:37.456837+02:00",
      "completed_at": null,
      "delay_until": null
    }"#;

    let parsed: ShoppingListEntry = serde_json::from_str(json).expect("shopping entry");
    assert_eq!(parsed.food.name, "Onion");
    assert!(!parsed.food.food_onhand);
    assert_eq!(
        parsed.food.sourcing_info.as_deref(),
        Some("Available at the farmers market.")
    );
    assert_eq!(
        parsed.food.sourcing_info_markdown.as_deref(),
        Some("<p>Available at the farmers market.</p>")
    );
    assert_eq!(parsed.created_by.as_ref().map(|user| user.id()), Some(1));
}

#[test]
fn meal_plan_accepts_from_to_dates_and_recipe_summary() {
    let json = r##"{
      "id": 108,
      "title": "",
      "recipe": {"id": 1122, "name": "Potato Wedge Salad GF", "description": "desc", "image": null},
      "servings": 4.0,
      "note": "",
      "from_date": "2026-10-11T10:00:00+02:00",
      "to_date": "2026-10-11T10:00:00+02:00",
      "meal_type": {"id": 10, "name": "Lunch/Dinner", "order": 0, "time": "15:00:00", "color": "#B55E4F", "created_by": 1},
      "created_by": 1,
      "recipe_name": "Potato Wedge Salad GF",
      "meal_type_name": "Lunch/Dinner",
      "shopping": true
    }"##;

    let parsed: MealPlan = serde_json::from_str(json).expect("meal plan");
    assert_eq!(parsed.recipe.as_ref().unwrap().id, 1122);
    assert_eq!(parsed.primary_date_label(), "2026-10-11");
    assert_eq!(parsed.created_by.id(), 1);
}

#[test]
fn meal_plan_create_serializes_nested_references() {
    let date = "2026-10-22T12:00:00Z".parse().expect("valid date");
    let request = CreateMealPlanRequest {
        recipe: Some(ApiNamedReference {
            id: 1284,
            name: "Buckwheat Porridge GF".to_string(),
        }),
        title: None,
        servings: 2.0,
        from_date: date,
        to_date: date,
        meal_type: ApiNamedReference {
            id: 10,
            name: "Breakfast".to_string(),
        },
        note: None,
    };

    let serialized = serde_json::to_value(request).expect("serialize request");
    assert_eq!(
        serialized["recipe"],
        serde_json::json!({"id": 1284, "name": "Buckwheat Porridge GF"})
    );
    assert_eq!(
        serialized["meal_type"],
        serde_json::json!({"id": 10, "name": "Breakfast"})
    );
    assert!(serialized.get("title").is_none());
    assert!(serialized.get("note").is_none());
}

/// Tandoor's `CookLogSerializer` emits `recipe` as a bare id and nullable
/// `servings`/`rating`. `get_cook_log` failed on every call while `CookLog`
/// expected a nested recipe object and a non-null `servings` (TANDOOR-MCP-3).
#[test]
fn cook_log_accepts_recipe_id_and_nullable_fields() {
    let json = r#"{
      "count": 2,
      "next": null,
      "previous": null,
      "timestamp": "2026-10-31T09:00:00.000000+01:00",
      "results": [{
        "id": 7,
        "recipe": 1647,
        "servings": 2,
        "rating": 5,
        "comment": "Great",
        "comment_markdown": "<p>Great</p>",
        "media": [],
        "created_by": {"id": 1, "username": "cook", "display_name": "Cook"},
        "created_at": "2026-10-20T18:00:00+02:00",
        "updated_at": "2026-10-20T18:00:00+02:00"
      }, {
        "id": 8,
        "recipe": 1647,
        "servings": null,
        "rating": null,
        "comment": null,
        "comment_markdown": "",
        "media": [],
        "created_by": {"id": 1, "username": "cook", "display_name": "Cook"},
        "created_at": "2026-10-22T18:00:00+02:00",
        "updated_at": "2026-10-22T18:00:00+02:00"
      }]
    }"#;

    let parsed: PaginatedResponse<CookLog> = serde_json::from_str(json).expect("cook log list");
    assert_eq!(parsed.results.len(), 2);
    assert_eq!(parsed.results[0].recipe.id(), 1647);
    assert_eq!(parsed.results[0].recipe.name(), None);
    assert_eq!(parsed.results[0].servings, Some(2));
    assert_eq!(parsed.results[1].servings, None);
    assert_eq!(parsed.results[1].rating, None);
}

#[test]
fn cook_log_still_accepts_nested_recipe_summary() {
    let json = r#"{
      "id": 9,
      "recipe": {"id": 12, "name": "Soup", "description": null, "image": null},
      "servings": 1,
      "rating": 4,
      "comment": null,
      "created_at": "2026-10-20T18:00:00Z",
      "created_by": 1
    }"#;

    let parsed: CookLog = serde_json::from_str(json).expect("cook log entry");
    assert_eq!(parsed.recipe.id(), 12);
    assert_eq!(parsed.recipe.name(), Some("Soup"));
}

/// Tandoor ignores date filters on `/api/cook-log/` and orders by primary key
/// ascending, so the client applies `days_back` itself and returns newest first.
#[test]
fn cook_log_recent_first_filters_and_sorts_locally() {
    let entry = |id: i32, created_at: &str| -> CookLog {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "recipe": 1,
            "created_at": created_at,
        }))
        .expect("cook log entry")
    };
    let entries = vec![
        entry(1, "2026-09-01T12:00:00Z"),
        entry(2, "2026-10-20T12:00:00Z"),
        entry(3, "2026-10-25T12:00:00Z"),
    ];
    let from = "2026-10-01T00:00:00Z".parse().expect("valid date");

    let recent = CookLog::recent_first(entries, Some(from));
    let ids: Vec<i32> = recent.iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![3, 2]);
}
