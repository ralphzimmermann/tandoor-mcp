//! Regression tests for Tandoor response shapes that have drifted over time.
//!
//! These fixtures mirror current production payloads (nested `created_by`, meal-plan
//! `from_date`/`to_date`, slim shopping-list foods) so serde stays compatible.

use mcp_tandoor::client::types::{MealPlan, PaginatedResponse, Recipe, ShoppingListEntry};

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
fn shopping_list_accepts_created_at_and_slim_food() {
    let json = r#"{
      "id": 846,
      "food": {"id": 1946, "name": "Onion", "plural_name": "Onion", "supermarket_category": null},
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
    assert_eq!(parsed.created_by.id(), 1);
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
