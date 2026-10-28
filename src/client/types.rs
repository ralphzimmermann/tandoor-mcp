//! Type definitions for the Tandoor API.
//!
//! This module contains all the data structures used for communicating with the Tandoor API,
//! including request and response types for recipes, shopping lists, meal plans, and more.
//!
//! ## Key Types
//!
//! - [`Recipe`] - Core recipe data with ingredients, steps, and metadata
//! - [`Keyword`] - Recipe categories and tags (with custom deserializer for API inconsistencies)
//! - [`ShoppingListEntry`] - Shopping list items with food, amounts, and completion status
//! - [`MealPlan`] - Scheduled meals with recipes and serving sizes
//! - [`PaginatedResponse`] - Standard API response wrapper for lists
//!
//! ## API Compatibility
//!
//! Some types include custom serialization logic to handle inconsistencies in the Tandoor API:
//! - [`Keyword`] handles both "name" and "label" fields depending on endpoint
//! - [`Recipe`] maps "created_at"/"updated_at" to "created"/"updated" fields
//! - Many fields are optional to handle varying API response formats

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Tandoor may return a bare user id or a nested user object depending on endpoint/version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserOrId {
    Id(i32),
    User(UserInfo),
}

impl UserOrId {
    pub fn id(&self) -> i32 {
        match self {
            Self::Id(id) => *id,
            Self::User(user) => user.id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: i32,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Nested recipe payloads on list endpoints are often summaries, not full recipes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeSummary {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
}

/// Tandoor returns recipe foreign keys as either a bare id or a nested summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RecipeOrId {
    Id(i32),
    Recipe(RecipeSummary),
}

impl RecipeOrId {
    pub fn id(&self) -> i32 {
        match self {
            Self::Id(id) => *id,
            Self::Recipe(recipe) => recipe.id,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Id(_) => None,
            Self::Recipe(recipe) => Some(&recipe.name),
        }
    }
}

/// OAuth2 authentication token response from Tandoor.
///
/// Contains the Bearer token used for authenticating subsequent API requests.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuthToken {
    /// The Bearer token string (format: `tda_xxxxxxxx_xxxx_xxxx_xxxx_xxxxxxxxxxxx`)
    pub token: String,
}

/// Authentication request payload for obtaining tokens.
///
/// Used with the `/api-token-auth/` endpoint to exchange credentials for a Bearer token.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuthRequest {
    /// Tandoor username
    pub username: String,
    /// Tandoor password
    pub password: String,
}

/// A complete recipe with all its components and metadata.
///
/// Represents a recipe from the Tandoor API, including ingredients, cooking steps,
/// nutritional information, and various metadata fields.
#[derive(Debug, Serialize, Deserialize)]
pub struct Recipe {
    /// Unique recipe identifier
    pub id: i32,
    /// Recipe name/title
    pub name: String,
    /// Optional recipe description
    pub description: Option<String>,
    /// Optional cooking instructions (may be in steps instead)
    pub instructions: Option<String>,
    /// Number of servings this recipe produces
    pub servings: Option<i32>,
    /// Active cooking time in minutes
    pub working_time: Option<i32>,
    /// Passive waiting time in minutes (e.g., rising, marinating)
    pub waiting_time: Option<i32>,
    /// Recipe creation timestamp
    #[serde(rename = "created_at")]
    pub created: DateTime<Utc>,
    /// Last modification timestamp
    #[serde(rename = "updated_at")]
    pub updated: DateTime<Utc>,
    /// Whether this is an internal/system recipe
    pub internal: bool,
    /// Display setting for ingredient overview
    #[serde(default)]
    pub show_ingredient_overview: Option<bool>,
    /// Associated keywords/tags for categorization
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    /// Cooking steps with ingredients and instructions
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Nutritional information if calculated
    #[serde(default)]
    pub nutrition: Option<Nutrition>,
    /// Recipe image URL or path
    #[serde(default)]
    pub image: Option<String>,
    /// User who created this recipe (id or nested user object)
    #[serde(default)]
    pub created_by: Option<UserOrId>,
    /// Original source URL if imported
    #[serde(default)]
    pub source_url: Option<String>,
    /// Explicit recipe-level properties (for example nutrition values).
    #[serde(default)]
    pub properties: Vec<Property>,
    /// Food-specific properties
    #[serde(default)]
    pub food_properties: Option<serde_json::Value>,
    /// File system path for recipe data
    #[serde(default)]
    pub file_path: Option<String>,
    /// Human-readable servings description
    #[serde(default)]
    pub servings_text: Option<String>,
    /// User rating (1-5 stars)
    #[serde(default)]
    pub rating: Option<f64>,
    /// When this recipe was last cooked
    #[serde(default)]
    pub last_cooked: Option<DateTime<Utc>>,
    /// Whether this recipe is private to the creator
    #[serde(default)]
    pub private: Option<bool>,
    /// Users/groups this recipe is shared with
    #[serde(default)]
    pub shared: Option<Vec<serde_json::Value>>,
    /// Whether this recipe is newly added
    #[serde(default)]
    pub new: Option<bool>,
}

/// Recipe keyword/tag for categorization and filtering.
///
/// Keywords are used to categorize recipes and can be hierarchical.
/// This type includes a custom deserializer to handle API inconsistencies
/// where some endpoints return "name" and others return "label".
#[derive(Debug, Serialize)]
pub struct Keyword {
    /// Unique keyword identifier
    pub id: i32,
    /// Keyword name (primary field)
    pub name: String,
    /// Alternative label field (API compatibility)
    #[serde(default)]
    pub label: Option<String>,
    /// Optional keyword description
    #[serde(default)]
    pub description: Option<String>,
    /// Creation timestamp
    #[serde(rename = "created_at", default)]
    pub created: Option<DateTime<Utc>>,
    /// Last modification timestamp
    #[serde(rename = "updated_at", default)]
    pub updated: Option<DateTime<Utc>>,
    /// Parent keyword for hierarchical organization
    #[serde(default)]
    pub parent: Option<serde_json::Value>,
    /// Number of child keywords
    #[serde(default)]
    pub numchild: Option<i32>,
    /// Full hierarchical name path
    #[serde(default)]
    pub full_name: Option<String>,
}

/// Custom deserializer for Keyword to handle API inconsistencies.
///
/// The Tandoor API returns keywords with different field names depending on the endpoint:
/// - Search endpoints use "label"
/// - Detail endpoints use "name"
impl<'de> serde::Deserialize<'de> for Keyword {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct KeywordVisitor;

        impl<'de> Visitor<'de> for KeywordVisitor {
            type Value = Keyword;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a keyword object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut id = None;
                let mut name = None;
                let mut label = None;
                let mut description = None;
                let mut created = None;
                let mut updated = None;
                let mut parent = None;
                let mut numchild = None;
                let mut full_name = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => id = Some(map.next_value()?),
                        "name" => name = Some(map.next_value()?),
                        "label" => label = Some(map.next_value()?),
                        "description" => description = map.next_value()?,
                        "created_at" => created = map.next_value()?,
                        "updated_at" => updated = map.next_value()?,
                        "parent" => parent = map.next_value()?,
                        "numchild" => numchild = map.next_value()?,
                        "full_name" => full_name = map.next_value()?,
                        _ => {
                            let _ = map.next_value::<serde_json::Value>()?;
                        }
                    }
                }

                let id = id.ok_or_else(|| de::Error::missing_field("id"))?;

                // Use name if present, otherwise use label (handles API inconsistency)
                let name = name
                    .or_else(|| label.clone())
                    .ok_or_else(|| de::Error::missing_field("name or label"))?;

                Ok(Keyword {
                    id,
                    name,
                    label,
                    description,
                    created,
                    updated,
                    parent,
                    numchild,
                    full_name,
                })
            }
        }

        deserializer.deserialize_map(KeywordVisitor)
    }
}

/// A single cooking step within a recipe.
///
/// Steps contain ingredients and instructions, and can be ordered sequentially.
#[derive(Debug, Serialize, Deserialize)]
pub struct Step {
    /// Unique step identifier
    pub id: i32,
    /// Step name/title
    pub name: String,
    /// Cooking instruction text
    pub instruction: String,
    /// Ingredients needed for this step
    pub ingredients: Vec<StepIngredient>,
    /// Time required for this step in minutes
    pub time: Option<i32>,
    /// Display order within the recipe
    pub order: i32,
    /// Media file attached to this step, or `None` when there is none.
    #[serde(default)]
    pub file: Option<StepFile>,
    /// Markdown-formatted instructions
    #[serde(default)]
    pub instructions_markdown: Option<String>,
    /// Whether to show this step as a section header
    #[serde(default)]
    pub show_as_header: Option<bool>,
    /// Reference to a sub-recipe
    #[serde(default)]
    pub step_recipe: Option<i32>,
    /// Sub-recipe data if applicable
    #[serde(default)]
    pub step_recipe_data: Option<serde_json::Value>,
    /// Whether to show ingredients in table format
    #[serde(default)]
    pub show_ingredients_table: Option<bool>,
}

/// A file attached to a recipe step (Tandoor `UserFile`), such as a video or
/// photo uploaded through the step editor.
///
/// Tandoor serializes `step.file` as this nested object, never as a bare path
/// string. A recipe with any such attachment failed to deserialize while the
/// field was typed `Option<String>` (production recipe 1647, Oct 2026), so
/// every field except `id` is optional to tolerate further drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepFile {
    pub id: i32,
    /// Original upload name, e.g. `IMG_6109.MOV`
    #[serde(default)]
    pub name: Option<String>,
    /// Authenticated download endpoint on the Tandoor origin
    #[serde(default)]
    pub file_download: Option<String>,
    /// Direct media URL (may point at external storage)
    #[serde(default)]
    pub media_url: Option<String>,
    /// `"image"`, `"video"`, or another Tandoor media category
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub file_size_kb: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StepIngredient {
    pub id: i32,
    /// None for section headers (is_header), which carry their text in `note`
    pub food: Option<Food>,
    pub unit: Option<Unit>,
    pub amount: f64,
    pub note: Option<String>,
    pub order: i32,
    pub is_header: bool,
    pub no_amount: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Food {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub plural_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Recipe represented as an id or nested summary, depending on endpoint.
    #[serde(default)]
    pub recipe: Option<RecipeOrId>,
    #[serde(default)]
    pub food_onhand: bool,
    /// Whether a configured substitute is currently on hand.
    #[serde(default)]
    pub substitute_onhand: bool,
    /// Category id or nested object depending on endpoint.
    #[serde(default)]
    pub supermarket_category: Option<serde_json::Value>,
    #[serde(default)]
    pub inherit_fields: Vec<InheritField>,
    #[serde(default)]
    pub properties: Vec<Property>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Unit {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub plural_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub base_unit: Option<String>,
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
}

/// Preference record for the authenticated Tandoor account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreference {
    pub user: UserOrId,
    pub mealplan_autoinclude_related: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InheritField {
    pub id: i32,
    pub name: String,
    pub field: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    #[serde(default)]
    pub id: Option<i32>,
    #[serde(default)]
    pub property_amount: Option<f64>,
    pub property_type: PropertyType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyType {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub fdc_id: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Nutrition {
    #[serde(default)]
    pub id: Option<i32>,
    #[serde(default)]
    pub calories: Option<f64>,
    #[serde(default)]
    pub proteins: Option<f64>,
    #[serde(default)]
    pub fats: Option<f64>,
    #[serde(default)]
    pub carbohydrates: Option<f64>,
    #[serde(default)]
    pub source: Option<String>,
}

/// An entry in a shopping list with food item and purchase details.
#[derive(Debug, Serialize, Deserialize)]
pub struct ShoppingListEntry {
    /// Unique entry identifier
    pub id: i32,
    /// Food item to purchase (Tandoor sends a slimmer food object here than elsewhere)
    pub food: ShoppingFood,
    /// Measurement unit (grams, cups, etc.)
    pub unit: Option<Unit>,
    /// Quantity to purchase
    pub amount: f64,
    /// Display order in the shopping list
    #[serde(default)]
    pub order: i32,
    /// Whether this item has been purchased
    pub checked: bool,
    /// When this entry was added
    #[serde(rename = "created_at", alias = "created", default)]
    pub created: Option<DateTime<Utc>>,
    /// When this item was marked as purchased
    #[serde(rename = "completed_at", alias = "completed", default)]
    pub completed: Option<DateTime<Utc>>,
    /// Optional delay before showing this item
    #[serde(default)]
    pub delay_until: Option<DateTime<Utc>>,
    /// User who added this entry
    #[serde(default)]
    pub created_by: Option<UserOrId>,
    /// User who marked this as completed
    #[serde(default)]
    pub completed_by: Option<UserOrId>,
    /// Set when the entry was added from a recipe or meal plan
    #[serde(default)]
    pub list_recipe: Option<i32>,
    /// Named shopping lists this entry is on
    #[serde(default)]
    pub shopping_lists: Vec<NamedRef>,
}

/// A minimal {id, name} reference to another object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedRef {
    pub id: i32,
    pub name: String,
}

/// The food object embedded in a shopping list entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct ShoppingFood {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub plural_name: Option<String>,
    /// Shopping endpoints often omit pantry state; default it when absent.
    #[serde(default)]
    pub food_onhand: bool,
    #[serde(default)]
    pub substitute_onhand: bool,
    #[serde(default)]
    pub supermarket_category: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MealPlan {
    pub id: i32,
    pub title: Option<String>,
    pub recipe: Option<RecipeSummary>,
    pub servings: f64,
    pub note: Option<String>,
    /// Legacy single-day field (older Tandoor versions).
    #[serde(default)]
    pub date: Option<NaiveDate>,
    /// Current Tandoor meal-plan window start.
    #[serde(default)]
    pub from_date: Option<DateTime<Utc>>,
    /// Current Tandoor meal-plan window end.
    #[serde(default)]
    pub to_date: Option<DateTime<Utc>>,
    pub meal_type: MealType,
    #[serde(rename = "created_at", alias = "created", default)]
    pub created: Option<DateTime<Utc>>,
    #[serde(rename = "updated_at", alias = "updated", default)]
    pub updated: Option<DateTime<Utc>>,
    pub created_by: UserOrId,
}

impl MealPlan {
    /// Prefer `from_date` when present; fall back to legacy `date`.
    pub fn primary_date_label(&self) -> String {
        if let Some(from) = self.from_date {
            return from.date_naive().to_string();
        }
        if let Some(date) = self.date {
            return date.to_string();
        }
        "unknown".to_string()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MealType {
    pub id: i32,
    pub name: String,
    pub order: i32,
    pub color: String,
    #[serde(default)]
    pub default: bool,
    pub created_by: UserOrId,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CookLog {
    pub id: i32,
    pub recipe: RecipeSummary,
    pub servings: f64,
    pub rating: Option<i32>,
    pub comment: Option<String>,
    #[serde(rename = "created_at", alias = "created")]
    pub created: DateTime<Utc>,
    pub created_by: UserOrId,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipeImport {
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipeFromSourceResponse {
    pub recipe: Option<serde_json::Value>,
    pub recipe_id: Option<i64>,
    pub images: Vec<serde_json::Value>,
    pub error: bool,
    pub msg: String,
    pub duplicates: Vec<serde_json::Value>,
}

/// Standard paginated response wrapper used by most Tandoor API endpoints.
///
/// This follows Django REST framework pagination format.
#[derive(Debug, Serialize, Deserialize)]
pub struct PaginatedResponse<T> {
    /// Total number of items across all pages
    pub count: i32,
    /// URL for the next page of results
    pub next: Option<String>,
    /// URL for the previous page of results
    pub previous: Option<String>,
    /// Items for the current page
    pub results: Vec<T>,
}

// Request types for creating/updating resources
// These types are used when sending data to the API to create or modify entities.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRecipeRequest {
    pub name: String,
    /// Tandoor uses this flag to distinguish editable recipes from externally
    /// managed recipes. MCP-created recipes are always managed in Tandoor.
    pub internal: bool,
    // Tandoor rejects null for these, so omit them when unset
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servings: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servings_text: Option<String>,
    pub working_time: i32,
    pub waiting_time: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_ingredient_overview: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nutrition: Option<NutritionRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<PropertyRequest>>,
    pub keywords: Vec<CreateKeywordRequest>,
    pub steps: Vec<CreateStepRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NutritionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
    pub calories: f64,
    pub carbohydrates: f64,
    pub fats: f64,
    pub proteins: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
    pub property_amount: Option<f64>,
    pub property_type: PropertyTypeReference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyTypeReference {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateKeywordRequest {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateStepRequest {
    // Tandoor rejects null for these, so omit them when unset
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub instruction: String,
    pub ingredients: Vec<CreateStepIngredientRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<i32>,
    pub order: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateStepIngredientRequest {
    /// None for section headers
    pub food: Option<CreateFoodRequest>,
    pub unit: Option<CreateUnitRequest>,
    pub amount: String,
    pub note: Option<String>,
    pub order: i32,
    pub is_header: bool,
    pub no_amount: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateFoodRequest {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateUnitRequest {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateShoppingListEntryRequest {
    pub food: i32,
    pub unit: Option<i32>,
    pub amount: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiNamedReference {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateMealPlanRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ApiNamedReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub servings: f64,
    /// Meal-plan window start (ISO-8601). Required by current Tandoor.
    pub from_date: DateTime<Utc>,
    /// Meal-plan window end (ISO-8601). Defaults to `from_date` when omitted by callers.
    pub to_date: DateTime<Utc>,
    pub meal_type: ApiNamedReference,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateCookLogRequest {
    pub recipe: i32,
    pub servings: i32,
    pub rating: Option<i32>,
    pub comment: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NewFoodRequest {
    pub name: String,
    pub food_onhand: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supermarket_category: Option<SupermarketCategoryRef>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SupermarketCategoryRef {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateFoodRequest {
    pub food_onhand: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateShoppingListEntryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct BulkShoppingListRequest {
    pub entries: Vec<CreateShoppingListEntryRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipeBook {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub shared: Vec<serde_json::Value>,
    pub filter: Option<serde_json::Value>,
    pub order: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipeBookEntry {
    pub id: i32,
    pub book: i32,
    pub book_content: Option<serde_json::Value>,
    pub recipe: i32,
    pub recipe_content: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Supermarket {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnitConversion {
    pub id: i32,
    pub name: Option<String>,
    pub base_amount: f64,
    pub base_unit: Option<serde_json::Value>,
    pub converted_amount: f64,
    pub converted_unit: Option<serde_json::Value>,
    pub food: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRecipeBookRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub shared: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRecipeBookEntryRequest {
    pub book: i32,
    pub recipe: i32,
}

impl StepIngredient {
    /// Food name, or "" for section headers that have no food
    pub fn food_name(&self) -> &str {
        self.food.as_ref().map(|f| f.name.as_str()).unwrap_or("")
    }
}
