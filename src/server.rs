//! # Tandoor MCP Server Implementation
//!
//! This module implements a Model Context Protocol (MCP) server that exposes Tandoor
//! functionality as standardized tools for AI assistants. The server handles authentication,
//! recipe management, shopping lists, meal planning, and more.
//!
//! ## Key Features
//!
//! - **Recipe Management**: Search, create, retrieve, and manage recipes
//! - **Shopping Lists**: Add items, manage shopping lists, check off items
//! - **Meal Planning**: Plan meals and add to shopping lists
//! - **Food & Ingredient Search**: Find foods and ingredients in the database
//! - **Unit Management**: Get available measurement units
//! - **Keyword Management**: Get and search recipe keywords
//!
//! ## Authentication
//!
//! The server supports both credential-based authentication and pre-set tokens
//! to work around Tandoor's strict rate limiting (10 auth requests per day).

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde_json::json;
use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::Mutex;

use crate::domain::shopping::{
    name_matches, parse_shopping_text, resolve_unit_name, ParsedShoppingItem,
};
use crate::{client::TandoorClient, thumbnail_service::ThumbnailService};

// Preserve the existing text-only tool result format while using rmcp's current name.
use rmcp::model::ContentBlock as Content;

/// Server-wide guidance returned by both legacy initialization and modern discovery.
///
/// The opening paragraph is deliberately self-contained because ChatGPT gives the
/// first 512 characters of MCP instructions special prominence.
pub const SERVER_INSTRUCTIONS: &str = "This server manages one shared Tandoor Recipes kitchen; it is not the user's personal ChatGPT data. Read tools do not change data. Write tools change the shared kitchen. Always confirm before destructive tools. Look up identifiers before writing: use search_recipes, search_foods, get_recipe_books, get_shopping_lists, get_supermarkets, get_meal_types, and get_property_types as appropriate. Dates use YYYY-MM-DD. Never invent identifiers. For recipe-as-ingredient cleanup, call find_unlinked_recipe_ingredients first, explain that a step embed is local while a Food link is global, and confirm the user's choice before linking. Recipe tools search, inspect, create, import, update, delete, and manage photos and books. When a user asks for a meal-plan summary with photos, call get_meal_plan_gallery so the complete card gallery and authoritative thumbnails render in one inline component; do not recreate that image layout in prose. Whenever you present one or more recipes to the user (for example after search_recipes), call get_recipe_gallery with their ids before answering so the photos render inline, even if the user did not ask for images; skip it only for has_photo: false cleanup searches or when the user asks for text only. After a gallery tool, write a normal Markdown summary with recipe names as links, but include no images of any kind (no Markdown images, image lists, carousels, or web-searched or stock photos) and do not call get_recipe_image or get_recipe_details for photos, because the component already shows the authoritative recipe photos. Use get_recipe_image with variant original only when the user asks for full size; never substitute another image. Render recipe names as clickable Markdown links using `recipe_url`. Shopping tools support plain text items, named lists, recipes, meal plans, stores, and aisles. Pantry tools update availability and find or merge duplicate foods. Meal-plan tools read, create, update, and delete plans. Recipe photos: set_recipe_image takes a public HTTPS image URL; set_recipe_image_from_file takes a photo attached to the chat (uploaded by the user or generated here): pass the attachment itself as the image argument and ChatGPT fills in the file object; never pass a /mnt/data path, sandbox: URL, or hand-built object, and never tell the user an attachment cannot be used. Both accept JPEG, PNG, WebP, or GIF up to 20 MB. Composite operations can partially succeed because Tandoor does not provide transactions; inspect returned errors and resulting state.";

// Parameter structs for tools
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchRecipesParams {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<i32>,
    /// Keyword IDs to filter by (OR logic)
    #[serde(default)]
    pub keywords: Option<Vec<i64>>,
    /// Food IDs to filter by (OR logic)
    #[serde(default)]
    pub foods: Option<Vec<i64>>,
    /// Maximum total cooking time in minutes
    #[serde(default)]
    pub max_cooking_time: Option<i64>,
    /// Minimum rating (0-5)
    #[serde(default)]
    pub min_rating: Option<i64>,
    /// Return a random recipe
    #[serde(default)]
    pub random: Option<bool>,
    /// true = only recipes with a photo, false = only recipes missing a photo
    #[serde(default)]
    pub has_photo: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRecipeDetailsParams {
    pub id: i32,
    #[serde(default)]
    pub servings: Option<i32>,
    /// Which image representation to attach. Thumbnails are the default.
    #[serde(default)]
    pub image: RecipeImageVariant,
}

#[derive(
    Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RecipeImageVariant {
    #[default]
    Thumbnail,
    Original,
    None,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRecipeGalleryParams {
    /// Recipe identifiers to show, in display order.
    pub recipe_ids: Vec<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRecipeImageParams {
    pub recipe_id: i32,
    #[serde(default)]
    pub variant: RecipeImageVariant,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateRecipeParams {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Legacy single-step fallback. Produces one step with no ingredients — prefer `steps`.
    #[serde(default)]
    pub instructions: Option<String>,
    /// Structured recipe steps with ingredients. Takes precedence over `instructions` when both are given.
    #[serde(default)]
    pub steps: Option<Vec<RecipeStepInput>>,
    #[serde(default)]
    pub servings: Option<i32>,
    /// Human-readable yield, for example "Makes 1 cup".
    #[serde(default)]
    pub servings_text: Option<String>,
    #[serde(default)]
    pub prep_time: Option<i32>,
    #[serde(default)]
    pub cook_time: Option<i32>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub show_ingredient_overview: Option<bool>,
    #[serde(default)]
    pub nutrition: Option<NutritionInput>,
    /// Recipe-level properties. Call get_property_types before setting these.
    #[serde(default)]
    pub properties: Option<Vec<RecipePropertyInput>>,
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct NutritionInput {
    #[serde(default)]
    pub calories: Option<f64>,
    #[serde(default)]
    pub carbohydrates: Option<f64>,
    #[serde(default)]
    pub fats: Option<f64>,
    #[serde(default)]
    pub proteins: Option<f64>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct RecipePropertyInput {
    /// Existing property type name or ID from get_property_types.
    pub property_type: NameOrId,
    /// Property amount; omit to store an unspecified amount.
    #[serde(default)]
    pub amount: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecipeStepInput {
    /// Instruction text for this step
    pub instruction: String,
    /// Step title (optional; omit for none)
    #[serde(default)]
    pub name: Option<String>,
    /// Ingredients used in this step (optional)
    #[serde(default)]
    pub ingredients: Option<Vec<RecipeStepIngredientInput>>,
    /// Time for this step in minutes (optional; omit for none)
    #[serde(default)]
    pub time: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecipeStepIngredientInput {
    /// Ingredient name. Reuses an existing Tandoor food with this name, or creates a new one.
    /// Omit only when `header` is set.
    #[serde(default)]
    pub food: Option<String>,
    /// Section header text (e.g. "For the sauce") shown as a heading in the ingredient
    /// list. A header entry has no food, unit, or amount.
    #[serde(default)]
    pub header: Option<String>,
    /// Unit name (e.g. "cup", "g"). Reuses an existing unit or creates a new one. Omit for unitless ingredients.
    #[serde(default)]
    pub unit: Option<String>,
    /// Quantity of the ingredient. Omit for ingredients without an amount (e.g. "salt to taste").
    #[serde(default)]
    pub amount: Option<f64>,
    /// Optional free-text note (e.g. "finely chopped")
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StepUpdateInput {
    /// Which step to change: its `step_number` from get_recipe_details (1-based, numbered
    /// as the recipe was before this call)
    pub step_number: usize,
    /// New instruction text for this step (replaces the old text)
    #[serde(default)]
    pub instruction: Option<String>,
    /// New step title
    #[serde(default)]
    pub name: Option<String>,
    /// New time for this step in minutes
    #[serde(default)]
    pub time: Option<i32>,
    /// Ingredients to add to this step. Existing ingredients are kept.
    #[serde(default)]
    pub add_ingredients: Option<Vec<RecipeStepIngredientInput>>,
    /// Food names of ingredients to remove from this step (case-insensitive exact match)
    #[serde(default)]
    pub remove_ingredients: Option<Vec<String>>,
    /// Change existing ingredients in this step in place (amount, unit, note, food), keeping
    /// everything you don't mention.
    #[serde(default)]
    pub update_ingredients: Option<Vec<IngredientEdit>>,
    /// Replace ALL of this step's ingredients with this list (other steps are unaffected).
    /// Cannot be combined with add/remove/update_ingredients.
    #[serde(default)]
    pub replace_ingredients: Option<Vec<RecipeStepIngredientInput>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IngredientEdit {
    /// Food name of the ingredient to change (case-insensitive), or a header's text
    pub food: String,
    /// New amount (also clears "no amount")
    #[serde(default)]
    pub amount: Option<f64>,
    /// true = no amount (e.g. "to taste"); the stored amount is ignored
    #[serde(default)]
    pub no_amount: Option<bool>,
    /// New unit name; "" removes the unit
    #[serde(default)]
    pub unit: Option<String>,
    /// New note; "" clears it
    #[serde(default)]
    pub note: Option<String>,
    /// Swap the food for a different one (reused or created by name)
    #[serde(default)]
    pub new_food: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MoveStepInput {
    /// step_number of the step to move
    pub step_number: usize,
    /// Place it after this step_number (0 = at the beginning)
    pub after_step: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddStepInput {
    /// Insert after this step_number (0 = at the beginning). Omit to append at the end.
    #[serde(default)]
    pub after_step: Option<usize>,
    #[serde(flatten)]
    pub step: RecipeStepInput,
}

/// An ingredient entry is either a food or a section header, never both.
fn validate_ingredient(ing: &RecipeStepIngredientInput) -> Result<(), String> {
    let has_food = ing.food.as_deref().is_some_and(|f| !f.trim().is_empty());
    match &ing.header {
        Some(header) if header.trim().is_empty() => Err("header text cannot be empty".to_string()),
        Some(header) if has_food || ing.unit.is_some() || ing.amount.is_some() => Err(format!(
            "header '{header}' cannot also have a food, unit, or amount — add the ingredient as a separate entry"
        )),
        Some(_) => Ok(()),
        None if has_food => Ok(()),
        None => Err("each ingredient needs a `food` (or a `header` for a section heading)".to_string()),
    }
}

fn validate_steps(steps: &[RecipeStepInput]) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        for ing in step.ingredients.iter().flatten() {
            validate_ingredient(ing).map_err(|e| format!("step {}: {e}", i + 1))?;
        }
    }
    Ok(())
}

/// Convert Tandoor's source-import preview into a recipe create payload.
///
/// The preview contains UI-only fields that `POST /api/recipe/` does not
/// accept. It also marks keywords that the user chose not to import.
fn prepare_source_import_recipe(
    mut recipe: serde_json::Value,
) -> Result<(serde_json::Value, Option<String>), String> {
    let object = recipe
        .as_object_mut()
        .ok_or_else(|| "Tandoor returned a non-object recipe preview".to_string())?;
    // Source previews may mark recipes as externally managed even when no
    // external storage is configured. Recipes persisted by this server belong
    // to Tandoor and must remain fully editable there.
    object.insert("internal".to_string(), serde_json::Value::Bool(true));
    let image_url = object
        .remove("image_url")
        .and_then(|value| value.as_str().map(str::to_owned));

    if let Some(keywords) = object
        .get_mut("keywords")
        .and_then(serde_json::Value::as_array_mut)
    {
        keywords.retain_mut(|keyword| {
            let Some(keyword) = keyword.as_object_mut() else {
                return false;
            };
            keyword
                .remove("import_keyword")
                .and_then(|value| value.as_bool())
                .unwrap_or(true)
        });
    }

    let has_name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|name| !name.trim().is_empty());
    if !has_name {
        return Err("Tandoor returned a recipe preview without a name".to_string());
    }

    Ok((recipe, image_url))
}

/// Assumes `ing` passed validate_ingredient.
fn build_ingredient_request(
    ing: RecipeStepIngredientInput,
    order: i32,
) -> crate::client::types::CreateStepIngredientRequest {
    if let Some(header) = ing.header {
        return crate::client::types::CreateStepIngredientRequest {
            food: None,
            unit: None,
            amount: "0".to_string(),
            note: Some(header.trim().to_string()),
            order,
            is_header: true,
            no_amount: true,
        };
    }
    crate::client::types::CreateStepIngredientRequest {
        food: ing
            .food
            .map(|name| crate::client::types::CreateFoodRequest { name }),
        unit: ing
            .unit
            .map(|name| crate::client::types::CreateUnitRequest { name }),
        amount: ing.amount.unwrap_or(0.0).to_string(),
        note: ing.note,
        order,
        is_header: false,
        no_amount: ing.amount.is_none(),
    }
}

/// Targeted step edits resolved against a recipe's current steps.
#[derive(Debug)]
pub struct StepPlan {
    /// PATCH bodies for /api/step/{id}/
    pub patches: Vec<(i32, serde_json::Value)>,
    /// New `steps` list for the recipe PATCH when steps are added or removed. Existing
    /// steps are referenced by ID only, so Tandoor keeps their content.
    pub steps_body: Option<serde_json::Value>,
}

/// Validates step_updates / add_steps / remove_steps / move_steps against the recipe's
/// existing steps and turns them into API requests. Nothing is sent if any part is invalid.
pub fn plan_step_changes(
    existing: &[crate::client::types::Step],
    updates: Vec<StepUpdateInput>,
    adds: Vec<AddStepInput>,
    removes: Vec<usize>,
    moves: Vec<MoveStepInput>,
) -> Result<StepPlan, String> {
    let ordered = ordered_steps(existing);
    let count = ordered.len();
    let check = |n: usize, what: &str| {
        if n == 0 || n > count {
            Err(format!(
                "{what}: step {n} does not exist (recipe has {count} steps, numbered from 1)"
            ))
        } else {
            Ok(())
        }
    };
    let check_after = |after: usize, what: &str| {
        if after > count {
            Err(format!(
                "{what}: after_step {after} does not exist (recipe has {count} steps; use 0 for the beginning)"
            ))
        } else {
            Ok(())
        }
    };

    for n in &removes {
        check(*n, "remove_steps")?;
    }
    for add in &adds {
        if let Some(after) = add.after_step {
            check_after(after, "add_steps")?;
        }
        validate_steps(std::slice::from_ref(&add.step)).map_err(|e| format!("add_steps: {e}"))?;
    }
    let mut moved = std::collections::HashSet::new();
    for m in &moves {
        check(m.step_number, "move_steps")?;
        check_after(m.after_step, "move_steps")?;
        if removes.contains(&m.step_number) {
            return Err(format!(
                "step {} is in both move_steps and remove_steps",
                m.step_number
            ));
        }
        if !moved.insert(m.step_number) {
            return Err(format!(
                "move_steps: step {} is listed more than once",
                m.step_number
            ));
        }
    }

    let mut patches = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for update in updates {
        let n = update.step_number;
        check(n, "step_updates")?;
        if !seen.insert(n) {
            return Err(format!("step_updates: step {n} is listed more than once"));
        }
        if removes.contains(&n) {
            return Err(format!("step {n} is in both step_updates and remove_steps"));
        }
        let step = ordered[n - 1];

        let mut body = serde_json::Map::new();
        if let Some(v) = update.instruction {
            body.insert("instruction".to_string(), json!(v));
        }
        if let Some(v) = update.name {
            body.insert("name".to_string(), json!(v));
        }
        if let Some(v) = update.time {
            body.insert("time".to_string(), json!(v));
        }

        let adding = update.add_ingredients.unwrap_or_default();
        let removing = update.remove_ingredients.unwrap_or_default();
        let editing = update.update_ingredients.unwrap_or_default();
        for ing in &adding {
            validate_ingredient(ing).map_err(|e| format!("step {n}: {e}"))?;
        }
        let labels = || -> Vec<String> { step.ingredients.iter().map(ingredient_label).collect() };

        if let Some(replacement) = update.replace_ingredients {
            if !adding.is_empty() || !removing.is_empty() || !editing.is_empty() {
                return Err(format!(
                    "step {n}: replace_ingredients cannot be combined with add/remove/update_ingredients"
                ));
            }
            for ing in &replacement {
                validate_ingredient(ing).map_err(|e| format!("step {n}: {e}"))?;
            }
            let ings: Vec<_> = replacement
                .into_iter()
                .enumerate()
                .map(|(i, ing)| build_ingredient_request(ing, i as i32))
                .collect();
            body.insert("ingredients".to_string(), json!(ings));
        } else if !adding.is_empty() || !removing.is_empty() || !editing.is_empty() {
            let mut kept: Vec<_> = step.ingredients.iter().collect();
            kept.sort_by_key(|ing| (ing.order, ing.id));
            let matches = |kept: &[&crate::client::types::StepIngredient], name: &str| {
                let target = name.trim().to_lowercase();
                kept.iter()
                    .enumerate()
                    .filter(|(_, ing)| ingredient_label(ing).trim().to_lowercase() == target)
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>()
            };

            for name in &removing {
                let found = matches(&kept, name);
                if found.is_empty() {
                    return Err(format!(
                        "step {n} has no ingredient '{name}'. Its ingredients are: {:?}",
                        labels()
                    ));
                }
                for i in found.into_iter().rev() {
                    kept.remove(i);
                }
            }

            let mut changes: Vec<serde_json::Map<String, serde_json::Value>> =
                vec![serde_json::Map::new(); kept.len()];
            for edit in editing {
                let found = matches(&kept, &edit.food);
                let i = match found.as_slice() {
                    [i] => *i,
                    [] => {
                        return Err(format!(
                            "step {n} has no ingredient '{}' to update. Its ingredients are: {:?}",
                            edit.food,
                            labels()
                        ))
                    }
                    _ => {
                        return Err(format!(
                            "step {n} has {} ingredients named '{}'; use replace_ingredients to change them",
                            found.len(),
                            edit.food
                        ))
                    }
                };
                if !changes[i].is_empty() {
                    return Err(format!(
                        "step {n}: ingredient '{}' is in update_ingredients more than once",
                        edit.food
                    ));
                }
                let change = &mut changes[i];
                if kept[i].is_header
                    && (edit.amount.is_some()
                        || edit.no_amount.is_some()
                        || edit.unit.is_some()
                        || edit.new_food.is_some())
                {
                    return Err(format!(
                        "step {n}: '{}' is a section header; only its text (note) can be changed",
                        edit.food
                    ));
                }
                if edit.amount.is_some() && edit.no_amount == Some(true) {
                    return Err(format!(
                        "step {n}: '{}' can't have both an amount and no_amount",
                        edit.food
                    ));
                }
                if let Some(amount) = edit.amount {
                    change.insert("amount".to_string(), json!(amount.to_string()));
                    change.insert("no_amount".to_string(), json!(false));
                }
                if let Some(no_amount) = edit.no_amount {
                    change.insert("no_amount".to_string(), json!(no_amount));
                }
                if let Some(unit) = edit.unit {
                    let unit = unit.trim();
                    let value = if unit.is_empty() {
                        serde_json::Value::Null
                    } else {
                        json!({"name": unit})
                    };
                    change.insert("unit".to_string(), value);
                }
                if let Some(note) = edit.note {
                    if kept[i].is_header && note.trim().is_empty() {
                        return Err(format!(
                            "step {n}: header text cannot be empty (use remove_ingredients to delete it)"
                        ));
                    }
                    change.insert("note".to_string(), json!(note));
                }
                if let Some(food) = edit.new_food {
                    if food.trim().is_empty() {
                        return Err(format!("step {n}: new_food cannot be empty"));
                    }
                    change.insert("food".to_string(), json!({"name": food.trim()}));
                }
                if change.is_empty() {
                    return Err(format!(
                        "step {n}: update for ingredient '{}' has no changes",
                        edit.food
                    ));
                }
            }

            let mut ings: Vec<serde_json::Value> = kept
                .iter()
                .zip(changes)
                .enumerate()
                .map(|(i, (ing, mut change))| {
                    change.insert("id".to_string(), json!(ing.id));
                    change.insert("order".to_string(), json!(i));
                    serde_json::Value::Object(change)
                })
                .collect();
            let start = ings.len();
            ings.extend(
                adding
                    .into_iter()
                    .enumerate()
                    .map(|(i, ing)| json!(build_ingredient_request(ing, (start + i) as i32))),
            );
            body.insert("ingredients".to_string(), json!(ings));
        }

        if body.is_empty() {
            return Err(format!("step_updates: step {n} has no changes"));
        }
        patches.push((step.id, serde_json::Value::Object(body)));
    }

    let steps_body = if adds.is_empty() && removes.is_empty() && moves.is_empty() {
        None
    } else {
        let mut adds = adds;
        // Everything that lands right after original step `after` (0 = beginning):
        // moved steps first, then new ones
        let mut slot = |after: Option<usize>| {
            let mut out: Vec<serde_json::Value> = moves
                .iter()
                .filter(|m| Some(m.after_step) == after)
                .map(|m| json!({"id": ordered[m.step_number - 1].id}))
                .collect();
            let (matching, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut adds)
                .into_iter()
                .partition(|a| a.after_step == after);
            adds = rest;
            out.extend(
                build_step_requests(matching.into_iter().map(|a| a.step).collect())
                    .into_iter()
                    .map(|s| json!(s)),
            );
            out
        };
        let mut list = slot(Some(0));
        for (i, step) in ordered.iter().enumerate() {
            let n = i + 1;
            if !removes.contains(&n) && !moved.contains(&n) {
                list.push(json!({"id": step.id}));
            }
            list.extend(slot(Some(n)));
        }
        list.extend(slot(None));
        for (i, step) in list.iter_mut().enumerate() {
            step["order"] = json!(i + 1);
        }
        Some(json!(list))
    };

    Ok(StepPlan {
        patches,
        steps_body,
    })
}

/// How an ingredient is referred to by name: its food, or a header's text.
fn ingredient_label(ing: &crate::client::types::StepIngredient) -> String {
    if ing.is_header {
        ing.note.clone().unwrap_or_default()
    } else {
        ing.food_name().to_string()
    }
}

/// Builds a recipe's new keyword list from targeted adds/removes. `known` maps lowercase
/// names of keywords that already exist in Tandoor to their IDs; anything else is sent
/// by name so Tandoor creates it.
pub fn plan_keyword_changes(
    current: &[crate::client::types::Keyword],
    add: &[String],
    remove: &[String],
    known: &std::collections::HashMap<String, i32>,
) -> Result<serde_json::Value, String> {
    let lower = |s: &str| s.trim().to_lowercase();
    let mut kept: Vec<&crate::client::types::Keyword> = current.iter().collect();
    for name in remove {
        let before = kept.len();
        kept.retain(|k| lower(&k.name) != lower(name));
        if kept.len() == before {
            let names: Vec<&str> = current.iter().map(|k| k.name.as_str()).collect();
            return Err(format!(
                "recipe has no keyword '{name}'. Its keywords are: {names:?}"
            ));
        }
    }

    let mut list: Vec<serde_json::Value> = kept.iter().map(|k| json!({"id": k.id})).collect();
    let mut have: Vec<String> = kept.iter().map(|k| lower(&k.name)).collect();
    for name in add {
        let key = lower(name);
        if key.is_empty() || have.contains(&key) {
            continue;
        }
        list.push(match known.get(&key) {
            Some(id) => json!({"id": id}),
            None => json!({"name": name.trim()}),
        });
        have.push(key);
    }
    Ok(json!(list))
}

/// A recipe's steps in display order, so `step_number` (index + 1) is stable between
/// get_recipe_details and update_recipe.
fn ordered_steps(steps: &[crate::client::types::Step]) -> Vec<&crate::client::types::Step> {
    let mut ordered: Vec<_> = steps.iter().collect();
    ordered.sort_by_key(|s| (s.order, s.id));
    ordered
}

fn ingredient_view(
    ing: &crate::client::types::StepIngredient,
    scale: f64,
    client: &TandoorClient,
) -> serde_json::Value {
    if ing.is_header {
        return json!({
            "ingredient_id": ing.id,
            "header": ing.note.clone().unwrap_or_default()
        });
    }
    let food = ing.food.as_ref();
    json!({
        "ingredient_id": ing.id,
        "food_id": food.map(|food| food.id),
        "food": ing.food_name(),
        "food_recipe": food.and_then(|food| food.recipe.as_ref()).map(|recipe| json!({
            "id": recipe.id(),
            "name": recipe.name(),
            "recipe_url": client.recipe_url(recipe.id())
        })),
        "amount": if ing.no_amount { None } else { Some(ing.amount * scale) },
        "unit": ing.unit.as_ref().map(|u| &u.name),
        "note": ing.note,
        "is_header": ing.is_header,
        "no_amount": ing.no_amount
    })
}

/// Per-step view: each step with its own ingredients.
fn steps_view(
    steps: &[crate::client::types::Step],
    scale: f64,
    client: &TandoorClient,
) -> Vec<serde_json::Value> {
    ordered_steps(steps)
        .into_iter()
        .enumerate()
        .map(|(i, step)| {
            let mut ings: Vec<_> = step.ingredients.iter().collect();
            ings.sort_by_key(|ing| (ing.order, ing.id));
            let linked_name = step
                .step_recipe_data
                .as_ref()
                .and_then(|data| data.get("name"))
                .and_then(serde_json::Value::as_str);
            json!({
                "step_id": step.id,
                "step_number": i + 1,
                "name": step.name,
                "instruction": step.instruction,
                "time": step.time,
                "show_as_header": step.show_as_header,
                "show_ingredients_table": step.show_ingredients_table,
                "file": step.file.as_ref().map(|file| json!({
                    "id": file.id,
                    "name": file.name,
                    "media_type": file.media_type
                })),
                "step_recipe": step.step_recipe.map(|id| json!({
                    "id": id,
                    "name": linked_name,
                    "recipe_url": client.recipe_url(id)
                })),
                "ingredients": ings.into_iter()
                    .map(|ing| ingredient_view(ing, scale, client))
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

fn normalized_relation_name(name: &str) -> String {
    name.trim().to_lowercase()
}

fn build_header_patch(
    step: &crate::client::types::Step,
    header: &str,
    before_ingredient_id: Option<i32>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut ingredients: Vec<_> = step.ingredients.iter().collect();
    ingredients.sort_by_key(|ingredient| (ingredient.order, ingredient.id));
    let insert_at = match before_ingredient_id {
        Some(id) => ingredients
            .iter()
            .position(|ingredient| ingredient.id == id)
            .ok_or_else(|| {
                format!(
                    "before_ingredient_id {id} does not belong to step {}",
                    step.id
                )
            })?,
        None => ingredients.len(),
    };
    let mut patch: Vec<_> = ingredients
        .iter()
        .enumerate()
        .map(|(index, ingredient)| json!({"id": ingredient.id, "order": index}))
        .collect();
    patch.insert(
        insert_at,
        json!({
            "food": null,
            "unit": null,
            "amount": "0",
            "note": header,
            "order": insert_at,
            "is_header": true,
            "no_amount": true
        }),
    );
    for (index, ingredient) in patch.iter_mut().enumerate() {
        ingredient["order"] = json!(index);
    }
    Ok(patch)
}

fn find_relation_candidates(
    recipes: &[crate::client::types::Recipe],
    requested_parent_ids: Option<&[i32]>,
    client: &TandoorClient,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let mut by_name: std::collections::HashMap<String, Vec<&crate::client::types::Recipe>> =
        std::collections::HashMap::new();
    for recipe in recipes {
        by_name
            .entry(normalized_relation_name(&recipe.name))
            .or_default()
            .push(recipe);
    }

    let mut candidates = Vec::new();
    let mut ambiguous = Vec::new();
    for parent in recipes {
        if requested_parent_ids.is_some_and(|ids| !ids.contains(&parent.id)) {
            continue;
        }
        for (step_index, step) in ordered_steps(&parent.steps).into_iter().enumerate() {
            if step.step_recipe.is_some() {
                continue;
            }
            let mut ingredients: Vec<_> = step.ingredients.iter().collect();
            ingredients.sort_by_key(|ingredient| (ingredient.order, ingredient.id));
            for ingredient in ingredients {
                let Some(food) = ingredient.food.as_ref() else {
                    continue;
                };
                if food.recipe.is_some() {
                    continue;
                }
                let Some(matches) = by_name.get(&normalized_relation_name(&food.name)) else {
                    continue;
                };
                let matches: Vec<_> = matches
                    .iter()
                    .copied()
                    .filter(|target| target.id != parent.id)
                    .collect();
                if matches.is_empty() {
                    continue;
                }
                let source = json!({
                    "parent_recipe": {
                        "id": parent.id,
                        "name": parent.name,
                        "recipe_url": client.recipe_url(parent.id)
                    },
                    "step_id": step.id,
                    "step_number": step_index + 1,
                    "ingredient_id": ingredient.id,
                    "food_id": food.id,
                    "food": food.name
                });
                if matches.len() > 1 {
                    ambiguous.push(json!({
                        "source": source,
                        "reason": "Multiple recipes have the exact same normalized name",
                        "matching_recipes": matches.into_iter().map(|target| json!({
                            "id": target.id,
                            "name": target.name,
                            "recipe_url": client.recipe_url(target.id)
                        })).collect::<Vec<_>>()
                    }));
                } else {
                    let target = matches[0];
                    candidates.push(json!({
                        "source": source,
                        "proposed_recipe": {
                            "id": target.id,
                            "name": target.name,
                            "recipe_url": client.recipe_url(target.id)
                        },
                        "choices": {
                            "step_embed": "Local to this parent step; use link_step_recipe",
                            "food_link": "Global to every use of this Food record; use link_food_recipe"
                        }
                    }));
                }
            }
        }
    }
    (candidates, ambiguous)
}

/// Converts step/ingredient input from tool params into the nested request shape
/// Tandoor's recipe endpoint expects, auto-creating foods/units by name.
fn build_step_requests(
    steps: Vec<RecipeStepInput>,
) -> Vec<crate::client::types::CreateStepRequest> {
    steps
        .into_iter()
        .enumerate()
        .map(|(step_idx, step)| crate::client::types::CreateStepRequest {
            name: step.name,
            instruction: step.instruction,
            ingredients: step
                .ingredients
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(ing_idx, ing)| build_ingredient_request(ing, ing_idx as i32))
                .collect(),
            time: step.time,
            order: step_idx as i32 + 1,
        })
        .collect()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ImportRecipeParams {
    pub url: String,
}

/// Something to buy: plain text like "3 lemons", or a structured object.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ShoppingItemInput {
    /// Plain text, e.g. "3 lemons", "2 lb chicken thighs", "milk", "1/2 cup parsley",
    /// "a dozen eggs", "2 cans of tomatoes"
    Text(String),
    Structured(ShoppingItem),
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ShoppingItem {
    /// Food name, e.g. "lemons"
    #[serde(alias = "name")]
    pub food: String,
    /// How many / how much (omit for just "milk")
    #[serde(default)]
    pub amount: Option<f64>,
    /// Unit name, e.g. "lb", "bag" (omit for a count)
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddToShoppingListParams {
    /// What to add, in plain words: ["3 lemons", "2 lb chicken thighs", "milk"].
    /// Objects like {"food": "lemons", "amount": 3} also work.
    pub items: Vec<ShoppingItemInput>,
    /// If the same food (and unit) is already on the list, increase that line instead of
    /// adding a second one (default true)
    #[serde(default)]
    pub merge_with_existing: Option<bool>,
    /// Also put the items on this named shopping list (name or ID; created if the name is new)
    #[serde(default)]
    pub shopping_list: Option<NameOrId>,
}

/// A shopping list item: the food's name as it appears on the list, or an entry ID
/// from get_shopping_list.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ShoppingRef {
    /// Entry ID from get_shopping_list
    Id(i64),
    /// Food name, e.g. "lemons" (case-insensitive; plurals match)
    Name(String),
}

/// Something referred to by name (case-insensitive) or by ID.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum NameOrId {
    Id(i64),
    Name(String),
}

/// Finds the object a NameOrId points to among `items` (JSON objects with "id" and
/// "name"): exact ID, else exact case-insensitive name, else a partial name match only
/// when it is unambiguous.
fn resolve_named<'a>(
    items: &'a [serde_json::Value],
    target: &NameOrId,
    what: &str,
) -> Result<&'a serde_json::Value, String> {
    let name_of = |v: &serde_json::Value| v["name"].as_str().unwrap_or("").to_string();
    match target {
        NameOrId::Id(id) => items
            .iter()
            .find(|v| v["id"].as_i64() == Some(*id))
            .ok_or_else(|| format!("No {what} with ID {id}")),
        NameOrId::Name(name) => {
            let lower = name.trim().to_lowercase();
            if let Some(v) = items.iter().find(|v| name_of(v).to_lowercase() == lower) {
                return Ok(v);
            }
            let partial: Vec<_> = items
                .iter()
                .filter(|v| name_of(v).to_lowercase().contains(&lower))
                .collect();
            match partial.as_slice() {
                [v] => Ok(v),
                [] => {
                    let mut names: Vec<String> = items.iter().map(name_of).collect();
                    names.truncate(25);
                    Err(format!("No {what} named '{name}'. Existing: {names:?}"))
                }
                many => {
                    let names: Vec<String> = many.iter().map(|v| name_of(v)).collect();
                    Err(format!(
                        "'{name}' matches several {what}s: {names:?}. Use the exact name or ID"
                    ))
                }
            }
        }
    }
}

/// Resolves destructive targets without partial-name fallback.
fn resolve_named_exact<'a>(
    items: &'a [serde_json::Value],
    target: &NameOrId,
    what: &str,
) -> Result<&'a serde_json::Value, String> {
    match target {
        NameOrId::Id(id) => items
            .iter()
            .find(|value| value["id"].as_i64() == Some(*id))
            .ok_or_else(|| format!("No {what} with ID {id}")),
        NameOrId::Name(name) => items
            .iter()
            .find(|value| {
                value["name"]
                    .as_str()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name.trim()))
            })
            .ok_or_else(|| {
                format!("No {what} with the exact name '{name}'. Use the exact name or numeric ID")
            }),
    }
}

fn shopping_list_view(v: &serde_json::Value) -> serde_json::Value {
    json!({"id": v["id"], "name": v["name"], "description": v["description"], "color": v["color"]})
}

fn category_view(v: &serde_json::Value) -> serde_json::Value {
    json!({"id": v["id"], "name": v["name"], "description": v["description"]})
}

/// A supermarket with its aisles (categories) in walking order.
fn supermarket_view(v: &serde_json::Value) -> serde_json::Value {
    let mut relations: Vec<&serde_json::Value> = v["category_to_supermarket"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    relations.sort_by_key(|r| {
        (
            r["order"].as_i64().unwrap_or(0),
            r["id"].as_i64().unwrap_or(0),
        )
    });
    json!({
        "id": v["id"],
        "name": v["name"],
        "description": v["description"],
        "category_order": relations.iter().map(|r| r["category"]["name"].clone()).collect::<Vec<_>>()
    })
}

/// Shopping-list recipe groups, named after their recipe so they can be referred to
/// by recipe name.
fn recipe_group_view(
    v: &serde_json::Value,
    entries: &[crate::client::types::ShoppingListEntry],
) -> serde_json::Value {
    let id = v["id"].as_i64().unwrap_or(0);
    let name = v["recipe_data"]["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| v["name"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| v["meal_plan_data"]["title"].as_str())
        .unwrap_or("");
    let items: Vec<_> = entries
        .iter()
        .filter(|e| e.list_recipe.map(i64::from) == Some(id))
        .map(|e| json!({"food": e.food.name, "amount": e.amount, "unit": e.unit.as_ref().map(|u| &u.name), "checked": e.checked}))
        .collect();
    json!({
        "id": id,
        "name": name,
        "recipe_id": v["recipe"],
        "meal_plan_id": v["mealplan"],
        "servings": v["servings"],
        "items": items
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EmptyParams {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateShoppingListParams {
    /// List name, e.g. "Costco" or "Party"
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Hex color, e.g. "#2e7d32"
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateShoppingListParams {
    /// The list to change (name or ID)
    pub shopping_list: NameOrId,
    /// New name
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Hex color, e.g. "#2e7d32"
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteShoppingListParams {
    /// The list to delete (name or ID)
    pub shopping_list: NameOrId,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateShoppingListRecipeParams {
    /// The recipe on the shopping list (recipe name, or group ID from get_shopping_list_recipes)
    pub recipe: NameOrId,
    /// New number of servings; that recipe's item amounts are rescaled
    pub servings: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveRecipeFromShoppingListParams {
    /// The recipe on the shopping list (recipe name, or group ID from get_shopping_list_recipes)
    pub recipe: NameOrId,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateSupermarketParams {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Category (aisle) names in the order you walk the store; missing categories are created
    #[serde(default)]
    pub category_order: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateSupermarketParams {
    /// The supermarket to change (name or ID)
    pub supermarket: NameOrId,
    /// New name
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// FULL aisle order for this store: category names in walking order. Categories not
    /// listed are removed from this store (the categories themselves are kept).
    #[serde(default)]
    pub category_order: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteSupermarketParams {
    /// The supermarket to delete (name or ID)
    pub supermarket: NameOrId,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateSupermarketCategoryParams {
    /// Category (aisle/section) name, e.g. "Produce"
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateSupermarketCategoryParams {
    /// The category to change (name or ID)
    pub category: NameOrId,
    /// New name
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteSupermarketCategoryParams {
    /// The category to delete (name or ID)
    pub category: NameOrId,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetFoodCategoryParams {
    /// Food names, e.g. ["lemons", "parsley"]
    pub foods: Vec<String>,
    /// Category (aisle) name, e.g. "Produce"; created if new. Omit or "" to clear.
    #[serde(default)]
    pub category: Option<String>,
}

/// Authenticates or returns an "Authentication Error" tool result from the handler.
macro_rules! auth_or_return {
    ($server:expr) => {
        match $server.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => return tool_err("Authentication Error", e),
        }
    };
}

fn tool_ok(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&value).unwrap(),
    )]))
}

fn tool_err(error: &str, details: impl std::fmt::Display) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![Content::text(
        json!({"error": error, "details": details.to_string()}).to_string(),
    )]))
}

/// Keyword names in display order, with "source" keywords first.
///
/// This kitchen marks the origin of a recipe (cookbook, website, person) with a
/// keyword whose description starts with "Source". Every recipe view lists that
/// keyword first so the origin is always visible; the remaining keywords keep
/// Tandoor's order. The name is also checked because list endpoints may omit
/// keyword descriptions.
fn keyword_names(keywords: &[crate::client::types::Keyword]) -> Vec<&str> {
    fn is_source(keyword: &crate::client::types::Keyword) -> bool {
        let starts_with_source = |text: &str| {
            text.trim_start()
                .get(..6)
                .is_some_and(|p| p.eq_ignore_ascii_case("source"))
        };
        keyword
            .description
            .as_deref()
            .is_some_and(starts_with_source)
            || starts_with_source(&keyword.name)
    }
    let (source, rest): (Vec<_>, Vec<_>) = keywords.iter().partition(|k| is_source(k));
    source
        .into_iter()
        .chain(rest)
        .map(|k| k.name.as_str())
        .collect()
}

fn merge_nutrition(
    current: Option<&crate::client::types::Nutrition>,
    input: &NutritionInput,
) -> crate::client::types::NutritionRequest {
    crate::client::types::NutritionRequest {
        id: current.and_then(|value| value.id),
        calories: input
            .calories
            .or_else(|| current.and_then(|value| value.calories))
            .unwrap_or(0.0),
        carbohydrates: input
            .carbohydrates
            .or_else(|| current.and_then(|value| value.carbohydrates))
            .unwrap_or(0.0),
        fats: input
            .fats
            .or_else(|| current.and_then(|value| value.fats))
            .unwrap_or(0.0),
        proteins: input
            .proteins
            .or_else(|| current.and_then(|value| value.proteins))
            .unwrap_or(0.0),
        source: input
            .source
            .clone()
            .or_else(|| current.and_then(|value| value.source.clone())),
    }
}

fn property_type_for_input<'a>(
    target: &NameOrId,
    types: &'a [crate::client::types::PropertyType],
) -> Result<&'a crate::client::types::PropertyType, String> {
    let found = match target {
        NameOrId::Id(id) => types.iter().find(|value| i64::from(value.id) == *id),
        NameOrId::Name(name) => types
            .iter()
            .find(|value| value.name.trim().eq_ignore_ascii_case(name.trim())),
    };
    found.ok_or_else(|| {
        let available = types
            .iter()
            .map(|value| value.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Unknown recipe property type. Call get_property_types first. Available types: {available}"
        )
    })
}

fn plan_property_changes(
    current: &[crate::client::types::Property],
    set: &[RecipePropertyInput],
    remove: &[String],
    types: &[crate::client::types::PropertyType],
) -> Result<Vec<crate::client::types::PropertyRequest>, String> {
    let removed = remove
        .iter()
        .map(|name| name.trim().to_lowercase())
        .collect::<std::collections::HashSet<_>>();
    let mut result = current
        .iter()
        .filter(|property| !removed.contains(&property.property_type.name.trim().to_lowercase()))
        .map(|property| crate::client::types::PropertyRequest {
            id: property.id,
            property_amount: property.property_amount,
            property_type: crate::client::types::PropertyTypeReference {
                id: property.property_type.id,
                name: property.property_type.name.clone(),
            },
        })
        .collect::<Vec<_>>();

    for input in set {
        let property_type = property_type_for_input(&input.property_type, types)?;
        let existing = current
            .iter()
            .find(|property| property.property_type.id == property_type.id);
        result.retain(|property| property.property_type.id != property_type.id);
        result.push(crate::client::types::PropertyRequest {
            id: existing.and_then(|property| property.id),
            property_amount: input.amount,
            property_type: crate::client::types::PropertyTypeReference {
                id: property_type.id,
                name: property_type.name.clone(),
            },
        });
    }
    Ok(result)
}

fn recipe_scalar_body(params: &UpdateRecipeParams) -> serde_json::Map<String, serde_json::Value> {
    let mut body = serde_json::Map::new();
    body.insert("id".to_string(), json!(params.id));
    body.insert("internal".to_string(), json!(true));
    for (key, value) in [
        ("name", params.name.as_ref().map(|value| json!(value))),
        (
            "description",
            params.description.as_ref().map(|value| json!(value)),
        ),
        ("servings", params.servings.map(|value| json!(value))),
        (
            "servings_text",
            params.servings_text.as_ref().map(|value| json!(value)),
        ),
        (
            "working_time",
            params.cooking_time.map(|value| json!(value)),
        ),
        (
            "waiting_time",
            params.waiting_time.map(|value| json!(value)),
        ),
        (
            "source_url",
            params.source_url.as_ref().map(|value| json!(value)),
        ),
        (
            "show_ingredient_overview",
            params.show_ingredient_overview.map(|value| json!(value)),
        ),
    ] {
        if let Some(value) = value {
            body.insert(key.to_string(), value);
        }
    }
    body
}

fn recipe_fields_view(
    recipe: &crate::client::types::Recipe,
    client: &TandoorClient,
) -> serde_json::Map<String, serde_json::Value> {
    let created_by = recipe.created_by.as_ref().map(|creator| match creator {
        crate::client::types::UserOrId::Id(id) => json!({"id": id}),
        crate::client::types::UserOrId::User(user) => {
            json!({"id": user.id, "username": user.username, "display_name": user.display_name})
        }
    });
    let mut result = serde_json::Map::new();
    result.insert("id".to_string(), json!(recipe.id));
    result.insert("name".to_string(), json!(recipe.name));
    result.insert(
        "recipe_url".to_string(),
        json!(client.recipe_url(recipe.id)),
    );
    result.insert("description".to_string(), json!(recipe.description));
    result.insert("servings".to_string(), json!(recipe.servings));
    result.insert("servings_text".to_string(), json!(recipe.servings_text));
    result.insert("working_time".to_string(), json!(recipe.working_time));
    result.insert("waiting_time".to_string(), json!(recipe.waiting_time));
    result.insert(
        "total_time".to_string(),
        json!(recipe.working_time.unwrap_or(0) + recipe.waiting_time.unwrap_or(0)),
    );
    result.insert("source_url".to_string(), json!(recipe.source_url));
    result.insert(
        "show_ingredient_overview".to_string(),
        json!(recipe.show_ingredient_overview),
    );
    result.insert("internal".to_string(), json!(recipe.internal));
    result.insert(
        "keywords".to_string(),
        json!(keyword_names(&recipe.keywords)),
    );
    result.insert("nutrition".to_string(), json!(recipe.nutrition));
    result.insert(
        "properties".to_string(),
        json!(recipe
            .properties
            .iter()
            .map(|property| json!({
                "id": property.id,
                "type_id": property.property_type.id,
                "type": property.property_type.name,
                "unit": property.property_type.unit,
                "amount": property.property_amount
            }))
            .collect::<Vec<_>>()),
    );
    result.insert("rating".to_string(), json!(recipe.rating));
    result.insert("last_cooked".to_string(), json!(recipe.last_cooked));
    result.insert("created_by".to_string(), json!(created_by));
    result.insert("created".to_string(), json!(recipe.created));
    result.insert("updated".to_string(), json!(recipe.updated));
    result
}

/// Build a gallery result without exposing presigned bearer URLs to the model.
///
/// Structured content remains useful to non-UI clients, while `_meta` carries
/// URL data only to the MCP App. The result deliberately contains no inline
/// image blocks: ChatGPT silently fails to hand large tool results (hundreds of
/// kilobytes of base64) to the component iframe, which left the widget stuck on
/// its loading text. Clients without the component can fetch individual photos
/// through `get_recipe_image`.
fn gallery_result(
    structured_content: serde_json::Value,
    private_items: Vec<serde_json::Value>,
    urls_expire_at: u64,
) -> CallToolResult {
    CallToolResult::structured(structured_content).with_meta(Some(MetaObject(
        json!({
            "tandoor/gallery": {
                "items": private_items,
                "urls_expire_at": urls_expire_at
            }
        })
        .as_object()
        .expect("gallery result metadata must be an object")
        .clone(),
    )))
}

/// Makes `order` (category names, walking order) the store's full aisle order:
/// reorders existing aisles, adds missing ones (creating categories as needed), and
/// drops aisles not listed. Returns the refreshed store.
async fn sync_category_order(
    client: &TandoorClient,
    store: &serde_json::Value,
    order: &[String],
) -> Result<serde_json::Value, String> {
    let store_id = store["id"].as_i64().unwrap_or(0);
    let mut seen = std::collections::HashSet::new();
    let order: Vec<&str> = order
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty() && seen.insert(n.to_lowercase()))
        .collect();

    let relations: Vec<serde_json::Value> = store["category_to_supermarket"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let relation_for = |name: &str| {
        relations.iter().find(|r| {
            r["category"]["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
    };

    for (index, name) in order.iter().enumerate() {
        let result = match relation_for(name) {
            Some(r) if r["order"].as_i64() == Some(index as i64) => Ok(serde_json::Value::Null),
            Some(r) => {
                client
                    .api_update(
                        "supermarket-category-relation",
                        r["id"].as_i64().unwrap_or(0),
                        json!({"order": index}),
                    )
                    .await
            }
            // Tandoor reuses an existing category with this name or creates it
            None => client
                .api_create(
                    "supermarket-category-relation",
                    json!({"category": {"name": name}, "supermarket": store_id, "order": index}),
                )
                .await,
        };
        result.map_err(|e| format!("aisle '{name}': {e}"))?;
    }
    for r in &relations {
        let name = r["category"]["name"].as_str().unwrap_or("");
        if !order.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            client
                .api_delete(
                    "supermarket-category-relation",
                    r["id"].as_i64().unwrap_or(0),
                )
                .await
                .map_err(|e| format!("removing aisle '{name}': {e}"))?;
        }
    }
    client
        .api_get("supermarket", store_id)
        .await
        .map_err(|e| e.to_string())
}

/// Finds a recipe group on the shopping list by recipe name or group ID. Returns the
/// raw group and its view.
async fn find_recipe_group(
    client: &TandoorClient,
    target: &NameOrId,
    exact: bool,
) -> Result<(serde_json::Value, serde_json::Value), String> {
    let groups = client
        .api_list_all("shopping-list-recipe")
        .await
        .map_err(|e| e.to_string())?;
    let entries = client
        .get_all_shopping_entries()
        .await
        .map_err(|e| e.to_string())?;
    let views: Vec<serde_json::Value> = groups
        .iter()
        .map(|g| recipe_group_view(g, &entries))
        .collect();
    let view = if exact {
        resolve_named_exact(&views, target, "recipe on the shopping list")
    } else {
        resolve_named(&views, target, "recipe on the shopping list")
    }?
    .clone();
    let group = groups
        .into_iter()
        .find(|g| g["id"] == view["id"])
        .ok_or("recipe group disappeared")?;
    Ok((group, view))
}

/// Resolves a named shopping list; a name that matches nothing is created.
async fn resolve_or_create_shopping_list(
    client: &TandoorClient,
    target: &NameOrId,
) -> Result<crate::client::types::NamedRef, String> {
    let lists = client
        .api_list_all("shopping-list")
        .await
        .map_err(|e| e.to_string())?;
    let found = match target {
        NameOrId::Name(name)
            if !lists.iter().any(|l| {
                l["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&name.trim().to_lowercase())
            }) =>
        {
            if name.trim().is_empty() {
                return Err("shopping list name cannot be empty".to_string());
            }
            client
                .api_create(
                    "shopping-list",
                    json!({"name": name.trim(), "description": ""}),
                )
                .await
                .map_err(|e| e.to_string())?
        }
        _ => resolve_named(&lists, target, "shopping list")?.clone(),
    };
    serde_json::from_value(found).map_err(|e| e.to_string())
}

/// Resolves named shopping lists that must already exist.
async fn resolve_shopping_lists(
    client: &TandoorClient,
    targets: &[NameOrId],
) -> Result<Vec<crate::client::types::NamedRef>, String> {
    let lists = client
        .api_list_all("shopping-list")
        .await
        .map_err(|e| e.to_string())?;
    targets
        .iter()
        .map(|t| {
            let l = resolve_named(&lists, t, "shopping list")?;
            serde_json::from_value(json!({"id": l["id"], "name": l["name"]}))
                .map_err(|e| e.to_string())
        })
        .collect()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetShoppingListParams {
    /// Response layout: `flat` for a single list; other values are reserved.
    #[serde(default = "default_format")]
    pub format: String,
    /// Only items on this named shopping list (name or ID)
    #[serde(default)]
    pub shopping_list: Option<NameOrId>,
}

fn default_format() -> String {
    "flat".to_string()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CheckShoppingItemsParams {
    /// Items to act on: food names as they appear on the list (e.g. "lemons") or entry IDs
    pub items: Vec<ShoppingRef>,
    /// true = check off (default), false = uncheck (put back on the list)
    #[serde(default)]
    pub checked: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateShoppingListItemParams {
    /// The item to change: food name as it appears on the list (e.g. "lemons") or entry ID
    pub item: ShoppingRef,
    /// New quantity for the item
    #[serde(default)]
    pub amount: Option<f64>,
    /// New checked/purchased status
    #[serde(default)]
    pub checked: Option<bool>,
    /// New unit name, e.g. "lb"; "" removes the unit
    #[serde(default)]
    pub unit: Option<String>,
    /// Swap the food, e.g. "Meyer lemons" (reused or created by name)
    #[serde(default)]
    pub food: Option<String>,
    /// Named shopping lists this item should be on (replaces its current lists; [] = none)
    #[serde(default)]
    pub shopping_lists: Option<Vec<NameOrId>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveFromShoppingListParams {
    /// Items to act on: food names as they appear on the list (e.g. "lemons") or entry IDs
    pub items: Vec<ShoppingRef>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchFoodsParams {
    pub query: String,
    #[serde(default)]
    pub limit: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdatePantryItem {
    /// Food name. Matched against existing foods by exact name or plural (case-insensitive).
    pub food: String,
    /// Whether the food is on hand
    pub available: bool,
    /// Supermarket category to assign if the food has to be created (e.g. "Produce")
    #[serde(default)]
    pub supermarket_category: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdatePantryParams {
    pub items: Vec<UpdatePantryItem>,
    /// Create foods that don't exist yet when marking them available (default true)
    #[serde(default)]
    pub create_missing: Option<bool>,
}

fn food_name_matches(query: &str, food: &crate::client::types::Food) -> bool {
    name_matches(query, &food.name, food.plural_name.as_deref())
}

/// Which shopping list entries a reference points to: an entry ID, or a food name
/// matched exactly (case-insensitive, simple plurals), falling back to a partial match
/// only when it is unambiguous. A leading amount in the name ("3 lemons") is ignored.
fn match_shopping_entries<'a>(
    entries: &'a [crate::client::types::ShoppingListEntry],
    item: &ShoppingRef,
) -> Result<Vec<&'a crate::client::types::ShoppingListEntry>, String> {
    let name = match item {
        ShoppingRef::Id(id) => {
            return entries
                .iter()
                .find(|e| e.id as i64 == *id)
                .map(|e| vec![e])
                .ok_or_else(|| format!("No shopping list entry with ID {id}"));
        }
        ShoppingRef::Name(name) => parse_shopping_text(name, &[]).food,
    };

    let exact: Vec<_> = entries
        .iter()
        .filter(|e| name_matches(&name, &e.food.name, e.food.plural_name.as_deref()))
        .collect();
    if !exact.is_empty() {
        return Ok(exact);
    }

    let lower = name.trim().to_lowercase();
    let partial: Vec<_> = entries
        .iter()
        .filter(|e| e.food.name.to_lowercase().contains(&lower))
        .collect();
    let mut foods: Vec<&str> = partial.iter().map(|e| e.food.name.as_str()).collect();
    foods.sort_unstable();
    foods.dedup();
    match foods.len() {
        0 => Err(format!("'{name}' is not on the shopping list")),
        1 => Ok(partial),
        _ => Err(format!(
            "'{name}' matches several items: {foods:?}. Use the exact name or the entry ID"
        )),
    }
}

/// Merge target for a new item: an unchecked, manually added (not recipe-linked) entry
/// for the same food with the same unit.
fn find_mergeable_entry<'a>(
    entries: &'a [crate::client::types::ShoppingListEntry],
    food_id: i32,
    unit: Option<&str>,
) -> Option<&'a crate::client::types::ShoppingListEntry> {
    entries.iter().find(|e| {
        !e.checked
            && e.list_recipe.is_none()
            && e.food.id == food_id
            && match (e.unit.as_ref(), unit) {
                (None, None) => true,
                (Some(u), Some(name)) => u.name.eq_ignore_ascii_case(name),
                _ => false,
            }
    })
}

/// Look up an existing food by name. Returns the matching food's ID (if any) and up to
/// five similar food names. Tandoor's search doesn't find "Pepper" for "peppers", so if
/// the first search has no match, retry with the singular stem.
async fn find_food_by_name(
    client: &TandoorClient,
    name: &str,
) -> anyhow::Result<(Option<i32>, Vec<String>)> {
    let name = name.trim();
    let mut queries = vec![name];
    if let Some(stem) = name.strip_suffix("es").or_else(|| name.strip_suffix('s')) {
        if !stem.is_empty() {
            queries.push(stem);
        }
    }

    let mut candidates = Vec::new();
    for query in queries {
        let results = client.search_foods(query, Some(25)).await?.results;
        if let Some(food) = results.iter().find(|f| food_name_matches(name, f)) {
            return Ok((Some(food.id), candidates));
        }
        for food in results {
            if candidates.len() < 5 && !candidates.contains(&food.name) {
                candidates.push(food.name);
            }
        }
    }
    Ok((None, candidates))
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetMealPlansParams {
    /// Inclusive start date in `YYYY-MM-DD` format.
    pub from_date: String,
    /// Inclusive end date in `YYYY-MM-DD` format.
    pub to_date: String,
    #[serde(default)]
    pub meal_type: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateMealPlanParams {
    #[serde(default)]
    pub recipe_id: Option<i32>,
    #[serde(default)]
    pub title: Option<String>,
    pub servings: i32,
    /// Meal date in `YYYY-MM-DD` format.
    pub date: String,
    /// Numeric Tandoor meal-type identifier; discover it with `get_meal_types`.
    pub meal_type: i32,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteMealPlanParams {
    pub id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateMealPlanParams {
    pub id: i32,
    #[serde(default)]
    pub recipe_id: Option<i32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub servings: Option<i32>,
    /// YYYY-MM-DD format
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub meal_type: Option<i32>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetCookLogParams {
    #[serde(default)]
    pub recipe_id: Option<i32>,
    #[serde(default = "default_days_back")]
    pub days_back: i32,
}

fn default_days_back() -> i32 {
    30
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LogCookedRecipeParams {
    pub recipe_id: i32,
    #[serde(default = "default_servings")]
    pub servings: i32,
    #[serde(default)]
    pub rating: Option<i32>,
    #[serde(default)]
    pub comment: Option<String>,
}

fn default_servings() -> i32 {
    1
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SuggestFromInventoryParams {
    /// Suggestion strategy. The default is `maximum-use`.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Prefer foods expiring within this many days.
    #[serde(default = "default_days_until_expiry")]
    pub days_until_expiry: i32,
}

fn default_mode() -> String {
    "maximum-use".to_string()
}

fn default_days_until_expiry() -> i32 {
    3
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateRecipeParams {
    pub id: i32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Keyword (tag) names to add. Existing keywords are reused (case-insensitive); new
    /// ones are created. Other keywords on the recipe are kept.
    #[serde(default)]
    pub add_keywords: Option<Vec<String>>,
    /// Keyword (tag) names to remove from the recipe (case-insensitive). Others are kept.
    #[serde(default)]
    pub remove_keywords: Option<Vec<String>>,
    /// FULL REPLACEMENT: keyword IDs that become the recipe's entire keyword list; any
    /// keyword not listed is removed. To add or remove one, use add_keywords / remove_keywords.
    #[serde(default, alias = "keywords")]
    pub replace_all_keywords: Option<Vec<i64>>,
    #[serde(default)]
    pub servings: Option<i64>,
    #[serde(default)]
    pub servings_text: Option<String>,
    #[serde(default)]
    pub cooking_time: Option<i64>,
    #[serde(default)]
    pub waiting_time: Option<i64>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub show_ingredient_overview: Option<bool>,
    /// Partial nutrition update; omitted values retain their current values.
    #[serde(default)]
    pub nutrition: Option<NutritionInput>,
    /// Remove the complete nutrition record.
    #[serde(default)]
    pub remove_nutrition: Option<bool>,
    /// Add or replace properties by property type. Call get_property_types first.
    #[serde(default)]
    pub set_properties: Option<Vec<RecipePropertyInput>>,
    /// Property type names to remove (case-insensitive).
    #[serde(default)]
    pub remove_properties: Option<Vec<String>>,
    /// Edit specific existing steps (instruction, name, time, and that step's ingredients).
    /// Steps not listed are left untouched. Preferred way to change steps.
    #[serde(default)]
    pub step_updates: Option<Vec<StepUpdateInput>>,
    /// Insert new steps without touching existing ones.
    #[serde(default)]
    pub add_steps: Option<Vec<AddStepInput>>,
    /// step_numbers of steps to delete (with their ingredients).
    #[serde(default)]
    pub remove_steps: Option<Vec<usize>>,
    /// Reorder steps without changing them.
    #[serde(default)]
    pub move_steps: Option<Vec<MoveStepInput>>,
    /// FULL REPLACEMENT: deletes every existing step and ingredient and writes exactly
    /// this list. Only use to rewrite the whole recipe; to change part of it use
    /// step_updates / add_steps / remove_steps / move_steps. Cannot be combined with those.
    #[serde(default, alias = "steps")]
    pub replace_all_steps: Option<Vec<RecipeStepInput>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindUnlinkedRecipeIngredientsParams {
    /// Limit the audit to these parent recipes. Omit to scan the whole kitchen.
    #[serde(default)]
    pub recipe_ids: Option<Vec<i32>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinkStepRecipeParams {
    pub parent_recipe_id: i32,
    pub step_id: i32,
    pub linked_recipe_id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UnlinkStepRecipeParams {
    pub parent_recipe_id: i32,
    pub step_id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinkFoodRecipeParams {
    pub food_id: i32,
    pub linked_recipe_id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UnlinkFoodRecipeParams {
    pub food_id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddIngredientHeaderParams {
    pub parent_recipe_id: i32,
    pub step_id: i32,
    pub header: String,
    /// Insert before this stable ingredient id. Omit to append.
    #[serde(default)]
    pub before_ingredient_id: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetRelatedRecipeMealplanInclusionParams {
    pub enabled: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteRecipeParams {
    pub id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetRecipeImageParams {
    /// Recipe ID
    pub recipe_id: i32,
    /// Direct link to a JPEG, PNG, WebP, or GIF image (not a web page that shows one)
    pub image_url: String,
}

/// A file ChatGPT attached to the conversation (user upload or generated
/// image), as passed to tools listed in `_meta["openai/fileParams"]`.
///
/// The JSON schema is written by hand (see the `JsonSchema` impl below) so it
/// matches the Apps SDK template exactly: all four properties declared as plain
/// `string`, exactly `download_url` and `file_id` required, no
/// `additionalProperties`. A derive cannot express "optional but not nullable
/// and without a default": `#[serde(default)]` adds `"default": null`, and
/// `Option<String>` adds a null type, both of which deviate from the template
/// the host validates against. `download_url` is a short-lived signed HTTPS
/// URL that the regular public-image downloader can fetch.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatFile {
    pub download_url: String,
    pub file_id: String,
    pub mime_type: Option<String>,
    pub file_name: Option<String>,
}

impl schemars::JsonSchema for ChatFile {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ChatFile")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "A photo attached to the ChatGPT conversation. ChatGPT fills this in; do not construct it by hand.",
            "properties": {
                "download_url": { "type": "string", "description": "Temporary signed HTTPS URL supplied by ChatGPT." },
                "file_id": { "type": "string", "description": "ChatGPT file identifier." },
                "mime_type": { "type": "string", "description": "MIME type, when ChatGPT knows it." },
                "file_name": { "type": "string", "description": "Original file name, when ChatGPT knows it." }
            },
            "required": ["download_url", "file_id"],
            "additionalProperties": false
        })
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetRecipeImageFromFileParams {
    /// Recipe ID
    pub recipe_id: i32,
    /// The photo attached to the chat. Pass the attachment; ChatGPT supplies the file object. Not a /mnt/data path or sandbox: URL.
    // Typed as a raw value so that the placeholder strings some ChatGPT
    // clients send instead of a file object become a useful error rather
    // than a deserialization failure; the schema is still the `ChatFile` object.
    #[schemars(with = "ChatFile")]
    pub image: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveRecipeImageParams {
    /// Recipe ID
    pub recipe_id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRecipeBooksParams {
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateRecipeBookParams {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteRecipeBookParams {
    pub id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateRecipeBookParams {
    pub id: i32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddToRecipeBookParams {
    pub recipe_book: i32,
    pub recipe: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveFromRecipeBookParams {
    pub id: i32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRecipeBookEntriesParams {
    #[serde(default)]
    pub book_id: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddRecipeToShoppingListParams {
    pub recipe_id: i32,
    /// Servings to shop for (amounts are scaled). Defaults to the recipe's servings.
    #[serde(default)]
    pub servings: Option<i32>,
    /// Also add foods that are already on hand (default false)
    #[serde(default)]
    pub include_on_hand: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindDuplicateFoodsParams {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MergeFoodsParams {
    /// ID of the food to merge away. It is DELETED after its uses move to the target.
    pub source_id: i32,
    /// ID of the food to keep
    pub target_id: i32,
}

/// Groups foods whose names differ only by case, spacing, or a simple "s"/"es" plural
/// (e.g. "Tomato" / "tomatoes"), including matches via plural_name.
pub fn find_duplicate_food_groups(
    foods: &[crate::client::types::Food],
) -> Vec<Vec<&crate::client::types::Food>> {
    fn keys(name: &str) -> Vec<String> {
        let n = name
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let mut out = vec![n.clone()];
        if let Some(s) = n.strip_suffix("es") {
            out.push(s.to_string());
        }
        if let Some(s) = n.strip_suffix('s') {
            out.push(s.to_string());
        }
        out.retain(|k| !k.is_empty());
        out
    }
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    // Union-find over foods that share any name key
    let mut parent: Vec<usize> = (0..foods.len()).collect();
    let mut owner: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, food) in foods.iter().enumerate() {
        let names = std::iter::once(food.name.as_str()).chain(food.plural_name.as_deref());
        for key in names.flat_map(keys) {
            match owner.get(&key) {
                Some(&j) => {
                    let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                    parent[a] = b;
                }
                None => {
                    owner.insert(key, i);
                }
            }
        }
    }

    let mut groups: std::collections::BTreeMap<usize, Vec<&crate::client::types::Food>> =
        std::collections::BTreeMap::new();
    for (i, food) in foods.iter().enumerate() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(food);
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddMealPlanToShoppingListParams {
    pub from_date: String,
    pub to_date: String,
    /// Leave out foods marked on hand in the pantry (default true)
    #[serde(default)]
    pub skip_on_hand: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetSupermarketsParams {
    /// Only stores whose name contains this text
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetUnitConversionsParams {
    #[serde(default)]
    pub food_id: Option<i64>,
}

// Global shared authentication state
/// Global authentication token storage to handle Tandoor's rate limiting
static GLOBAL_AUTH: OnceLock<Arc<Mutex<Option<String>>>> = OnceLock::new();

/// Global credentials storage for automatic re-authentication
static GLOBAL_CREDENTIALS: OnceLock<(String, String)> = OnceLock::new();

/// # Tandoor MCP Server
///
/// The main MCP server implementation that provides Tandoor functionality through
/// standardized tools. This server handles all communication with Tandoor APIs
/// and exposes them as MCP tools for AI assistants.
///
/// ## Rate Limiting Considerations
///
/// Tandoor has aggressive rate limiting on authentication endpoints (10 requests/day).
/// The server handles this by:
/// - Storing authentication tokens globally
/// - Reusing tokens across requests
/// - Supporting pre-set tokens via environment variables
///
/// ## Example Usage
///
/// ```no_run
/// use mcp_tandoor::server::TandoorMcpServer;
///
/// // Create server with credentials
/// let server = TandoorMcpServer::new_with_credentials(
///     "http://localhost:8080".to_string(),
///     "admin".to_string(),
///     "password".to_string()
/// );
///
/// // Or set a pre-authenticated token
/// server.set_global_auth_token("your_token".to_string()).await.unwrap();
/// ```
#[derive(Clone)]
pub struct TandoorMcpServer {
    /// Thread-safe client for Tandoor API communication
    client: Arc<Mutex<TandoorClient>>,
    /// MCP tool router for handling tool requests
    tool_router: ToolRouter<TandoorMcpServer>,
    /// Optional S3-backed thumbnail pipeline used by gallery/image tools.
    thumbnails: Option<Arc<ThumbnailService>>,
}

/// Current widget resource URIs. ChatGPT treats a URI as an immutable cache key,
/// so the version suffix must be bumped whenever the widget HTML changes.
const RECIPE_GALLERY_URI: &str = "ui://tandoor/recipe-gallery-v12.html";
const MEAL_PLAN_GALLERY_URI: &str = "ui://tandoor/meal-plan-gallery-v16.html";
/// `read_resource` serves the current HTML for any `ui://tandoor/<widget>*.html`
/// URI, so a client holding a stale cached tool descriptor still gets a working
/// widget instead of an "unknown resource" error.
const RECIPE_GALLERY_URI_PREFIX: &str = "ui://tandoor/recipe-gallery";
const MEAL_PLAN_GALLERY_URI_PREFIX: &str = "ui://tandoor/meal-plan-gallery";

/// Insertion markers the widget HTML files use for the shared fragments.
const SHARED_CSS_MARKER: &str = "/* @include shared.css */";
const SHARED_JS_MARKER: &str = "// @include shared.js";

/// Assemble a widget document by inlining `widget/shared.css` and
/// `widget/shared.js` at their markers.
///
/// Both galleries render the same card and talk to the host the same way, so
/// that code lives once in the shared files and each HTML file keeps only its
/// own layout and copy. The sources are compile-time constants, so the result
/// is built on first use and cached for the life of the process. Missing
/// markers are a programming error and panic here rather than serving a widget
/// without styles or plumbing.
fn widget_html(template: &'static str, cache: &'static OnceLock<String>) -> &'static str {
    cache.get_or_init(|| {
        assert!(
            template.contains(SHARED_CSS_MARKER) && template.contains(SHARED_JS_MARKER),
            "widget template must contain both @include markers"
        );
        template
            .replace(SHARED_CSS_MARKER, include_str!("widget/shared.css"))
            .replace(SHARED_JS_MARKER, include_str!("widget/shared.js"))
    })
}

fn recipe_gallery_html() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    widget_html(include_str!("widget/recipe_gallery.html"), &CACHE)
}

fn meal_plan_gallery_html() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    widget_html(include_str!("widget/meal_plan_gallery.html"), &CACHE)
}

/// True for `<prefix>.html` and `<prefix>-v<N>.html` style URIs.
fn widget_uri_matches(uri: &str, prefix: &str) -> bool {
    uri.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(".html"))
        .is_some_and(|version| version.is_empty() || version.starts_with("-v"))
}

fn gallery_tool_meta() -> MetaObject {
    MetaObject(
        json!({
            "ui": {
                "resourceUri": RECIPE_GALLERY_URI,
                "visibility": ["model", "app"]
            },
            "openai/outputTemplate": RECIPE_GALLERY_URI,
            "openai/widgetAccessible": true,
            "openai/toolInvocation/invoking": "Preparing recipe gallery",
            "openai/toolInvocation/invoked": "Recipe gallery ready"
        })
        .as_object()
        .expect("static gallery metadata must be an object")
        .clone(),
    )
}

/// Marks `image` as a file input so ChatGPT fills it with a `ChatFile` for an
/// attachment instead of asking the user for a URL.
fn file_image_tool_meta() -> MetaObject {
    MetaObject(
        json!({
            "openai/fileParams": ["image"],
            "openai/toolInvocation/invoking": "Uploading recipe photo",
            "openai/toolInvocation/invoked": "Recipe photo updated"
        })
        .as_object()
        .expect("static file tool metadata must be an object")
        .clone(),
    )
}

fn meal_plan_gallery_tool_meta() -> MetaObject {
    MetaObject(
        json!({
            "ui": {
                "resourceUri": MEAL_PLAN_GALLERY_URI,
                "visibility": ["model", "app"]
            },
            "openai/outputTemplate": MEAL_PLAN_GALLERY_URI,
            "openai/widgetAccessible": true,
            "openai/toolInvocation/invoking": "Preparing meal-plan gallery",
            "openai/toolInvocation/invoked": "Meal-plan gallery ready"
        })
        .as_object()
        .expect("static meal-plan gallery metadata must be an object")
        .clone(),
    )
}

#[tool_router]
impl TandoorMcpServer {
    /// Create a new Tandoor MCP server with just a base URL.
    /// Authentication will need to be handled separately.
    pub fn new(base_url: String) -> Self {
        Self {
            client: Arc::new(Mutex::new(TandoorClient::new(base_url))),
            tool_router: Self::tool_router(),
            thumbnails: None,
        }
    }

    /// Create a new Tandoor MCP server with credentials for automatic authentication.
    ///
    /// The credentials are stored globally and will be used for automatic re-authentication
    /// when tokens expire. This is the recommended constructor for most use cases.
    ///
    /// # Arguments
    ///
    /// * `base_url` - The base URL of the Tandoor server (e.g., "http://localhost:8080")
    /// * `username` - Tandoor username for authentication
    /// * `password` - Tandoor password for authentication
    pub fn new_with_credentials(base_url: String, username: String, password: String) -> Self {
        // Store credentials globally so all instances can use them
        let _ = GLOBAL_CREDENTIALS.set((username, password));

        Self {
            client: Arc::new(Mutex::new(TandoorClient::new(base_url))),
            tool_router: Self::tool_router(),
            thumbnails: None,
        }
    }

    /// Attach the optional thumbnail pipeline configured by the binary.
    pub fn with_thumbnails(mut self, thumbnails: Arc<ThumbnailService>) -> Self {
        self.thumbnails = Some(thumbnails);
        self
    }

    fn gallery_resource_meta(&self) -> MetaObject {
        self.widget_resource_meta(
            "A responsive gallery of authoritative recipe photos from the user's Tandoor kitchen.",
        )
    }

    fn meal_plan_gallery_resource_meta(&self) -> MetaObject {
        self.widget_resource_meta(
            "A polished meal-plan card gallery with authoritative recipe photos, tags, notes, timing, and servings.",
        )
    }

    fn widget_resource_meta(&self, description: &str) -> MetaObject {
        let resource_domains: Vec<&str> = self
            .thumbnails
            .as_ref()
            .map(|service| vec![service.resource_origin()])
            .unwrap_or_default();
        MetaObject(
            json!({
                "ui": {
                    "prefersBorder": true,
                    "csp": {
                        "connectDomains": [],
                        "resourceDomains": resource_domains
                    }
                },
                "openai/widgetCSP": {
                    "connect_domains": [],
                    "resource_domains": resource_domains
                },
                "openai/widgetDescription": description,
                "openai/ui": {
                    "availableDisplayModes": ["inline", "fullscreen"]
                }
            })
            .as_object()
            .expect("static widget metadata must be an object")
            .clone(),
        )
    }

    /// Set a pre-authenticated token to avoid rate limiting.
    ///
    /// This is useful when you have a token from a previous authentication
    /// or from an external source. Tandoor limits authentication to 10 requests
    /// per day, so reusing tokens is important for reliability.
    ///
    /// # Arguments
    ///
    /// * `token` - A valid Tandoor OAuth2 access token
    pub async fn set_global_auth_token(&self, token: String) -> Result<(), anyhow::Error> {
        let auth_storage = GLOBAL_AUTH.get_or_init(|| Arc::new(Mutex::new(None)));
        let mut auth = auth_storage.lock().await;
        *auth = Some(token);
        tracing::debug!("Global auth token updated");
        Ok(())
    }

    pub async fn authenticate(
        &self,
        username: String,
        password: String,
    ) -> Result<(), anyhow::Error> {
        let mut client = self.client.lock().await;
        let result = client.authenticate(username, password).await;

        if result.is_ok() {
            // Store the token globally for all instances to use
            if let Some(token) = client.get_token() {
                self.set_global_auth_token(token.to_string()).await?;
            }
        }

        result
    }

    /// Returns an authenticated client snapshot. The mutex protects token
    /// refresh only; network requests must not serialize behind it.
    async fn ensure_authenticated(&self) -> Result<TandoorClient, anyhow::Error> {
        tracing::info!("=== ensure_authenticated: acquiring client lock ===");

        // Add timeout to client lock acquisition
        let client_result =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.client.lock()).await;

        let mut client = match client_result {
            Ok(client) => {
                tracing::info!("=== ensure_authenticated: client lock acquired ===");
                client
            }
            Err(_) => {
                tracing::error!("=== ensure_authenticated: TIMEOUT acquiring client lock ===");
                return Err(anyhow::anyhow!("Timeout acquiring client lock"));
            }
        };

        if client.is_authenticated() {
            tracing::info!("=== ensure_authenticated: already authenticated ===");
            return Ok(client.clone());
        }

        // Prefer a reusable token to avoid Tandoor's authentication rate limit.
        if let Some(auth_storage) = GLOBAL_AUTH.get() {
            if let Some(token) = auth_storage.lock().await.clone() {
                client.set_token(token);
                tracing::info!("Using pre-authenticated Tandoor token");
                return Ok(client.clone());
            }
        }

        tracing::info!("=== ensure_authenticated: not authenticated, proceeding with auth ===");

        // If no global token, try to authenticate with stored credentials directly
        if let Some((username, password)) = GLOBAL_CREDENTIALS.get() {
            tracing::info!(
                "Auto-authenticating with stored credentials for user: {}",
                username
            );
            let result = client
                .authenticate(username.clone(), password.clone())
                .await;

            match result {
                Ok(_) => {
                    tracing::info!("Authentication successful");
                    if let Some(token) = client.get_token().map(str::to_owned) {
                        let auth_storage = GLOBAL_AUTH.get_or_init(|| Arc::new(Mutex::new(None)));
                        *auth_storage.lock().await = Some(token);
                    }
                    Ok(client.clone())
                }
                Err(e) => {
                    tracing::error!("Authentication failed: {}", e);
                    Err(e)
                }
            }
        } else {
            tracing::error!("No authentication credentials available");
            Err(anyhow::anyhow!("No authentication credentials available"))
        }
    }

    pub async fn test_api_access(&self) -> Result<(), anyhow::Error> {
        // Just check if we can authenticate - don't make actual API calls during startup
        // to avoid holding locks during network I/O
        let client = self.ensure_authenticated().await?;

        // Check if we have a token
        if client.is_authenticated() {
            if let Some(preview) = client.get_token_preview() {
                tracing::info!("Authentication test successful - have token: {}", preview);
            }
            Ok(())
        } else {
            tracing::error!("Authentication test failed - no token available");
            Err(anyhow::anyhow!("No authentication token available"))
        }
        // Lock is automatically released here when client goes out of scope
    }

    // Recipe tools
    #[tool(
        description = "Search for recipes with flexible querying. Each result has `has_photo`; pass has_photo: false to list recipes missing a photo (e.g. to add photos with set_recipe_image or set_recipe_image_from_file). When presenting results to the user, call get_recipe_gallery with their ids so the authoritative photos render inline, even if photos were not requested; use get_recipe_details only for ingredients, steps, or nutrition.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn search_recipes(
        &self,
        Parameters(params): Parameters<SearchRecipesParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in search_recipes: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        let search = |limit: Option<i32>| {
            client.search_recipes(
                params.query.as_deref(),
                limit,
                params.keywords.as_deref(),
                params.foods.as_deref(),
                params.max_cooking_time,
                params.min_rating,
                params.random,
            )
        };
        let result = match params.has_photo {
            None => search(params.limit).await,
            // Tandoor can't filter by photo, so read every matching recipe and filter here
            Some(want) => {
                let mut result = search(Some(100)).await;
                if let Ok(page) = &mut result {
                    let mut next = page.next.take();
                    let mut pages = 1usize;
                    while let Some(url) = next {
                        if pages >= 100 {
                            result = Err(anyhow::anyhow!(
                                "Photo filtering exceeded the 100-page safety limit"
                            ));
                            break;
                        }
                        match client.get_recipe_page(&url).await {
                            Ok(more) => {
                                page.results.extend(more.results);
                                next = more.next;
                                pages += 1;
                            }
                            Err(e) => {
                                result = Err(e);
                                break;
                            }
                        }
                    }
                }
                result.map(|mut page| {
                    page.results.retain(|r| r.image.is_some() == want);
                    page.count = page.results.len() as i32;
                    if let Some(limit) = params.limit.filter(|l| *l > 0) {
                        page.results.truncate(limit as usize);
                    }
                    page
                })
            }
        };

        match result {
            Ok(response) => {
                let recipes_json: Vec<serde_json::Value> = response.results
                    .into_iter()
                    .map(|recipe| {
                        json!({
                            "id": recipe.id,
                            "name": recipe.name,
                            "recipe_url": client.recipe_url(recipe.id),
                            "description": recipe.description,
                            "total_time": recipe.working_time.unwrap_or(0) + recipe.waiting_time.unwrap_or(0),
                            "has_photo": recipe.image.is_some(),
                            "image": recipe.image,
                            "servings": recipe.servings,
                            "servings_text": recipe.servings_text,
                            "keywords": keyword_names(&recipe.keywords),
                            "rating": recipe.rating,
                            "last_cooked": recipe.last_cooked,
                            "created": recipe.created,
                            "updated": recipe.updated
                        })
                    })
                    .collect();

                let result = json!({
                    "recipes": recipes_json,
                    "total_count": response.count,
                    "image_instruction": if params.has_photo == Some(false) {
                        "These recipes have no photo; do not substitute an unrelated, web-searched, or stock image."
                    } else {
                        "Call get_recipe_gallery with the ids of the recipes you present so their authoritative photos render inline, even if the user did not ask for images; do not substitute an unrelated, web-searched, or stock image."
                    },
                    "search_interpretation": format!("Found {} recipes{}{}",
                        response.count,
                        params.query.as_ref().map_or(String::new(), |q| format!(" matching '{q}'")),
                        match params.has_photo { Some(true) => " with a photo", Some(false) => " missing a photo", None => "" }
                    )
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to search recipes",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Build an inline gallery for 1 to 24 recipes using small authoritative thumbnails. Call this whenever you present recipes to the user (search results, recommendations, comparisons), even if photos were not requested; it is the only way recipe photos reach the user.",
        annotations(read_only_hint = true, open_world_hint = true),
        meta = gallery_tool_meta()
    )]
    async fn get_recipe_gallery(
        &self,
        Parameters(params): Parameters<GetRecipeGalleryParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.recipe_ids.is_empty() || params.recipe_ids.len() > 24 {
            return tool_err(
                "Invalid gallery size",
                "recipe_ids must contain between 1 and 24 identifiers",
            );
        }
        if params.recipe_ids.iter().any(|id| *id <= 0) {
            return tool_err("Invalid recipe id", "recipe identifiers must be positive");
        }
        let Some(thumbnails) = self.thumbnails.clone() else {
            return tool_err(
                "Recipe galleries are not configured",
                "Set S3_BUCKET and the documented S3 thumbnail credentials",
            );
        };
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(error) => return tool_err("Authentication Error", error),
        };

        let mut items = Vec::with_capacity(params.recipe_ids.len());
        let mut private_items = Vec::with_capacity(params.recipe_ids.len());
        let mut expires_at = 0_u64;
        for recipe_id in params.recipe_ids {
            let recipe = match client.get_recipe(recipe_id).await {
                Ok(recipe) => recipe,
                Err(error) => {
                    items.push(json!({
                        "recipe_id": recipe_id,
                        "name": format!("Recipe {recipe_id}"),
                        "image_status": "unavailable",
                        "error": error.to_string()
                    }));
                    continue;
                }
            };
            // The widget renders the same card as the meal-plan gallery, so the
            // item carries the same recipe facts. Everything here is
            // model-visible structured content; thumbnail URLs are not.
            let mut item = json!({
                "recipe_id": recipe.id,
                "name": recipe.name,
                "recipe_url": client.recipe_url(recipe.id),
                "description": recipe.description,
                "servings": recipe.servings,
                "working_time": recipe.working_time,
                "waiting_time": recipe.waiting_time,
                "total_time": recipe.working_time.unwrap_or(0) + recipe.waiting_time.unwrap_or(0),
                "keywords": keyword_names(&recipe.keywords),
                "image_status": "not_set"
            });
            let Some(image_path) = recipe.image.clone() else {
                items.push(item);
                continue;
            };
            let image_client = client.clone();
            let fetch_path = image_path.clone();
            match thumbnails
                .thumbnail_for(recipe.id, &image_path, move || async move {
                    image_client
                        .get_recipe_image(&fetch_path)
                        .await
                        .map(|(bytes, _)| bytes)
                })
                .await
            {
                Ok(thumbnail) => {
                    expires_at = if expires_at == 0 {
                        thumbnail.expires_at
                    } else {
                        expires_at.min(thumbnail.expires_at)
                    };
                    private_items.push(json!({
                        "recipe_id": recipe.id,
                        "thumbnail_url": thumbnail.url.as_str(),
                        "width": thumbnail.width,
                        "height": thumbnail.height
                    }));
                    item["image_status"] = json!("attached");
                }
                Err(error) => {
                    tracing::warn!(recipe_id = recipe.id, %error, "Could not prepare thumbnail");
                    item["image_status"] = json!("unavailable");
                    item["error"] = json!(error.to_string());
                }
            }
            items.push(item);
        }

        Ok(gallery_result(
            json!({
                "items": items,
                "message": "The attached gallery component displays the authoritative recipe photos. Summarize in Markdown with each recipe name linked to its recipe_url, but include no images of any kind (no Markdown images, image lists, carousels, or web-searched or stock photos), never print or reconstruct thumbnail URLs, and do not call get_recipe_image for these recipes."
            }),
            private_items,
            expires_at,
        ))
    }

    #[tool(
        description = "Get one authoritative recipe image. Use thumbnail by default; request original only when the user explicitly asks for the full-size photo.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_recipe_image(
        &self,
        Parameters(params): Parameters<GetRecipeImageParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.recipe_id <= 0 {
            return tool_err("Invalid recipe id", "recipe_id must be positive");
        }
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(error) => return tool_err("Authentication Error", error),
        };
        let recipe = match client.get_recipe(params.recipe_id).await {
            Ok(recipe) => recipe,
            Err(error) => return tool_err("Recipe not found", error),
        };
        let Some(image_path) = recipe.image.clone() else {
            return tool_err("Recipe has no photo", recipe.name);
        };

        // The image travels as an inline content block only. Presigned thumbnail
        // URLs are bearer credentials and never appear in model-visible text.
        let (bytes, mime_type) = match params.variant {
            RecipeImageVariant::Original => match client.get_recipe_image(&image_path).await {
                Ok((bytes, mime_type)) => (bytes, mime_type),
                Err(error) => return tool_err("Could not fetch recipe photo", error),
            },
            RecipeImageVariant::Thumbnail => {
                let Some(thumbnails) = self.thumbnails.clone() else {
                    return tool_err(
                        "Recipe thumbnails are not configured",
                        "Set S3_BUCKET and the documented S3 thumbnail credentials",
                    );
                };
                let image_client = client.clone();
                let fetch_path = image_path.clone();
                let thumbnail = match thumbnails
                    .thumbnail_for(recipe.id, &image_path, move || async move {
                        image_client
                            .get_recipe_image(&fetch_path)
                            .await
                            .map(|(bytes, _)| bytes)
                    })
                    .await
                {
                    Ok(thumbnail) => thumbnail,
                    Err(error) => return tool_err("Could not prepare recipe thumbnail", error),
                };
                let bytes = match thumbnails.bytes(&thumbnail).await {
                    Ok(bytes) => bytes,
                    Err(error) => return tool_err("Could not load recipe thumbnail", error),
                };
                (bytes, "image/jpeg")
            }
            RecipeImageVariant::None => {
                return tool_err(
                    "Invalid image variant",
                    "get_recipe_image supports thumbnail or original",
                );
            }
        };

        let description = json!({
            "recipe_id": recipe.id,
            "name": recipe.name,
            "recipe_url": client.recipe_url(recipe.id),
            "variant": params.variant
        });
        Ok(CallToolResult::success(vec![
            Content::text(description.to_string()),
            Content::image(BASE64_STANDARD.encode(bytes), mime_type),
        ]))
    }

    #[tool(
        description = "Get comprehensive recipe information, including servings_text, source_url, nutrition, recipe properties, metadata, steps, and ingredients, and attach its authoritative thumbnail by default. Pass image: original only when full resolution is required, or image: none to omit it. Amounts are scaled if `servings` is given.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_recipe_details(
        &self,
        Parameters(params): Parameters<GetRecipeDetailsParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.servings.is_some_and(|servings| servings <= 0) {
            return tool_err("Invalid servings", "servings must be at least 1");
        }

        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in get_recipe_details: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.get_recipe(params.id).await {
            Ok(recipe) => {
                let scaling_factor = if let Some(target_servings) = params.servings {
                    let original_servings = recipe.servings.unwrap_or(1);
                    if original_servings <= 0 {
                        return tool_err(
                            "Cannot scale recipe",
                            "the stored recipe servings must be at least 1",
                        );
                    }
                    target_servings as f64 / original_servings as f64
                } else {
                    1.0
                };

                let steps = steps_view(&recipe.steps, scaling_factor, &client);

                // Full ingredient list across all steps, each tagged with its step
                let ingredients: Vec<serde_json::Value> = steps
                    .iter()
                    .flat_map(|step| {
                        let number = step["step_number"].clone();
                        step["ingredients"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .map(move |mut ing| {
                                ing["step_number"] = number.clone();
                                ing
                            })
                    })
                    .collect();

                // Only the inline image block carries the thumbnail; its presigned
                // URL stays out of model-visible text.
                let image_attachment = match (recipe.image.as_deref(), params.image) {
                    (_, RecipeImageVariant::None) | (None, _) => None,
                    (Some(image), RecipeImageVariant::Original) => {
                        client.get_recipe_image(image).await.ok()
                    }
                    (Some(image), RecipeImageVariant::Thumbnail) => {
                        if let Some(thumbnails) = self.thumbnails.clone() {
                            let image_client = client.clone();
                            let fetch_path = image.to_owned();
                            match thumbnails
                                .thumbnail_for(recipe.id, image, move || async move {
                                    image_client
                                        .get_recipe_image(&fetch_path)
                                        .await
                                        .map(|(bytes, _)| bytes)
                                })
                                .await
                            {
                                Ok(thumbnail) => match thumbnails.bytes(&thumbnail).await {
                                    Ok(bytes) => Some((bytes, "image/jpeg")),
                                    Err(error) => {
                                        tracing::warn!(recipe_id = recipe.id, %error, "Could not load recipe thumbnail");
                                        None
                                    }
                                },
                                Err(error) => {
                                    tracing::warn!(recipe_id = recipe.id, %error, "Could not prepare recipe thumbnail");
                                    None
                                }
                            }
                        } else {
                            tracing::warn!(
                                "S3 thumbnails are disabled; attaching the original recipe image"
                            );
                            client.get_recipe_image(image).await.ok()
                        }
                    }
                };
                let image_status = match (&recipe.image, &image_attachment) {
                    (_, Some(_)) => "attached",
                    (Some(_), None) if matches!(params.image, RecipeImageVariant::None) => {
                        "not_requested"
                    }
                    (Some(_), None) => "unavailable",
                    (None, None) => "not_set",
                };
                let image_instruction = match image_status {
                    "attached" => {
                        "Use the attached image as the authoritative photo for this recipe; do not add web-searched or stock photos."
                    }
                    "not_requested" => {
                        "The stored photo was intentionally omitted from this response."
                    }
                    "unavailable" => {
                        "The stored photo could not be attached; do not substitute an unrelated, web-searched, or stock image."
                    }
                    _ => "No photo is stored for this recipe; do not substitute an unrelated, web-searched, or stock image.",
                };

                let mut result = recipe_fields_view(&recipe, &client);
                result.insert("steps".to_string(), json!(steps));
                result.insert("ingredients".to_string(), json!(ingredients));
                result.insert(
                    "servings".to_string(),
                    json!(params.servings.unwrap_or(recipe.servings.unwrap_or(1))),
                );
                result.insert("image".to_string(), json!(recipe.image));
                result.insert("requested_image_variant".to_string(), json!(params.image));
                result.insert("image_status".to_string(), json!(image_status));
                result.insert("image_instruction".to_string(), json!(image_instruction));
                result.insert("food_properties".to_string(), json!(recipe.food_properties));
                result.insert("scaling_applied".to_string(), json!(scaling_factor != 1.0));

                let mut content = vec![Content::text(
                    serde_json::to_string_pretty(&serde_json::Value::Object(result)).unwrap(),
                )];
                if let Some((bytes, mime_type)) = image_attachment {
                    content.push(Content::image(BASE64_STANDARD.encode(bytes), mime_type));
                }
                Ok(CallToolResult::success(content))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to get recipe details",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Create a new internally managed recipe. Supports servings_text, source_url, show_ingredient_overview, nutrition, and recipe-level properties; call get_property_types before setting properties. Use `steps` to add structured steps with their ingredients. `instructions` is a legacy single-step fallback.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_recipe(
        &self,
        Parameters(params): Parameters<CreateRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in create_recipe: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        // Convert keywords from strings to CreateKeywordRequest
        let keywords = params
            .keywords
            .unwrap_or_default()
            .into_iter()
            .map(|name| crate::client::types::CreateKeywordRequest { name })
            .collect();

        let steps = if let Some(step_inputs) = params.steps {
            if let Err(e) = validate_steps(&step_inputs) {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Invalid recipe steps", "details": e}).to_string(),
                )]));
            }
            build_step_requests(step_inputs)
        } else if let Some(instructions) = params.instructions {
            vec![crate::client::types::CreateStepRequest {
                name: None,
                instruction: instructions,
                ingredients: vec![],
                time: None,
                order: 1,
            }]
        } else {
            vec![]
        };

        let properties = if let Some(inputs) = params.properties.as_deref() {
            let types = match client.list_property_types().await {
                Ok(response) => response.results,
                Err(error) => return tool_err("Failed to load property types", error),
            };
            match plan_property_changes(&[], inputs, &[], &types) {
                Ok(properties) => Some(properties),
                Err(error) => return tool_err("Invalid recipe properties", error),
            }
        } else {
            None
        };
        let nutrition = params
            .nutrition
            .as_ref()
            .map(|input| merge_nutrition(None, input));

        let request = crate::client::types::CreateRecipeRequest {
            name: params.name,
            internal: true,
            description: params.description,
            servings: params.servings,
            servings_text: params.servings_text,
            working_time: params.prep_time.unwrap_or(0),
            waiting_time: params.cook_time.unwrap_or(0),
            source_url: params.source_url,
            show_ingredient_overview: params.show_ingredient_overview,
            nutrition,
            properties,
            keywords,
            steps,
        };

        match client.create_recipe(request).await {
            Ok(recipe) => {
                let mut result = recipe_fields_view(&recipe, &client);
                result.insert("success".to_string(), json!(true));
                result.insert("message".to_string(), json!("Recipe created successfully"));

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&serde_json::Value::Object(result)).unwrap(),
                )]))
            }
            Err(e) => {
                tracing::error!("create_recipe tool failed: {}", e);
                let error = json!({
                    "error": "Failed to create recipe",
                    "details": e.to_string(),
                    "success": false
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Import a recipe from a URL as an internally managed Tandoor recipe. Tandoor will scrape and parse the page. Returns the imported recipe or an error message if the site is not supported.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn import_recipe_from_url(
        &self,
        Parameters(params): Parameters<ImportRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        match client.import_recipe_from_url(&params.url).await {
            Ok(result) => {
                if result.error {
                    return Ok(CallToolResult::error(vec![Content::text(
                        json!({
                            "error": "Import failed",
                            "message": result.msg,
                            "duplicates": result.duplicates,
                        })
                        .to_string(),
                    )]));
                }

                // Importing another Tandoor share URL creates the recipe in
                // the source endpoint itself.
                if let Some(recipe_id) = result.recipe_id {
                    let Ok(recipe_id_for_url) = i32::try_from(recipe_id) else {
                        return tool_err(
                            "Import partially succeeded",
                            "Tandoor returned a recipe ID outside the supported range",
                        );
                    };
                    if let Err(e) = client
                        .api_update("recipe", recipe_id, json!({"internal": true}))
                        .await
                    {
                        return tool_err(
                            "Import partially succeeded",
                            format!(
                                "Tandoor created recipe {recipe_id}, but it could not be marked as internally managed: {e}"
                            ),
                        );
                    }
                    return Ok(CallToolResult::success(vec![Content::text(
                        json!({
                            "message": if result.msg.is_empty() { "Recipe imported successfully" } else { &result.msg },
                            "recipe_id": recipe_id,
                            "recipe_url": client.recipe_url(recipe_id_for_url),
                            "duplicates": result.duplicates,
                        })
                        .to_string(),
                    )]));
                }

                let Some(recipe_preview) = result.recipe else {
                    return tool_err(
                        "Import failed",
                        "Tandoor returned neither a recipe preview nor a created recipe ID",
                    );
                };
                let (recipe_body, image_url) = match prepare_source_import_recipe(recipe_preview) {
                    Ok(prepared) => prepared,
                    Err(e) => return tool_err("Import failed", e),
                };
                let created = match client.api_create("recipe", recipe_body).await {
                    Ok(recipe) => recipe,
                    Err(e) => return tool_err("Failed to save imported recipe", e),
                };
                let Some(recipe_id) = created
                    .get("id")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|id| i32::try_from(id).ok())
                else {
                    return tool_err(
                        "Import partially succeeded",
                        "Tandoor created the recipe but returned no usable recipe ID",
                    );
                };

                let image_error = if let Some(image_url) = image_url {
                    match client.download_image(&image_url).await {
                        Ok((bytes, extension)) => client
                            .upload_recipe_image(recipe_id, bytes, extension)
                            .await
                            .err()
                            .map(|e| e.to_string()),
                        Err(e) => Some(e.to_string()),
                    }
                } else {
                    None
                };

                Ok(CallToolResult::success(vec![Content::text(
                    json!({
                        "message": "Recipe imported successfully",
                        "recipe_id": recipe_id,
                        "recipe_url": client.recipe_url(recipe_id),
                        "recipe": created,
                        "duplicates": result.duplicates,
                        "image_error": image_error,
                    })
                    .to_string(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to import recipe", "details": e.to_string()}).to_string(),
            )])),
        }
    }

    // Shopping list tools
    #[tool(
        description = "Add items to the shopping list in plain words, e.g. items: [\"3 lemons\", \"2 lb chicken thighs\", \"milk\", \"1/2 cup parsley\", \"a dozen eggs\"]. Amount and unit are read from the text; foods are matched by name (plurals included) and created if new. If the same food and unit is already on the list it increases that line instead of adding a duplicate (merge_with_existing, default true). For a recipe's ingredients use add_recipe_to_shopping_list.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn add_to_shopping_list(
        &self,
        Parameters(params): Parameters<AddToShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };
        let fail = |message: String| {
            Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to add to shopping list", "details": message}).to_string(),
            )]))
        };
        if params.items.is_empty() {
            return fail("items is empty".to_string());
        }
        let merge = params.merge_with_existing.unwrap_or(true);

        let units = match client.get_units().await {
            Ok(u) => u.results,
            Err(e) => return fail(format!("Failed to load units: {e}")),
        };
        let unit_names: Vec<String> = units.iter().map(|u| u.name.clone()).collect();
        let mut entries = match client.get_all_shopping_entries().await {
            Ok(e) => e,
            Err(e) => return fail(e.to_string()),
        };
        let list = match params.shopping_list {
            None => None,
            Some(target) => match resolve_or_create_shopping_list(&client, &target).await {
                Ok(l) => Some(l),
                Err(e) => return fail(e),
            },
        };

        let mut results = Vec::new();
        let mut errors = Vec::new();
        for input in params.items {
            let (label, parsed) = match input {
                ShoppingItemInput::Text(text) => {
                    let parsed = parse_shopping_text(&text, &unit_names);
                    (text, parsed)
                }
                ShoppingItemInput::Structured(item) => (
                    item.food.clone(),
                    ParsedShoppingItem {
                        food: item.food.trim().to_string(),
                        amount: item.amount,
                        unit: item.unit.filter(|u| !u.trim().is_empty()),
                    },
                ),
            };
            if parsed.food.is_empty() {
                errors.push(json!({"item": label, "error": "No food name"}));
                continue;
            }
            if parsed.amount.is_some_and(|a| a <= 0.0) {
                errors.push(json!({"item": label, "error": "Amount must be positive"}));
                continue;
            }
            let unit = parsed.unit.as_deref().map(|u| resolve_unit_name(u, &units));

            // Existing food (exact name or plural), else create it
            let (food_id, created_food) = match find_food_by_name(&client, &parsed.food).await {
                Ok((Some(id), _)) => (id, false),
                Ok((None, _)) => {
                    let request = crate::client::types::NewFoodRequest {
                        name: parsed.food.clone(),
                        food_onhand: false,
                        supermarket_category: None,
                    };
                    match client.create_food(request).await {
                        Ok(food) => (food.id, true),
                        Err(e) => {
                            errors.push(json!({"item": label, "error": "Failed to create food", "details": e.to_string()}));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    errors.push(json!({"item": label, "error": "Failed to look up food", "details": e.to_string()}));
                    continue;
                }
            };

            let existing = merge
                .then(|| find_mergeable_entry(&entries, food_id, unit.as_deref()))
                .flatten();
            if let Some(entry) = existing {
                let mut body = serde_json::Map::new();
                if let Some(extra) = parsed.amount {
                    body.insert("amount".to_string(), json!(entry.amount + extra));
                    body.insert("checked".to_string(), json!(false));
                }
                if let Some(list) = &list {
                    if !entry.shopping_lists.iter().any(|s| s.id == list.id) {
                        let mut lists = entry.shopping_lists.clone();
                        lists.push(list.clone());
                        body.insert("shopping_lists".to_string(), json!(lists));
                    }
                }
                if body.is_empty() {
                    results.push(json!({
                        "id": entry.id,
                        "food": entry.food.name,
                        "amount": entry.amount,
                        "unit": entry.unit.as_ref().map(|u| &u.name),
                        "status": "already on list"
                    }));
                    continue;
                }
                let status = match parsed.amount {
                    Some(_) => format!("increased from {}", entry.amount),
                    None => "added to list".to_string(),
                };
                let entry_id = entry.id;
                let updated = client
                    .api_update(
                        "shopping-list-entry",
                        entry_id as i64,
                        serde_json::Value::Object(body),
                    )
                    .await
                    .and_then(|v| {
                        Ok(serde_json::from_value::<
                            crate::client::types::ShoppingListEntry,
                        >(v)?)
                    });
                match updated {
                    Ok(updated) => {
                        results.push(json!({
                            "id": updated.id,
                            "food": updated.food.name,
                            "amount": updated.amount,
                            "unit": updated.unit.as_ref().map(|u| &u.name),
                            "status": status
                        }));
                        if let Some(slot) = entries.iter_mut().find(|e| e.id == entry_id) {
                            *slot = updated;
                        }
                    }
                    Err(e) => errors.push(json!({"item": label, "error": "Failed to update existing entry", "details": e.to_string()})),
                }
                continue;
            }

            let lists: Vec<_> = list.iter().cloned().collect();
            match client
                .add_shopping_entry(
                    food_id,
                    unit.as_deref(),
                    parsed.amount.unwrap_or(1.0),
                    &lists,
                )
                .await
            {
                Ok(entry) => {
                    results.push(json!({
                        "id": entry.id,
                        "food": entry.food.name,
                        "amount": entry.amount,
                        "unit": entry.unit.as_ref().map(|u| &u.name),
                        "status": if created_food { "added (new food)" } else { "added" }
                    }));
                    entries.push(entry);
                }
                Err(e) => errors.push(
                    json!({"item": label, "error": "Failed to add", "details": e.to_string()}),
                ),
            }
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json!({
                "items": results,
                "errors": errors,
                "summary": format!("{} items processed, {} errors", results.len(), errors.len())
            }))
            .unwrap(),
        )]))
    }

    #[tool(
        description = "Get current shopping list organized by store section",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_shopping_list(
        &self,
        Parameters(params): Parameters<GetShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in get_shopping_list: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.get_all_shopping_entries().await {
            Ok(mut entries) => {
                if let Some(target) = &params.shopping_list {
                    let list =
                        match resolve_shopping_lists(&client, std::slice::from_ref(target)).await {
                            Ok(mut l) => l.remove(0),
                            Err(e) => return tool_err("Shopping list not found", e),
                        };
                    entries.retain(|e| e.shopping_lists.iter().any(|s| s.id == list.id));
                }
                let total = entries.len();
                let items: Vec<serde_json::Value> = entries
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id,
                            "food": entry.food.name,
                            "amount": entry.amount,
                            "unit": entry.unit.as_ref().map(|u| &u.name),
                            "checked": entry.checked,
                            "category": entry.food.supermarket_category.as_ref().and_then(|c| c.get("name")).cloned(),
                            "from_recipe": entry.list_recipe.is_some(),
                            "lists": entry.shopping_lists.iter().map(|l| &l.name).collect::<Vec<_>>(),
                            "created": entry.created,
                            "completed": entry.completed
                        })
                    })
                    .collect();

                let result = if params.format == "grouped" {
                    let mut unchecked = Vec::new();
                    let mut checked = Vec::new();

                    for item in items {
                        if item
                            .get("checked")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        {
                            checked.push(item);
                        } else {
                            unchecked.push(item);
                        }
                    }

                    json!({
                        "unchecked_items": unchecked,
                        "checked_items": checked,
                        "total_items": total,
                        "format": "grouped"
                    })
                } else {
                    json!({
                        "items": items,
                        "total_items": total,
                        "format": "flat"
                    })
                };

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                tracing::error!("get_shopping_list tool failed: {}", e);
                let error = json!({
                    "error": "Failed to get shopping list",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Search for foods/ingredients with fuzzy name matching",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn search_foods(
        &self,
        Parameters(params): Parameters<SearchFoodsParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in search_foods: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.search_foods(&params.query, params.limit).await {
            Ok(response) => {
                let foods_json: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|food| {
                        json!({
                            "id": food.id,
                            "name": food.name,
                            "plural_name": food.plural_name,
                            "description": food.description,
                            "food_onhand": food.food_onhand,
                            "supermarket_category": food.supermarket_category
                        })
                    })
                    .collect();

                let result = json!({
                    "foods": foods_json,
                    "total_count": response.count,
                    "query": params.query
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                tracing::error!(
                    "search_foods tool failed for query '{}': {}",
                    params.query,
                    e
                );
                let error = json!({
                    "error": "Failed to search foods",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Get all available recipe keywords/tags",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_keywords(&self) -> Result<CallToolResult, McpError> {
        tracing::debug!("MCP tool call: get_keywords");

        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in get_keywords: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.get_keywords().await {
            Ok(response) => {
                tracing::debug!("Successfully retrieved keywords from Tandoor API");
                let keywords_json: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|keyword| {
                        json!({
                            "id": keyword.id,
                            "name": keyword.name,
                            "description": keyword.description
                        })
                    })
                    .collect();

                let result = json!({
                    "keywords": keywords_json,
                    "total_count": response.count
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                tracing::error!("get_keywords tool failed: {}", e);

                // Provide more specific error information
                let error_details = if e.to_string().contains("Not authenticated") {
                    json!({
                        "error": "Authentication Error",
                        "message": "Your authentication token has expired or is invalid",
                        "details": e.to_string(),
                        "suggestion": "Please restart the MCP server to re-authenticate with Tandoor"
                    })
                } else if e.to_string().contains("Failed to connect") {
                    json!({
                        "error": "Connection Error",
                        "message": "Unable to connect to Tandoor server",
                        "details": e.to_string(),
                        "suggestion": "Check that Tandoor is running and accessible at the configured URL"
                    })
                } else {
                    json!({
                        "error": "Failed to get keywords",
                        "message": "An unexpected error occurred while fetching keywords",
                        "details": e.to_string(),
                        "suggestion": "Check server logs for more details"
                    })
                };

                Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error_details).unwrap(),
                )]))
            }
        }
    }

    #[tool(
        description = "Get all existing recipe property types. Use an ID or exact name from this result when setting recipe-level properties.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_property_types(&self) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        match client.list_property_types().await {
            Ok(response) => tool_ok(json!({
                "property_types": response.results.into_iter().map(|property_type| json!({
                    "id": property_type.id,
                    "name": property_type.name,
                    "unit": property_type.unit,
                    "description": property_type.description
                })).collect::<Vec<_>>(),
                "total_count": response.count
            })),
            Err(error) => tool_err("Failed to get property types", error),
        }
    }

    #[tool(
        description = "Get available measurement units",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_units(&self) -> Result<CallToolResult, McpError> {
        tracing::info!("=== MCP tool call: get_units started ===");

        // Ensure we're authenticated before making API calls
        tracing::info!("Checking authentication for get_units");
        let client = match self.ensure_authenticated().await {
            Ok(client) => {
                tracing::info!("Authentication successful, proceeding with get_units API");
                client
            }
            Err(e) => {
                tracing::error!("Authentication failed in get_units: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.get_units().await {
            Ok(response) => {
                tracing::debug!("Successfully retrieved {} units", response.count);
                let units_json: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|unit| {
                        json!({
                            "id": unit.id,
                            "name": unit.name,
                            "plural_name": unit.plural_name,
                            "description": unit.description,
                            "base_unit": unit.base_unit,
                            "type": unit.type_
                        })
                    })
                    .collect();

                let result = json!({
                    "units": units_json,
                    "total_count": response.count
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                tracing::error!("get_units tool failed: {}", e);
                let error = json!({
                    "error": "Failed to get units",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    // Meal planning tools
    #[tool(
        description = "Get meal plans for a date range",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_meal_plans(
        &self,
        Parameters(params): Parameters<GetMealPlansParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in get_meal_plans: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client
            .get_all_meal_plans(Some(&params.from_date), Some(&params.to_date))
            .await
        {
            Ok(meal_plans) => {
                let meal_plans_json: Vec<serde_json::Value> = meal_plans
                    .into_iter()
                    .filter(|plan| {
                        params.meal_type.as_ref().is_none_or(|mt| {
                            plan.meal_type.name.to_lowercase() == mt.to_lowercase()
                        })
                    })
                    .map(|plan| {
                        json!({
                            "id": plan.id,
                            "date": plan.primary_date_label(),
                            "from_date": plan.from_date,
                            "to_date": plan.to_date,
                            "meal_type": plan.meal_type.name,
                            "recipe_id": plan.recipe.as_ref().map(|r| r.id),
                            "recipe_name": plan.recipe.as_ref().map(|r| &r.name),
                            "recipe_url": plan.recipe.as_ref().map(|r| client.recipe_url(r.id)),
                            "title": plan.title,
                            "servings": plan.servings,
                            "note": plan.note,
                            "created": plan.created
                        })
                    })
                    .collect();

                let result = json!({
                    "meal_plans": meal_plans_json,
                    "total_count": meal_plans_json.len(),
                    "date_range": format!("{} to {}", params.from_date, params.to_date),
                    "meal_type_filter": params.meal_type
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to get meal plans",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Render a polished meal-plan card gallery with authoritative recipe photos and tags. Use this instead of get_meal_plans whenever the user asks for a meal-plan summary with images; do not build a separate image layout.",
        annotations(read_only_hint = true, open_world_hint = true),
        meta = meal_plan_gallery_tool_meta()
    )]
    async fn get_meal_plan_gallery(
        &self,
        Parameters(params): Parameters<GetMealPlansParams>,
    ) -> Result<CallToolResult, McpError> {
        let from_date =
            chrono::NaiveDate::parse_from_str(&params.from_date, "%Y-%m-%d").map_err(|error| {
                McpError::invalid_params(
                    "Invalid from_date",
                    Some(json!({"error": error.to_string()})),
                )
            })?;
        let to_date =
            chrono::NaiveDate::parse_from_str(&params.to_date, "%Y-%m-%d").map_err(|error| {
                McpError::invalid_params(
                    "Invalid to_date",
                    Some(json!({"error": error.to_string()})),
                )
            })?;
        if !(0..=366).contains(&(to_date - from_date).num_days()) {
            return Err(McpError::invalid_params(
                "Meal-plan range must be ordered and at most 366 days",
                Some(json!({"from_date": params.from_date, "to_date": params.to_date})),
            ));
        }
        let Some(thumbnails) = self.thumbnails.clone() else {
            return tool_err(
                "Meal-plan galleries are not configured",
                "Set S3_BUCKET and the documented S3 thumbnail credentials",
            );
        };
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(error) => return tool_err("Authentication Error", error),
        };
        let meal_plans = match client
            .get_all_meal_plans(Some(&params.from_date), Some(&params.to_date))
            .await
        {
            Ok(meal_plans) => meal_plans
                .into_iter()
                .filter(|plan| {
                    params
                        .meal_type
                        .as_ref()
                        .is_none_or(|meal_type| plan.meal_type.name.eq_ignore_ascii_case(meal_type))
                })
                .collect::<Vec<_>>(),
            Err(error) => return tool_err("Failed to get meal plans", error),
        };
        if meal_plans.len() > 31 {
            return tool_err(
                "Too many meal-plan entries for one gallery",
                "Narrow the date range or meal_type so at most 31 entries are returned",
            );
        }

        let mut items = Vec::with_capacity(meal_plans.len());
        let mut private_items = Vec::with_capacity(meal_plans.len());
        let mut urls_expire_at = 0_u64;
        for plan in meal_plans {
            let mut item = json!({
                "meal_plan_id": plan.id,
                "date": plan.primary_date_label(),
                "meal_type": plan.meal_type.name,
                "title": plan.title,
                "servings": plan.servings,
                "note": plan.note,
                "image_status": "not_set"
            });
            let Some(summary) = plan.recipe else {
                item["display_name"] = item["title"]
                    .as_str()
                    .map_or_else(|| "Planned meal".into(), Into::into);
                items.push(item);
                continue;
            };
            item["recipe_id"] = json!(summary.id);
            item["recipe_name"] = json!(summary.name);
            item["recipe_url"] = json!(client.recipe_url(summary.id));

            let recipe = match client.get_recipe(summary.id).await {
                Ok(recipe) => recipe,
                Err(error) => {
                    item["display_name"] = item["recipe_name"].clone();
                    item["image_status"] = json!("unavailable");
                    item["error"] = json!(error.to_string());
                    items.push(item);
                    continue;
                }
            };
            let recipe_name = recipe.name.clone();
            item["display_name"] = json!(recipe_name);
            item["description"] = json!(recipe.description);
            item["working_time"] = json!(recipe.working_time);
            item["waiting_time"] = json!(recipe.waiting_time);
            item["total_time"] =
                json!(recipe.working_time.unwrap_or(0) + recipe.waiting_time.unwrap_or(0));
            item["keywords"] = json!(keyword_names(&recipe.keywords));

            let Some(image_path) = recipe.image.clone() else {
                items.push(item);
                continue;
            };
            let image_client = client.clone();
            let fetch_path = image_path.clone();
            match thumbnails
                .thumbnail_for(recipe.id, &image_path, move || async move {
                    image_client
                        .get_recipe_image(&fetch_path)
                        .await
                        .map(|(bytes, _)| bytes)
                })
                .await
            {
                Ok(thumbnail) => {
                    urls_expire_at = if urls_expire_at == 0 {
                        thumbnail.expires_at
                    } else {
                        urls_expire_at.min(thumbnail.expires_at)
                    };
                    private_items.push(json!({
                        "meal_plan_id": plan.id,
                        "recipe_id": recipe.id,
                        "thumbnail_url": thumbnail.url.as_str(),
                        "width": thumbnail.width,
                        "height": thumbnail.height
                    }));
                    item["image_status"] = json!("attached");
                }
                Err(error) => {
                    tracing::warn!(
                        meal_plan_id = plan.id,
                        recipe_id = recipe.id,
                        %error,
                        "Could not prepare meal-plan thumbnail"
                    );
                    item["image_status"] = json!("unavailable");
                    item["error"] = json!(error.to_string());
                }
            }
            items.push(item);
        }

        Ok(gallery_result(
            json!({
                "items": items,
                "from_date": params.from_date,
                "to_date": params.to_date,
                "meal_type_filter": params.meal_type,
                "message": "The attached meal-plan gallery component displays the authoritative recipe photos. Summarize in Markdown with each recipe name linked to its recipe_url, but include no images of any kind (no Markdown images, image lists, carousels, or web-searched or stock photos), never print or reconstruct thumbnail URLs, and do not call get_recipe_image for these recipes."
            }),
            private_items,
            urls_expire_at,
        ))
    }

    #[tool(
        description = "Create a new meal plan",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_meal_plan(
        &self,
        Parameters(params): Parameters<CreateMealPlanParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in create_meal_plan: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        let date = chrono::NaiveDate::parse_from_str(&params.date, "%Y-%m-%d").map_err(|e| {
            McpError::invalid_params(
                "Invalid date format",
                Some(serde_json::json!({"error": e.to_string()})),
            )
        })?;
        // Current Tandoor meal plans use from_date/to_date datetimes; noon UTC keeps the calendar day stable.
        let from_date = date
            .and_hms_opt(12, 0, 0)
            .expect("valid noon time")
            .and_utc();
        let to_date = from_date;

        let recipe = match params.recipe_id {
            Some(id) => match client.get_recipe(id).await {
                Ok(recipe) => Some(crate::client::types::ApiNamedReference {
                    id: recipe.id,
                    name: recipe.name,
                }),
                Err(e) => {
                    return Ok(CallToolResult::error(vec![Content::text(
                        json!({
                            "error": "Failed to resolve meal-plan recipe",
                            "details": e.to_string()
                        })
                        .to_string(),
                    )]));
                }
            },
            None => None,
        };
        let meal_type = match client.get_meal_type(params.meal_type).await {
            Ok(meal_type) => crate::client::types::ApiNamedReference {
                id: meal_type.id,
                name: meal_type.name,
            },
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({
                        "error": "Failed to resolve meal type",
                        "details": e.to_string()
                    })
                    .to_string(),
                )]));
            }
        };

        let request = crate::client::types::CreateMealPlanRequest {
            recipe,
            title: params.title,
            servings: params.servings as f64,
            from_date,
            to_date,
            meal_type,
            note: params.note,
        };

        match client.create_meal_plan(request).await {
            Ok(meal_plan) => {
                let result = json!({
                    "id": meal_plan.id,
                    "date": meal_plan.primary_date_label(),
                    "from_date": meal_plan.from_date,
                    "to_date": meal_plan.to_date,
                    "meal_type": meal_plan.meal_type.name,
                    "recipe_id": meal_plan.recipe.as_ref().map(|r| r.id),
                    "recipe_name": meal_plan.recipe.as_ref().map(|r| &r.name),
                    "recipe_url": meal_plan.recipe.as_ref().map(|r| client.recipe_url(r.id)),
                    "title": meal_plan.title,
                    "servings": meal_plan.servings,
                    "note": meal_plan.note,
                    "created": meal_plan.created,
                    "success": true,
                    "message": "Meal plan created successfully"
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to create meal plan",
                    "details": e.to_string(),
                    "success": false
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Update fields on an existing meal plan (recipe, title, servings, date, meal_type, note) without deleting and recreating it",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_meal_plan(
        &self,
        Parameters(params): Parameters<UpdateMealPlanParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let recipe = match params.recipe_id {
            Some(id) => match client.get_recipe(id).await {
                Ok(recipe) => Some(crate::client::types::ApiNamedReference {
                    id: recipe.id,
                    name: recipe.name,
                }),
                Err(e) => {
                    return Ok(CallToolResult::error(vec![Content::text(
                        json!({
                            "error": "Failed to resolve meal-plan recipe",
                            "details": e.to_string()
                        })
                        .to_string(),
                    )]));
                }
            },
            None => None,
        };
        let meal_type = match params.meal_type {
            Some(id) => match client.get_meal_type(id).await {
                Ok(meal_type) => Some(crate::client::types::ApiNamedReference {
                    id: meal_type.id,
                    name: meal_type.name,
                }),
                Err(e) => {
                    return Ok(CallToolResult::error(vec![Content::text(
                        json!({
                            "error": "Failed to resolve meal type",
                            "details": e.to_string()
                        })
                        .to_string(),
                    )]));
                }
            },
            None => None,
        };

        let mut body = serde_json::Map::new();
        if let Some(recipe) = recipe {
            body.insert("recipe".to_string(), json!(recipe));
        }
        if let Some(v) = params.title {
            body.insert("title".to_string(), json!(v));
        }
        if let Some(v) = params.servings {
            body.insert("servings".to_string(), json!(v));
        }
        if let Some(v) = params.date {
            let date = match chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d") {
                Ok(d) => d,
                Err(e) => {
                    return Err(McpError::invalid_params(
                        "Invalid date format",
                        Some(serde_json::json!({"error": e.to_string()})),
                    ));
                }
            };
            let date_time = date
                .and_hms_opt(12, 0, 0)
                .expect("noon is a valid time")
                .and_utc();
            body.insert("from_date".to_string(), json!(date_time));
            body.insert("to_date".to_string(), json!(date_time));
        }
        if let Some(meal_type) = meal_type {
            body.insert("meal_type".to_string(), json!(meal_type));
        }
        if let Some(v) = params.note {
            body.insert("note".to_string(), json!(v));
        }

        match client
            .update_meal_plan(params.id, serde_json::Value::Object(body))
            .await
        {
            Ok(meal_plan) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&json!({
                    "id": meal_plan.id,
                    "date": meal_plan.primary_date_label(),
                    "from_date": meal_plan.from_date,
                    "to_date": meal_plan.to_date,
                    "meal_type": meal_plan.meal_type.name,
                    "recipe_id": meal_plan.recipe.as_ref().map(|r| r.id),
                    "recipe_name": meal_plan.recipe.as_ref().map(|r| &r.name),
                    "recipe_url": meal_plan.recipe.as_ref().map(|r| client.recipe_url(r.id)),
                    "title": meal_plan.title,
                    "servings": meal_plan.servings,
                    "note": meal_plan.note
                }))
                .unwrap(),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to update meal plan", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "Delete a meal plan",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_meal_plan(
        &self,
        Parameters(params): Parameters<DeleteMealPlanParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in delete_meal_plan: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.delete_meal_plan(params.id).await {
            Ok(_) => {
                let result = json!({
                    "deleted": {
                        "id": params.id
                    },
                    "success": true,
                    "message": "Meal plan deleted successfully"
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to delete meal plan",
                    "details": e.to_string(),
                    "success": false
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Get available meal types",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_meal_types(&self) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in get_meal_types: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.get_meal_types().await {
            Ok(response) => {
                let meal_types_json: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|meal_type| {
                        json!({
                            "id": meal_type.id,
                            "name": meal_type.name,
                            "order": meal_type.order,
                            "icon": meal_type.icon,
                            "color": meal_type.color
                        })
                    })
                    .collect();

                let result = json!({
                    "meal_types": meal_types_json,
                    "total_count": response.count
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to get meal types",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    // Shopping list management tools
    #[tool(
        description = "Check off (mark purchased) shopping list items by food name, e.g. items: [\"lemons\", \"milk\"], or by entry ID. Every line for that food is checked. Pass checked: false to uncheck (put back on the list).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn check_shopping_items(
        &self,
        Parameters(params): Parameters<CheckShoppingItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };
        let entries = match client.get_all_shopping_entries().await {
            Ok(e) => e,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Failed to get shopping list", "details": e.to_string()})
                        .to_string(),
                )]));
            }
        };

        let target = params.checked.unwrap_or(true);
        let mut updated = Vec::new();
        let mut errors = Vec::new();
        for item in params.items {
            let matched = match match_shopping_entries(&entries, &item) {
                Ok(m) => m,
                Err(e) => {
                    errors.push(json!({"item": item, "error": e}));
                    continue;
                }
            };
            let to_change: Vec<_> = matched.iter().filter(|e| e.checked != target).collect();
            if to_change.is_empty() {
                let state = if target {
                    "Already checked off"
                } else {
                    "Not checked off"
                };
                errors.push(json!({"item": item, "error": state}));
                continue;
            }
            for entry in to_change {
                let request = crate::client::types::UpdateShoppingListEntryRequest {
                    checked: Some(target),
                    amount: None,
                };
                match client.update_shopping_list_entry(entry.id, request).await {
                    Ok(e) => updated.push(json!({
                        "id": e.id,
                        "food": e.food.name,
                        "amount": e.amount,
                        "unit": e.unit.as_ref().map(|u| &u.name),
                        "checked": e.checked
                    })),
                    Err(e) => errors.push(json!({"item": item, "error": "Failed to update item", "details": e.to_string()})),
                }
            }
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json!({
                "updated": updated,
                "errors": errors,
                "summary": format!("Updated {} items, {} errors", updated.len(), errors.len())
            }))
            .unwrap(),
        )]))
    }

    #[tool(
        description = "Change one shopping list item by food name (e.g. \"lemons\") or entry ID: amount, unit (\"\" removes it), food (swap for another, e.g. \"Meyer lemons\"), checked status, or which named shopping lists it's on. Other items are untouched.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_shopping_list_item(
        &self,
        Parameters(params): Parameters<UpdateShoppingListItemParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        if params.amount.is_none()
            && params.checked.is_none()
            && params.unit.is_none()
            && params.food.is_none()
            && params.shopping_lists.is_none()
        {
            return tool_err(
                "Nothing to change",
                "Pass amount, unit, food, checked, and/or shopping_lists",
            );
        }
        if params.amount.is_some_and(|a| a <= 0.0) {
            return tool_err("Invalid amount", "amount must be positive");
        }

        let entries = match client.get_all_shopping_entries().await {
            Ok(e) => e,
            Err(e) => return tool_err("Failed to get shopping list", e),
        };
        let entry_id = match match_shopping_entries(&entries, &params.item) {
            Ok(m) if m.len() == 1 => m[0].id,
            Ok(m) => {
                let lines: Vec<_> = m
                    .iter()
                    .map(|e| json!({"id": e.id, "amount": e.amount, "unit": e.unit.as_ref().map(|u| &u.name), "checked": e.checked}))
                    .collect();
                return tool_err(
                    "Item is on the list more than once",
                    format!("Pick one by entry ID: {}", json!(lines)),
                );
            }
            Err(e) => return tool_err("Item not found", e),
        };

        let mut body = serde_json::Map::new();
        if let Some(amount) = params.amount {
            body.insert("amount".to_string(), json!(amount));
        }
        if let Some(checked) = params.checked {
            body.insert("checked".to_string(), json!(checked));
        }
        if let Some(unit) = params.unit {
            let value = if unit.trim().is_empty() {
                serde_json::Value::Null
            } else {
                let units = client
                    .get_units()
                    .await
                    .map(|u| u.results)
                    .unwrap_or_default();
                json!({"name": resolve_unit_name(&unit, &units)})
            };
            body.insert("unit".to_string(), value);
        }
        if let Some(food) = params.food.filter(|f| !f.trim().is_empty()) {
            let food_id = match find_food_by_name(&client, &food).await {
                Ok((Some(id), _)) => id,
                Ok((None, _)) => {
                    let request = crate::client::types::NewFoodRequest {
                        name: food.trim().to_string(),
                        food_onhand: false,
                        supermarket_category: None,
                    };
                    match client.create_food(request).await {
                        Ok(f) => f.id,
                        Err(e) => return tool_err("Failed to create food", e),
                    }
                }
                Err(e) => return tool_err("Failed to look up food", e),
            };
            body.insert("food".to_string(), json!(food_id));
        }
        if let Some(targets) = params.shopping_lists {
            match resolve_shopping_lists(&client, &targets).await {
                Ok(lists) => {
                    body.insert("shopping_lists".to_string(), json!(lists));
                }
                Err(e) => return tool_err("Shopping list not found", e),
            }
        }

        let updated = client
            .api_update(
                "shopping-list-entry",
                entry_id as i64,
                serde_json::Value::Object(body),
            )
            .await
            .and_then(|v| {
                Ok(serde_json::from_value::<
                    crate::client::types::ShoppingListEntry,
                >(v)?)
            });
        match updated {
            Ok(entry) => tool_ok(json!({
                "id": entry.id,
                "food": entry.food.name,
                "amount": entry.amount,
                "unit": entry.unit.as_ref().map(|u| &u.name),
                "checked": entry.checked,
                "lists": entry.shopping_lists.iter().map(|l| &l.name).collect::<Vec<_>>()
            })),
            Err(e) => tool_err("Failed to update shopping list item", e),
        }
    }

    #[tool(
        description = "Remove items from the shopping list by food name (e.g. \"lemons\") or entry ID, without checking them off or touching other items. Every line for that food is removed.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn remove_from_shopping_list(
        &self,
        Parameters(params): Parameters<RemoveFromShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };
        let entries = match client.get_all_shopping_entries().await {
            Ok(e) => e,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Failed to get shopping list", "details": e.to_string()})
                        .to_string(),
                )]));
            }
        };

        let mut removed = Vec::new();
        let mut errors = Vec::new();
        for item in params.items {
            match match_shopping_entries(&entries, &item) {
                Ok(matched) => {
                    for entry in matched {
                        match client.delete_shopping_list_entry(entry.id).await {
                            Ok(_) => removed.push(json!({
                                "id": entry.id,
                                "food": entry.food.name,
                                "amount": entry.amount,
                                "unit": entry.unit.as_ref().map(|u| &u.name)
                            })),
                            Err(e) => errors.push(json!({"item": item, "error": "Failed to remove item", "details": e.to_string()})),
                        }
                    }
                }
                Err(e) => errors.push(json!({"item": item, "error": e})),
            }
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json!({
                "removed": removed,
                "errors": errors,
                "summary": format!("Removed {} items, {} errors", removed.len(), errors.len())
            }))
            .unwrap(),
        )]))
    }

    #[tool(
        description = "Clear checked items from shopping list and update pantry",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn clear_shopping_list(&self) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in clear_shopping_list: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client.get_all_shopping_entries().await {
            Ok(entries) => {
                let mut removed_items = Vec::new();
                let mut pantry_updates = Vec::new();
                let mut errors = Vec::new();

                for entry in entries {
                    if entry.checked {
                        match client.delete_shopping_list_entry(entry.id).await {
                            Ok(_) => {
                                removed_items.push(json!({
                                    "id": entry.id,
                                    "food": entry.food.name,
                                    "amount": entry.amount,
                                    "unit": entry.unit.as_ref().map(|u| &u.name),
                                    "was_checked": entry.checked
                                }));

                                match client.update_food_availability(entry.food.id, true).await {
                                    Ok(_) => {
                                        pantry_updates.push(entry.food.name.clone());
                                    }
                                    Err(e) => {
                                        errors.push(json!({
                                            "food": entry.food.name,
                                            "error": "Failed to update pantry",
                                            "details": e.to_string()
                                        }));
                                    }
                                }
                            }
                            Err(e) => {
                                errors.push(json!({
                                    "food": entry.food.name,
                                    "error": "Failed to remove from shopping list",
                                    "details": e.to_string()
                                }));
                            }
                        }
                    }
                }

                let result = json!({
                    "removed_items": removed_items,
                    "pantry_updates": pantry_updates,
                    "errors": errors,
                    "summary": format!("Removed {} checked items, updated pantry for {} items",
                        removed_items.len(), pantry_updates.len())
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to get shopping list",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    // Inventory management tools
    #[tool(
        description = "Update pantry inventory status (on hand / not on hand). Tandoor tracks only whether a food is on hand, not how much. Foods are matched by exact name or plural (case-insensitive); foods that don't exist yet are created when marked available, unless create_missing is false.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_pantry(
        &self,
        Parameters(params): Parameters<UpdatePantryParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in update_pantry: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        let mut updated = Vec::new();
        let mut errors = Vec::new();

        let create_missing = params.create_missing.unwrap_or(true);

        for item in params.items {
            match find_food_by_name(&client, &item.food).await {
                Ok((existing, candidates)) => {
                    // Resolve to (food id, created), creating the food if allowed
                    let resolved = match existing {
                        Some(id) => Ok((id, false)),
                        None if item.available && create_missing => {
                            let request = crate::client::types::NewFoodRequest {
                                name: item.food.trim().to_string(),
                                food_onhand: true,
                                supermarket_category: item.supermarket_category.clone().map(
                                    |name| crate::client::types::SupermarketCategoryRef { name },
                                ),
                            };
                            client
                                .create_food(request)
                                .await
                                .map(|f| (f.id, true))
                                .map_err(|e| {
                                    json!({
                                        "food": item.food,
                                        "error": "Failed to create food",
                                        "details": e.to_string()
                                    })
                                })
                        }
                        None => Err(json!({
                            "food": item.food,
                            "error": "Food not found",
                            "similar_foods": candidates,
                            "suggestion": "Use the exact name of an existing food, or mark it available with create_missing enabled to create it"
                        })),
                    };

                    let (food_id, created) = match resolved {
                        Ok(r) => r,
                        Err(err) => {
                            errors.push(err);
                            continue;
                        }
                    };

                    // Always patch: Tandoor's create returns an already-existing food unchanged
                    match client
                        .update_food_availability(food_id, item.available)
                        .await
                    {
                        Ok(updated_food) => {
                            updated.push(json!({
                                "id": updated_food.id,
                                "name": updated_food.name,
                                "available": updated_food.food_onhand,
                                "status": if created { "created" } else { "updated" }
                            }));
                        }
                        Err(e) => {
                            errors.push(json!({
                                "food": item.food,
                                "error": "Failed to update availability",
                                "details": e.to_string()
                            }));
                        }
                    }
                }
                Err(e) => {
                    errors.push(json!({
                        "food": item.food,
                        "error": "Failed to search for food",
                        "details": e.to_string()
                    }));
                }
            }
        }

        let result = json!({
            "updated": updated,
            "errors": errors,
            "summary": format!("Updated {} items, {} errors", updated.len(), errors.len())
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&result).unwrap(),
        )]))
    }

    // Recipe history tools
    #[tool(
        description = "Get cooking history",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_cook_log(
        &self,
        Parameters(params): Parameters<GetCookLogParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in get_cook_log: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        match client
            .get_cook_log(params.recipe_id, Some(params.days_back))
            .await
        {
            Ok(response) => {
                let cook_log_json: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|log| {
                        json!({
                            "id": log.id,
                            "recipe_id": log.recipe.id,
                            "recipe_name": log.recipe.name,
                            "recipe_url": client.recipe_url(log.recipe.id),
                            "servings": log.servings,
                            "rating": log.rating,
                            "comment": log.comment,
                            "created": log.created,
                            "date_cooked": log.created.format("%Y-%m-%d").to_string()
                        })
                    })
                    .collect();

                let result = json!({
                    "cook_log": cook_log_json,
                    "total_count": response.count,
                    "days_back": params.days_back,
                    "recipe_filter": params.recipe_id
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to get cook log",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Log a cooked recipe",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn log_cooked_recipe(
        &self,
        Parameters(params): Parameters<LogCookedRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in log_cooked_recipe: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        let request = crate::client::types::CreateCookLogRequest {
            recipe: params.recipe_id,
            servings: params.servings,
            rating: params.rating,
            comment: params.comment,
        };

        match client.log_cooked_recipe(request).await {
            Ok(cook_log) => {
                let result = json!({
                    "id": cook_log.id,
                    "recipe_id": cook_log.recipe.id,
                    "recipe_name": cook_log.recipe.name,
                    "recipe_url": client.recipe_url(cook_log.recipe.id),
                    "servings": cook_log.servings,
                    "rating": cook_log.rating,
                    "comment": cook_log.comment,
                    "created": cook_log.created,
                    "success": true,
                    "message": "Recipe cooking logged successfully"
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap(),
                )]))
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to log cooked recipe",
                    "details": e.to_string(),
                    "success": false
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Get recipe suggestions based on current inventory",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn suggest_from_inventory(
        &self,
        Parameters(params): Parameters<SuggestFromInventoryParams>,
    ) -> Result<CallToolResult, McpError> {
        // Ensure we're authenticated before making API calls
        let client = match self.ensure_authenticated().await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Authentication failed in suggest_from_inventory: {}", e);
                let error = json!({
                    "error": "Authentication Error",
                    "message": "Failed to authenticate with Tandoor",
                    "details": e.to_string(),
                    "suggestion": "Check your Tandoor credentials and server connectivity"
                });
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string_pretty(&error).unwrap(),
                )]));
            }
        };

        // Get available foods in pantry
        match client.search_foods("", Some(100)).await {
            Ok(foods_response) => {
                let available_foods: Vec<&crate::client::types::Food> = foods_response
                    .results
                    .iter()
                    .filter(|food| food.food_onhand || food.substitute_onhand)
                    .collect();

                if available_foods.is_empty() {
                    let result = json!({
                        "suggestions": [],
                        "message": "No ingredients found in pantry. Update your inventory first.",
                        "mode": params.mode
                    });
                    return Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&result).unwrap(),
                    )]));
                }

                // Search for recipes that can use these ingredients
                match client
                    .search_recipes(None, Some(20), None, None, None, None, None)
                    .await
                {
                    Ok(recipes_response) => {
                        let mut recipe_suggestions = Vec::new();

                        for recipe in recipes_response.results {
                            // Get recipe details to check ingredients
                            if let Ok(detailed_recipe) = client.get_recipe(recipe.id).await {
                                let mut matching_ingredients = 0;
                                let mut total_ingredients = 0;
                                let mut missing_ingredients = Vec::new();

                                for step in &detailed_recipe.steps {
                                    for ingredient in &step.ingredients {
                                        if !ingredient.is_header && !ingredient.no_amount {
                                            total_ingredients += 1;

                                            let ingredient_available =
                                                available_foods.iter().any(|food| {
                                                    food.name.to_lowercase()
                                                        == ingredient.food_name().to_lowercase()
                                                });

                                            if ingredient_available {
                                                matching_ingredients += 1;
                                            } else {
                                                missing_ingredients
                                                    .push(ingredient.food_name().to_string());
                                            }
                                        }
                                    }
                                }

                                if total_ingredients > 0 {
                                    let match_percentage = (matching_ingredients as f64
                                        / total_ingredients as f64)
                                        * 100.0;

                                    // Filter based on mode
                                    let should_include = match params.mode.as_str() {
                                        "maximum-use" => match_percentage >= 50.0, // At least 50% match
                                        "expiring" => {
                                            match_percentage >= 30.0
                                                && missing_ingredients.len() <= 3
                                        } // Good match with few missing items
                                        _ => match_percentage >= 60.0,
                                    };

                                    if should_include {
                                        let reason = if params.mode == "expiring" {
                                            format!("Uses {:.0}% of pantry ingredients, only {} missing items", match_percentage, missing_ingredients.len())
                                        } else {
                                            format!(
                                                "Uses {match_percentage:.0}% of available ingredients"
                                            )
                                        };

                                        recipe_suggestions.push(json!({
                                            "recipe_id": recipe.id,
                                            "recipe_name": recipe.name,
                                            "recipe_url": client.recipe_url(recipe.id),
                                            "match_percentage": match_percentage,
                                            "matching_ingredients": matching_ingredients,
                                            "total_ingredients": total_ingredients,
                                            "missing_ingredients": missing_ingredients,
                                            "reason": reason,
                                            "total_time": recipe.working_time.unwrap_or(0) + recipe.waiting_time.unwrap_or(0)
                                        }));
                                    }
                                }
                            }
                        }

                        // Sort by match percentage
                        recipe_suggestions.sort_by(|a, b| {
                            let a_match = a
                                .get("match_percentage")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            let b_match = b
                                .get("match_percentage")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            b_match
                                .partial_cmp(&a_match)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });

                        // Take top 10
                        recipe_suggestions.truncate(10);

                        let result = json!({
                            "suggestions": recipe_suggestions,
                            "available_ingredients": available_foods.iter().map(|f| &f.name).collect::<Vec<_>>(),
                            "mode": params.mode,
                            "total_available": available_foods.len(),
                            "message": format!("Found {} recipe suggestions using your {} available ingredients",
                                recipe_suggestions.len(), available_foods.len())
                        });

                        Ok(CallToolResult::success(vec![Content::text(
                            serde_json::to_string_pretty(&result).unwrap(),
                        )]))
                    }
                    Err(e) => {
                        let error = json!({
                            "error": "Failed to search recipes",
                            "details": e.to_string()
                        });
                        Ok(CallToolResult::error(vec![Content::text(
                            error.to_string(),
                        )]))
                    }
                }
            }
            Err(e) => {
                let error = json!({
                    "error": "Failed to get inventory",
                    "details": e.to_string()
                });
                Ok(CallToolResult::error(vec![Content::text(
                    error.to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Update an existing recipe and mark it as internally managed. Top-level fields (name, description, servings, servings_text, cooking_time, waiting_time, source_url, show_ingredient_overview), nutrition, and properties change only when passed. Call get_property_types before setting properties. Nutrition updates are partial; remove_nutrition clears it. Properties can be upserted or removed by type. Keywords support targeted add/remove or full replacement. Steps support targeted edits, inserts, moves, removals, or full replacement. Call get_recipe_details first for step numbers.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_recipe(
        &self,
        Parameters(params): Parameters<UpdateRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let mut body = recipe_scalar_body(&params);
        let reject = |message: String| {
            Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to update recipe", "details": message}).to_string(),
            )]))
        };

        let updates = params.step_updates.unwrap_or_default();
        let adds = params.add_steps.unwrap_or_default();
        let removes = params.remove_steps.unwrap_or_default();
        let moves = params.move_steps.unwrap_or_default();
        let targeted_steps =
            !updates.is_empty() || !adds.is_empty() || !removes.is_empty() || !moves.is_empty();
        let add_kw = params.add_keywords.unwrap_or_default();
        let remove_kw = params.remove_keywords.unwrap_or_default();
        let targeted_keywords = !add_kw.is_empty() || !remove_kw.is_empty();
        let nutrition_change = params.nutrition.is_some() || params.remove_nutrition == Some(true);
        let set_properties = params.set_properties.unwrap_or_default();
        let remove_properties = params.remove_properties.unwrap_or_default();
        let property_change = !set_properties.is_empty() || !remove_properties.is_empty();

        if params.replace_all_steps.is_some() && targeted_steps {
            return reject(
                "replace_all_steps cannot be combined with step_updates/add_steps/remove_steps/move_steps"
                    .to_string(),
            );
        }
        if params.replace_all_keywords.is_some() && targeted_keywords {
            return reject(
                "replace_all_keywords cannot be combined with add_keywords/remove_keywords"
                    .to_string(),
            );
        }
        if params.nutrition.is_some() && params.remove_nutrition == Some(true) {
            return reject("nutrition and remove_nutrition cannot be combined".to_string());
        }

        // Targeted edits are resolved against the recipe as it is now
        let current = if targeted_steps || targeted_keywords || nutrition_change || property_change
        {
            match client.get_recipe(params.id).await {
                Ok(r) => Some(r),
                Err(e) => return reject(format!("Failed to load recipe: {e}")),
            }
        } else {
            None
        };

        if params.remove_nutrition == Some(true) {
            body.insert("nutrition".to_string(), serde_json::Value::Null);
        } else if let Some(input) = params.nutrition.as_ref() {
            let request = merge_nutrition(
                current
                    .as_ref()
                    .and_then(|recipe| recipe.nutrition.as_ref()),
                input,
            );
            body.insert(
                "nutrition".to_string(),
                serde_json::to_value(request).expect("NutritionRequest always serializes"),
            );
        }

        if property_change {
            let property_types = if set_properties.is_empty() {
                Vec::new()
            } else {
                match client.list_property_types().await {
                    Ok(response) => response.results,
                    Err(error) => return reject(format!("Failed to load property types: {error}")),
                }
            };
            let Some(current_recipe) = current.as_ref() else {
                return reject("Failed to load current recipe properties".to_string());
            };
            match plan_property_changes(
                &current_recipe.properties,
                &set_properties,
                &remove_properties,
                &property_types,
            ) {
                Ok(properties) => {
                    body.insert(
                        "properties".to_string(),
                        serde_json::to_value(properties)
                            .expect("PropertyRequest always serializes"),
                    );
                }
                Err(error) => return reject(error),
            }
        }

        if let Some(kws) = params.replace_all_keywords {
            let kw_list: Vec<serde_json::Value> = kws.iter().map(|id| json!({"id": id})).collect();
            body.insert("keywords".to_string(), json!(kw_list));
        } else if let (true, Some(current)) = (targeted_keywords, &current) {
            let mut known = std::collections::HashMap::new();
            for name in &add_kw {
                let key = name.trim().to_lowercase();
                match client.search_keywords(name.trim()).await {
                    Ok(found) => {
                        if let Some(k) = found
                            .results
                            .iter()
                            .find(|k| k.name.trim().to_lowercase() == key)
                        {
                            known.insert(key, k.id);
                        }
                    }
                    Err(e) => return reject(format!("Failed to look up keyword '{name}': {e}")),
                }
            }
            match plan_keyword_changes(&current.keywords, &add_kw, &remove_kw, &known) {
                Ok(list) => {
                    body.insert("keywords".to_string(), list);
                }
                Err(message) => return reject(message),
            }
        }

        let mut step_patches = Vec::new();
        if let Some(step_inputs) = params.replace_all_steps {
            if let Err(e) = validate_steps(&step_inputs) {
                return reject(e);
            }
            let steps = build_step_requests(step_inputs);
            body.insert(
                "steps".to_string(),
                serde_json::to_value(steps).expect("CreateStepRequest always serializes"),
            );
        } else if let (true, Some(current)) = (targeted_steps, &current) {
            let plan = match plan_step_changes(&current.steps, updates, adds, removes, moves) {
                Ok(p) => p,
                Err(message) => return reject(message),
            };
            if let Some(steps) = plan.steps_body {
                body.insert("steps".to_string(), steps);
            }
            step_patches = plan.patches;
        }

        // Recipe first (it's where validation usually fails), then the per-step patches
        let mut recipe = match client
            .update_recipe(params.id, serde_json::Value::Object(body))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Failed to update recipe", "details": e.to_string()})
                        .to_string(),
                )]));
            }
        };
        if !step_patches.is_empty() {
            let total = step_patches.len();
            for (applied, (step_id, patch)) in step_patches.into_iter().enumerate() {
                if let Err(e) = client.update_step(step_id, patch).await {
                    return reject(format!(
                        "{e} (recipe fields and {applied} of {total} step updates were applied before this failure)"
                    ));
                }
            }
            match client.get_recipe(params.id).await {
                Ok(r) => recipe = r,
                Err(e) => return reject(format!("Steps updated, but reloading failed: {e}")),
            }
        }

        let mut result = recipe_fields_view(&recipe, &client);
        result.insert(
            "steps".to_string(),
            json!(steps_view(&recipe.steps, 1.0, &client)),
        );
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::Value::Object(result)).unwrap(),
        )]))
    }

    #[tool(
        description = "Find exact-name recipe-as-ingredient cleanup candidates. This is read-only. Each candidate can become either a local step embed with link_step_recipe or a global Food link with link_food_recipe. Duplicate recipe names are reported as ambiguous and never selected automatically.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn find_unlinked_recipe_ingredients(
        &self,
        Parameters(params): Parameters<FindUnlinkedRecipeIngredientsParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let recipes = match client.list_all_recipes().await {
            Ok(recipes) => recipes,
            Err(e) => return tool_err("Failed to scan recipes", e),
        };
        if let Some(ids) = params.recipe_ids.as_deref() {
            let missing: Vec<_> = ids
                .iter()
                .filter(|id| !recipes.iter().any(|recipe| recipe.id == **id))
                .copied()
                .collect();
            if !missing.is_empty() {
                return tool_err(
                    "Unknown parent recipe ids",
                    format!("Recipes not found: {missing:?}"),
                );
            }
        }
        let (candidates, ambiguous) =
            find_relation_candidates(&recipes, params.recipe_ids.as_deref(), &client);
        tool_ok(json!({
            "candidate_count": candidates.len(),
            "ambiguous_count": ambiguous.len(),
            "candidates": candidates,
            "ambiguous": ambiguous,
            "matching": "trimmed, case-insensitive exact recipe/food name",
            "next_step": "Explain that step embeds are local while Food links are global, then ask which relation type to apply."
        }))
    }

    #[tool(
        description = "Link a recipe into one existing parent step. This relation is local to that parent recipe. Refuses to replace a different existing relation; unlink it explicitly first.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn link_step_recipe(
        &self,
        Parameters(params): Parameters<LinkStepRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.parent_recipe_id == params.linked_recipe_id {
            return tool_err("Invalid recipe relation", "A recipe cannot link to itself");
        }
        let client = auth_or_return!(self);
        let parent = match client.get_recipe(params.parent_recipe_id).await {
            Ok(recipe) => recipe,
            Err(e) => return tool_err("Parent recipe not found", e),
        };
        let target = match client.get_recipe(params.linked_recipe_id).await {
            Ok(recipe) => recipe,
            Err(e) => return tool_err("Linked recipe not found", e),
        };
        let Some(step) = parent.steps.iter().find(|step| step.id == params.step_id) else {
            return tool_err(
                "Step does not belong to parent recipe",
                format!(
                    "Recipe {} has no step {}",
                    params.parent_recipe_id, params.step_id
                ),
            );
        };
        if step.step_recipe == Some(target.id) {
            return tool_ok(json!({
                "status": "already_linked",
                "parent_recipe_id": parent.id,
                "step_id": step.id,
                "linked_recipe_id": target.id,
                "scope": "local_step"
            }));
        }
        if let Some(existing) = step.step_recipe {
            return tool_err(
                "Step already links a different recipe",
                format!("Existing linked recipe is {existing}; unlink it first"),
            );
        }
        if let Err(e) = client.set_step_recipe(step.id, Some(target.id)).await {
            return tool_err("Failed to link step recipe", e);
        }
        tool_ok(json!({
            "status": "linked",
            "parent_recipe": {"id": parent.id, "name": parent.name},
            "step_id": step.id,
            "linked_recipe": {
                "id": target.id,
                "name": target.name,
                "recipe_url": client.recipe_url(target.id)
            },
            "scope": "local_step"
        }))
    }

    #[tool(
        description = "Remove a recipe relation from one parent step. This is destructive but idempotent and does not delete either recipe.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn unlink_step_recipe(
        &self,
        Parameters(params): Parameters<UnlinkStepRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let parent = match client.get_recipe(params.parent_recipe_id).await {
            Ok(recipe) => recipe,
            Err(e) => return tool_err("Parent recipe not found", e),
        };
        let Some(step) = parent.steps.iter().find(|step| step.id == params.step_id) else {
            return tool_err(
                "Step does not belong to parent recipe",
                format!(
                    "Recipe {} has no step {}",
                    params.parent_recipe_id, params.step_id
                ),
            );
        };
        if step.step_recipe.is_none() {
            return tool_ok(json!({"status": "already_unlinked", "step_id": step.id}));
        }
        if let Err(e) = client.set_step_recipe(step.id, None).await {
            return tool_err("Failed to unlink step recipe", e);
        }
        tool_ok(json!({
            "status": "unlinked",
            "parent_recipe_id": parent.id,
            "step_id": step.id
        }))
    }

    #[tool(
        description = "Link a Food record to a recipe. This is a GLOBAL relation: every ingredient occurrence using this Food displays the recipe link. Refuses to replace a different existing relation.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn link_food_recipe(
        &self,
        Parameters(params): Parameters<LinkFoodRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let food = match client.get_food(params.food_id).await {
            Ok(food) => food,
            Err(e) => return tool_err("Food not found", e),
        };
        let target = match client.get_recipe(params.linked_recipe_id).await {
            Ok(recipe) => recipe,
            Err(e) => return tool_err("Linked recipe not found", e),
        };
        if food.recipe.as_ref().map(|recipe| recipe.id()) == Some(target.id) {
            return tool_ok(json!({
                "status": "already_linked",
                "food_id": food.id,
                "linked_recipe_id": target.id,
                "scope": "global_food"
            }));
        }
        if let Some(existing) = food.recipe.as_ref() {
            return tool_err(
                "Food already links a different recipe",
                format!(
                    "Existing linked recipe is {}; unlink it first",
                    existing.id()
                ),
            );
        }
        if let Err(e) = client
            .set_food_recipe(
                food.id,
                Some(crate::client::types::NamedRef {
                    id: target.id,
                    name: target.name.clone(),
                }),
            )
            .await
        {
            return tool_err("Failed to link food recipe", e);
        }
        tool_ok(json!({
            "status": "linked",
            "food": {"id": food.id, "name": food.name},
            "linked_recipe": {
                "id": target.id,
                "name": target.name,
                "recipe_url": client.recipe_url(target.id)
            },
            "scope": "global_food",
            "warning": "This relation affects every ingredient occurrence using this Food record."
        }))
    }

    #[tool(
        description = "Clear the global recipe relation from a Food record. This is destructive but idempotent; ingredient occurrences and both resources remain.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn unlink_food_recipe(
        &self,
        Parameters(params): Parameters<UnlinkFoodRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let food = match client.get_food(params.food_id).await {
            Ok(food) => food,
            Err(e) => return tool_err("Food not found", e),
        };
        if food.recipe.is_none() {
            return tool_ok(json!({"status": "already_unlinked", "food_id": food.id}));
        }
        if let Err(e) = client.set_food_recipe(food.id, None).await {
            return tool_err("Failed to unlink food recipe", e);
        }
        tool_ok(json!({
            "status": "unlinked",
            "food_id": food.id,
            "scope": "global_food"
        }))
    }

    #[tool(
        description = "Insert an ingredient-list section header into an existing step, optionally before a stable ingredient id. Existing ingredients and values are preserved.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn add_ingredient_header(
        &self,
        Parameters(params): Parameters<AddIngredientHeaderParams>,
    ) -> Result<CallToolResult, McpError> {
        let header = params.header.trim();
        if header.is_empty() {
            return tool_err("Invalid ingredient header", "Header text cannot be empty");
        }
        let client = auth_or_return!(self);
        let parent = match client.get_recipe(params.parent_recipe_id).await {
            Ok(recipe) => recipe,
            Err(e) => return tool_err("Parent recipe not found", e),
        };
        let Some(step) = parent.steps.iter().find(|step| step.id == params.step_id) else {
            return tool_err(
                "Step does not belong to parent recipe",
                format!(
                    "Recipe {} has no step {}",
                    params.parent_recipe_id, params.step_id
                ),
            );
        };
        let patch = match build_header_patch(step, header, params.before_ingredient_id) {
            Ok(patch) => patch,
            Err(e) => return tool_err("Invalid ingredient position", e),
        };
        let existing_ids: std::collections::HashSet<_> = step
            .ingredients
            .iter()
            .map(|ingredient| ingredient.id)
            .collect();
        let updated = match client
            .update_step(step.id, json!({"ingredients": patch}))
            .await
        {
            Ok(step) => step,
            Err(e) => return tool_err("Failed to add ingredient header", e),
        };
        let created_id = updated
            .ingredients
            .iter()
            .find(|ingredient| {
                !existing_ids.contains(&ingredient.id)
                    && ingredient.is_header
                    && ingredient.note.as_deref() == Some(header)
            })
            .map(|ingredient| ingredient.id);
        tool_ok(json!({
            "status": "added",
            "parent_recipe_id": parent.id,
            "step_id": step.id,
            "header": header,
            "ingredient_id": created_id,
            "before_ingredient_id": params.before_ingredient_id
        }))
    }

    #[tool(
        description = "Set whether the authenticated Tandoor account automatically includes related recipes when building meal-plan shopping lists. This changes one account-wide preference.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_related_recipe_mealplan_inclusion(
        &self,
        Parameters(params): Parameters<SetRelatedRecipeMealplanInclusionParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        match client
            .set_related_recipe_mealplan_inclusion(params.enabled)
            .await
        {
            Ok(preference) => tool_ok(json!({
                "status": "updated",
                "user_id": preference.user.id(),
                "mealplan_autoinclude_related": preference.mealplan_autoinclude_related,
                "scope": "authenticated_account"
            })),
            Err(e) => tool_err("Failed to update related-recipe meal-plan inclusion", e),
        }
    }

    #[tool(
        description = "Set a recipe's photo from a public HTTPS image URL. The URL must point directly at a JPEG, PNG, WebP, or GIF file (not a web page). Private/reserved hosts are rejected and downloads are limited to 20 MB. Replaces any existing photo. For a photo attached to the chat, use set_recipe_image_from_file instead.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_recipe_image(
        &self,
        Parameters(params): Parameters<SetRecipeImageParams>,
    ) -> Result<CallToolResult, McpError> {
        self.store_recipe_image(params.recipe_id, params.image_url.trim(), "url")
            .await
    }

    #[tool(
        description = "Set a recipe's photo from an image attached to this chat: a photo the user uploaded or an image generated in the conversation. Use this instead of set_recipe_image whenever the photo is an attachment rather than a public URL. JPEG, PNG, WebP, or GIF up to 20 MB. Replaces any existing photo.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        ),
        meta = file_image_tool_meta()
    )]
    async fn set_recipe_image_from_file(
        &self,
        Parameters(params): Parameters<SetRecipeImageFromFileParams>,
    ) -> Result<CallToolResult, McpError> {
        // Some ChatGPT clients (observed on the mobile apps) send a placeholder
        // string such as "chat_upload" or "attached" instead of the file object.
        // Name the problem so the model can tell the user what to do.
        let file: ChatFile = match serde_json::from_value(params.image.clone()) {
            Ok(file) => file,
            Err(error) => {
                // Log the shape only: a value could be a signed download URL.
                let shape = match &params.image {
                    serde_json::Value::Object(map) => {
                        format!("object with keys {:?}", map.keys().collect::<Vec<_>>())
                    }
                    serde_json::Value::String(s) => format!("string of {} chars", s.len()),
                    serde_json::Value::Array(items) => format!("array of {} items", items.len()),
                    serde_json::Value::Null => "null".to_string(),
                    serde_json::Value::Bool(_) => "boolean".to_string(),
                    serde_json::Value::Number(_) => "number".to_string(),
                };
                tracing::warn!(
                    recipe_id = params.recipe_id,
                    %shape,
                    %error,
                    "set_recipe_image_from_file received no usable file object"
                );
                return tool_err(
                    "The attached photo was not handed over",
                    "ChatGPT sent a placeholder instead of the file. This happens in the ChatGPT mobile apps; retry from the web or desktop app, or provide a public HTTPS image URL for set_recipe_image.",
                );
            }
        };
        tracing::info!(
            recipe_id = params.recipe_id,
            file_id = %file.file_id,
            mime_type = file.mime_type.as_deref().unwrap_or("unknown"),
            file_name = file.file_name.as_deref().unwrap_or("unknown"),
            "Setting recipe photo from chat attachment"
        );
        self.store_recipe_image(params.recipe_id, &file.download_url, "chat_file")
            .await
    }

    /// Shared body of the two photo-setting tools: download from a public HTTPS
    /// URL (a user-supplied link or ChatGPT's signed attachment URL), upload to
    /// Tandoor, and drop the cached thumbnail of the replaced photo. Failures
    /// are logged so operators can see why a photo did not land; the tool error
    /// alone is only visible to the model.
    async fn store_recipe_image(
        &self,
        recipe_id: i32,
        image_url: &str,
        source: &'static str,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let recipe = match client.get_recipe(recipe_id).await {
            Ok(r) => r,
            Err(e) => return tool_err("Recipe not found", e),
        };
        let (bytes, extension) = match client.download_image(image_url).await {
            Ok(d) => d,
            Err(error) => {
                tracing::warn!(recipe_id, source, %error, "Could not download recipe photo");
                return tool_err("Could not use that image", error);
            }
        };
        let size = bytes.len();
        if let Err(error) = client
            .upload_recipe_image(recipe.id, bytes, extension)
            .await
        {
            tracing::warn!(recipe_id, source, %error, "Could not upload recipe photo to Tandoor");
            return tool_err("Failed to upload image", error);
        }
        if let (Some(thumbnails), Some(previous)) = (&self.thumbnails, recipe.image.as_deref()) {
            if let Err(error) = thumbnails.invalidate(recipe.id, previous).await {
                tracing::warn!(recipe_id = recipe.id, %error, "Could not invalidate old thumbnail");
            }
        }
        let image = client
            .get_recipe(recipe.id)
            .await
            .ok()
            .and_then(|r| r.image);
        tracing::info!(
            recipe_id,
            source,
            format = extension,
            bytes = size,
            "Recipe photo updated"
        );
        tool_ok(json!({
            "recipe": recipe.name,
            "image": image,
            "replaced_previous": recipe.image.is_some(),
            "format": extension,
            "bytes": size
        }))
    }

    #[tool(
        description = "Remove a recipe's photo",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn remove_recipe_image(
        &self,
        Parameters(params): Parameters<RemoveRecipeImageParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let recipe = match client.get_recipe(params.recipe_id).await {
            Ok(r) => r,
            Err(e) => return tool_err("Recipe not found", e),
        };
        if recipe.image.is_none() {
            return tool_ok(json!({"recipe": recipe.name, "message": "Recipe has no photo"}));
        }
        match client.clear_recipe_image(recipe.id).await {
            Ok(()) => {
                if let (Some(thumbnails), Some(previous)) =
                    (&self.thumbnails, recipe.image.as_deref())
                {
                    if let Err(error) = thumbnails.invalidate(recipe.id, previous).await {
                        tracing::warn!(recipe_id = recipe.id, %error, "Could not invalidate removed thumbnail");
                    }
                }
                tool_ok(json!({"recipe": recipe.name, "message": "Photo removed"}))
            }
            Err(e) => tool_err("Failed to remove photo", e),
        }
    }

    #[tool(
        description = "Delete a recipe permanently",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_recipe(
        &self,
        Parameters(params): Parameters<DeleteRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let previous_image = client
            .get_recipe(params.id)
            .await
            .ok()
            .and_then(|recipe| recipe.image);
        match client.delete_recipe(params.id).await {
            Ok(()) => {
                if let (Some(thumbnails), Some(previous)) =
                    (&self.thumbnails, previous_image.as_deref())
                {
                    if let Err(error) = thumbnails.invalidate(params.id, previous).await {
                        tracing::warn!(recipe_id = params.id, %error, "Could not invalidate deleted recipe thumbnail");
                    }
                }
                Ok(CallToolResult::success(vec![Content::text(
                    json!({"message": format!("Recipe {} deleted successfully", params.id)})
                        .to_string(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to delete recipe", "details": e.to_string()}).to_string(),
            )])),
        }
    }

    #[tool(
        description = "List all recipe books/collections, optionally filtered by name",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_recipe_books(
        &self,
        Parameters(params): Parameters<GetRecipeBooksParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        match client.get_recipe_books(params.query.as_deref()).await {
            Ok(response) => {
                let books_json: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|b| {
                        json!({"id": b.id, "name": b.name, "description": b.description, "order": b.order})
                    })
                    .collect();
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(
                        &json!({"books": books_json, "total": response.count}),
                    )
                    .unwrap(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to get recipe books", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "Create a new recipe book/collection",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_recipe_book(
        &self,
        Parameters(params): Parameters<CreateRecipeBookParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let request = crate::client::types::CreateRecipeBookRequest {
            name: params.name,
            description: params.description,
            shared: vec![],
        };

        match client.create_recipe_book(request).await {
            Ok(book) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(
                    &json!({"id": book.id, "name": book.name, "description": book.description}),
                )
                .unwrap(),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to create recipe book", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "Rename or update the description of a recipe book/collection",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_recipe_book(
        &self,
        Parameters(params): Parameters<UpdateRecipeBookParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let mut body = serde_json::Map::new();
        if let Some(v) = params.name {
            body.insert("name".to_string(), json!(v));
        }
        if let Some(v) = params.description {
            body.insert("description".to_string(), json!(v));
        }

        match client
            .update_recipe_book(params.id, serde_json::Value::Object(body))
            .await
        {
            Ok(book) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(
                    &json!({"id": book.id, "name": book.name, "description": book.description}),
                )
                .unwrap(),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to update recipe book", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "Delete a recipe book/collection by ID",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_recipe_book(
        &self,
        Parameters(params): Parameters<DeleteRecipeBookParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        match client.delete_recipe_book(params.id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                json!({"message": format!("Recipe book {} deleted", params.id)}).to_string(),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to delete recipe book", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "Add a recipe to a recipe book. Returns the entry ID needed to remove it later.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn add_to_recipe_book(
        &self,
        Parameters(params): Parameters<AddToRecipeBookParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let request = crate::client::types::CreateRecipeBookEntryRequest {
            book: params.recipe_book,
            recipe: params.recipe,
        };

        match client.add_to_recipe_book(request).await {
            Ok(entry) => {
                let recipe_name = entry
                    .recipe_content
                    .as_ref()
                    .and_then(|r| r.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let book_name = entry
                    .book_content
                    .as_ref()
                    .and_then(|b| b.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&json!({
                        "entry_id": entry.id,
                        "recipe_id": entry.recipe,
                        "recipe_name": recipe_name,
                        "recipe_url": client.recipe_url(entry.recipe),
                        "book_id": entry.book,
                        "book_name": book_name,
                        "message": format!("Added '{}' to book '{}'", recipe_name, book_name)
                    }))
                    .unwrap(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to add recipe to book", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "Remove a recipe from a recipe book by entry ID. Use get_recipe_book_entries to find the entry ID.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn remove_from_recipe_book(
        &self,
        Parameters(params): Parameters<RemoveFromRecipeBookParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        match client.remove_from_recipe_book(params.id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                json!({"message": format!("Removed recipe book entry {}", params.id)}).to_string(),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to remove from recipe book", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "List which recipes are in which recipe books",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_recipe_book_entries(
        &self,
        Parameters(params): Parameters<GetRecipeBookEntriesParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        match client.get_recipe_book_entries(params.book_id).await {
            Ok(response) => {
                let entries_json: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|e| {
                        let recipe_name = e
                            .recipe_content
                            .as_ref()
                            .and_then(|r| r.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let book_name = e
                            .book_content
                            .as_ref()
                            .and_then(|b| b.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        json!({
                            "entry_id": e.id,
                            "recipe_id": e.recipe,
                            "recipe_name": recipe_name,
                            "recipe_url": client.recipe_url(e.recipe),
                            "book_id": e.book,
                            "book_name": book_name
                        })
                    })
                    .collect();
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(
                        &json!({"entries": entries_json, "total": response.count}),
                    )
                    .unwrap(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to get recipe book entries", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "Add a recipe's ingredients to the shopping list, scaled to `servings`, skipping foods already on hand in the pantry (listed in skipped_on_hand) unless include_on_hand is true. Entries are linked to the recipe in Tandoor.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn add_recipe_to_shopping_list(
        &self,
        Parameters(params): Parameters<AddRecipeToShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };
        let fail = |message: String| {
            Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to add recipe to shopping list", "details": message})
                    .to_string(),
            )]))
        };

        let recipe = match client.get_recipe(params.recipe_id).await {
            Ok(r) => r,
            Err(e) => return fail(e.to_string()),
        };
        let base_servings = recipe.servings.unwrap_or(1).max(1);
        let servings = params.servings.unwrap_or(base_servings);
        if servings <= 0 {
            return fail("servings must be at least 1".to_string());
        }
        let scale = servings as f64 / base_servings as f64;
        let include_on_hand = params.include_on_hand.unwrap_or(false);

        let mut ingredient_ids = Vec::new();
        let mut adding = Vec::new();
        let mut skipped_on_hand: Vec<String> = Vec::new();
        for step in ordered_steps(&recipe.steps) {
            for ing in &step.ingredients {
                let Some(food) = ing.food.as_ref() else {
                    continue;
                };
                if ing.is_header {
                    continue;
                }
                if food.food_onhand && !include_on_hand {
                    if !skipped_on_hand.contains(&food.name) {
                        skipped_on_hand.push(food.name.clone());
                    }
                    continue;
                }
                ingredient_ids.push(ing.id);
                adding.push(json!({
                    "food": food.name,
                    "amount": if ing.no_amount { None } else { Some((ing.amount * scale * 100.0).round() / 100.0) },
                    "unit": ing.unit.as_ref().map(|u| &u.name)
                }));
            }
        }

        if ingredient_ids.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&json!({
                    "recipe": recipe.name,
                    "added": [],
                    "skipped_on_hand": skipped_on_hand,
                    "message": "Nothing to add: every ingredient is already on hand"
                }))
                .unwrap(),
            )]));
        }

        if let Err(e) = client
            .add_recipe_to_shopping_list(recipe.id, servings, &ingredient_ids)
            .await
        {
            return fail(e.to_string());
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json!({
                "recipe": recipe.name,
                "servings": servings,
                "added": adding,
                "skipped_on_hand": skipped_on_hand,
                "message": format!(
                    "Added {} ingredients from {} ({} skipped as on hand)",
                    ingredient_ids.len(),
                    recipe.name,
                    skipped_on_hand.len()
                )
            }))
            .unwrap(),
        )]))
    }

    #[tool(
        description = "Find foods that look like duplicates (same name apart from case, spacing, or a simple plural, e.g. \"Tomato\" / \"tomatoes\"). Use merge_foods to combine them.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn find_duplicate_foods(
        &self,
        Parameters(_params): Parameters<FindDuplicateFoodsParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let foods = match client.list_all_foods().await {
            Ok(f) => f,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Failed to list foods", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let groups: Vec<serde_json::Value> = find_duplicate_food_groups(&foods)
            .into_iter()
            .map(|group| {
                json!(group
                    .iter()
                    .map(|f| json!({
                        "id": f.id,
                        "name": f.name,
                        "plural_name": f.plural_name,
                        "on_hand": f.food_onhand
                    }))
                    .collect::<Vec<_>>())
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json!({
                "foods_checked": foods.len(),
                "duplicate_groups": groups,
            }))
            .unwrap(),
        )]))
    }

    #[tool(
        description = "Merge one food into another: every recipe ingredient, shopping list entry, etc. using the source food is moved to the target, then the source food is DELETED. Permanent — confirm with the user first. Use find_duplicate_foods or search_foods to get the IDs.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn merge_foods(
        &self,
        Parameters(params): Parameters<MergeFoodsParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };
        let fail = |message: String| {
            Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to merge foods", "details": message}).to_string(),
            )]))
        };

        if params.source_id == params.target_id {
            return fail("source_id and target_id are the same food".to_string());
        }
        let source = match client.get_food(params.source_id).await {
            Ok(f) => f,
            Err(e) => return fail(e.to_string()),
        };
        let target = match client.get_food(params.target_id).await {
            Ok(f) => f,
            Err(e) => return fail(e.to_string()),
        };
        if let Err(e) = client.merge_food(source.id, target.id).await {
            return fail(e.to_string());
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json!({
                "merged": source.name,
                "into": {"id": target.id, "name": target.name},
                "message": format!("Merged '{}' into '{}'; '{}' was deleted", source.name, target.name, source.name)
            }))
            .unwrap(),
        )]))
    }

    #[tool(
        description = "Add recipe ingredients from meal plans in a date range as linked shopping groups. Linked items can later be inspected, rescaled, or removed together. Foods already on hand are skipped unless skip_on_hand is false.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn add_meal_plan_to_shopping_list(
        &self,
        Parameters(params): Parameters<AddMealPlanToShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let from_date =
            chrono::NaiveDate::parse_from_str(&params.from_date, "%Y-%m-%d").map_err(|error| {
                McpError::invalid_params(
                    "Invalid from_date",
                    Some(json!({"error": error.to_string()})),
                )
            })?;
        let to_date =
            chrono::NaiveDate::parse_from_str(&params.to_date, "%Y-%m-%d").map_err(|error| {
                McpError::invalid_params(
                    "Invalid to_date",
                    Some(json!({"error": error.to_string()})),
                )
            })?;
        let days = (to_date - from_date).num_days();
        if !(0..=366).contains(&days) {
            return Err(McpError::invalid_params(
                "Meal-plan range must be ordered and at most 366 days",
                Some(json!({"from_date": params.from_date, "to_date": params.to_date})),
            ));
        }

        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        let meal_plans = match client
            .get_all_meal_plans(Some(&params.from_date), Some(&params.to_date))
            .await
        {
            Ok(plans) => plans,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Failed to get meal plans", "details": e.to_string()})
                        .to_string(),
                )]));
            }
        };

        let skip_on_hand = params.skip_on_hand.unwrap_or(true);
        let mut skipped_on_hand: Vec<String> = Vec::new();
        let mut prepared = Vec::new();
        let mut errors = Vec::new();

        // Resolve and validate every meal plan before creating any shopping data.
        for plan in &meal_plans {
            let recipe = match &plan.recipe {
                Some(r) => r,
                None => {
                    errors.push(json!({
                        "meal_plan_id": plan.id,
                        "error": "Meal plan has no recipe"
                    }));
                    continue;
                }
            };

            let detailed = match client.get_recipe(recipe.id).await {
                Ok(r) => r,
                Err(error) => {
                    errors.push(json!({
                        "meal_plan_id": plan.id,
                        "recipe_id": recipe.id,
                        "error": "Failed to load recipe",
                        "details": error.to_string()
                    }));
                    continue;
                }
            };

            let base_servings = detailed.servings.unwrap_or(1);
            if base_servings <= 0 || !plan.servings.is_finite() || plan.servings <= 0.0 {
                errors.push(json!({
                    "meal_plan_id": plan.id,
                    "recipe_id": recipe.id,
                    "error": "Recipe and meal-plan servings must be greater than zero"
                }));
                continue;
            }
            let scale = plan.servings / f64::from(base_servings);
            let mut entries = Vec::new();

            for step in &detailed.steps {
                for ingredient in &step.ingredients {
                    let Some(food) = ingredient.food.as_ref() else {
                        continue;
                    };
                    if ingredient.is_header || ingredient.no_amount {
                        continue;
                    }
                    if skip_on_hand && (food.food_onhand || food.substitute_onhand) {
                        if !skipped_on_hand.contains(&food.name) {
                            skipped_on_hand.push(food.name.clone());
                        }
                        continue;
                    }
                    entries.push(json!({
                        "amount": (ingredient.amount * scale * 10.0).round() / 10.0,
                        "food_id": food.id,
                        "unit_id": ingredient.unit.as_ref().map(|unit| unit.id),
                        "ingredient_id": ingredient.id
                    }));
                }
            }

            prepared.push((
                plan.id,
                recipe.id,
                recipe.name.clone(),
                plan.servings,
                entries,
            ));
        }

        let mut groups = Vec::new();
        let mut added = 0usize;
        for (meal_plan_id, recipe_id, recipe_name, servings, entries) in prepared {
            if entries.is_empty() {
                errors.push(json!({
                    "meal_plan_id": meal_plan_id,
                    "recipe_id": recipe_id,
                    "error": "No eligible ingredients"
                }));
                continue;
            }

            let group = match client
                .api_create(
                    "shopping-list-recipe",
                    json!({
                        "recipe": recipe_id,
                        "mealplan": meal_plan_id,
                        "servings": servings
                    }),
                )
                .await
            {
                Ok(group) => group,
                Err(error) => {
                    errors.push(json!({
                        "meal_plan_id": meal_plan_id,
                        "recipe_id": recipe_id,
                        "error": "Failed to create linked shopping group",
                        "details": error.to_string()
                    }));
                    continue;
                }
            };
            let Some(group_id) = group["id"].as_i64() else {
                errors.push(json!({
                    "meal_plan_id": meal_plan_id,
                    "recipe_id": recipe_id,
                    "error": "Tandoor returned a shopping group without an ID"
                }));
                continue;
            };

            let count = entries.len();
            let resource = format!("shopping-list-recipe/{group_id}/bulk_create_entries");
            match client
                .api_create(
                    &resource,
                    json!({"entries": entries, "shopping_lists_ids": []}),
                )
                .await
            {
                Ok(_) => {
                    added += count;
                    groups.push(json!({
                        "id": group_id,
                        "meal_plan_id": meal_plan_id,
                        "recipe_id": recipe_id,
                        "recipe": recipe_name,
                        "recipe_url": client.recipe_url(recipe_id),
                        "servings": servings,
                        "items_added": count
                    }));
                }
                Err(error) => {
                    // Avoid leaving an empty group when its entries fail.
                    let cleanup_error = client
                        .api_delete("shopping-list-recipe", group_id)
                        .await
                        .err()
                        .map(|cleanup| cleanup.to_string());
                    errors.push(json!({
                        "meal_plan_id": meal_plan_id,
                        "recipe_id": recipe_id,
                        "error": "Failed to create linked shopping entries",
                        "details": error.to_string(),
                        "cleanup_error": cleanup_error
                    }));
                }
            }
        }

        let result = serde_json::to_string_pretty(&json!({
            "added": added,
            "groups": groups,
            "errors": errors,
            "meal_plans_processed": meal_plans.len(),
            "skipped_on_hand": skipped_on_hand,
            "message": format!(
                "Added {} linked ingredients to shopping list from {} meal plans ({} to {})",
                added,
                meal_plans.len(),
                params.from_date,
                params.to_date
            )
        }))
        .unwrap();
        if added == 0 && !meal_plans.is_empty() {
            Ok(CallToolResult::error(vec![Content::text(result)]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(result)]))
        }
    }

    #[tool(
        description = "List supermarkets/stores with their aisle (category) order. Optional `query` filters by name.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_supermarkets(
        &self,
        Parameters(params): Parameters<GetSupermarketsParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        match client.api_list_all("supermarket").await {
            Ok(stores) => {
                let query = params.query.unwrap_or_default().to_lowercase();
                let stores: Vec<_> = stores
                    .iter()
                    .filter(|s| {
                        s["name"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&query)
                    })
                    .map(supermarket_view)
                    .collect();
                tool_ok(json!({"total": stores.len(), "supermarkets": stores}))
            }
            Err(e) => tool_err("Failed to get supermarkets", e),
        }
    }

    #[tool(
        description = "Create a supermarket/store, optionally with its aisle (category) order",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_supermarket(
        &self,
        Parameters(params): Parameters<CreateSupermarketParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        if params.name.trim().is_empty() {
            return tool_err("Invalid supermarket", "name cannot be empty");
        }
        let existing = match client.api_list_all("supermarket").await {
            Ok(s) => s,
            Err(e) => return tool_err("Failed to load supermarkets", e),
        };
        if existing.iter().any(|s| {
            s["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(params.name.trim()))
        }) {
            return tool_err(
                "Supermarket already exists",
                format!("'{}' exists; use update_supermarket", params.name.trim()),
            );
        }
        let store = match client
            .api_create(
                "supermarket",
                json!({"name": params.name.trim(), "description": params.description}),
            )
            .await
        {
            Ok(s) => s,
            Err(e) => return tool_err("Failed to create supermarket", e),
        };
        let store = match params.category_order {
            Some(order) => match sync_category_order(&client, &store, &order).await {
                Ok(s) => s,
                Err(e) => {
                    return tool_err("Created the supermarket but failed to set its aisles", e)
                }
            },
            None => store,
        };
        tool_ok(json!({"created": supermarket_view(&store)}))
    }

    #[tool(
        description = "Rename or describe a supermarket, and/or set its aisle order. `category_order` is the FULL walking order of category names; categories not listed are removed from this store (not deleted).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_supermarket(
        &self,
        Parameters(params): Parameters<UpdateSupermarketParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let stores = match client.api_list_all("supermarket").await {
            Ok(s) => s,
            Err(e) => return tool_err("Failed to load supermarkets", e),
        };
        let store = match resolve_named_exact(&stores, &params.supermarket, "supermarket") {
            Ok(s) => s.clone(),
            Err(e) => return tool_err("Supermarket not found", e),
        };
        let id = store["id"].as_i64().unwrap_or(0);

        let mut body = serde_json::Map::new();
        if let Some(name) = params.name.filter(|n| !n.trim().is_empty()) {
            body.insert("name".to_string(), json!(name.trim()));
        }
        if let Some(description) = params.description {
            body.insert("description".to_string(), json!(description));
        }
        if body.is_empty() && params.category_order.is_none() {
            return tool_err(
                "Nothing to change",
                "Pass name, description, and/or category_order",
            );
        }
        let mut store = store;
        if !body.is_empty() {
            store = match client
                .api_update("supermarket", id, serde_json::Value::Object(body))
                .await
            {
                Ok(s) => s,
                Err(e) => return tool_err("Failed to update supermarket", e),
            };
        }
        if let Some(order) = params.category_order {
            store = match sync_category_order(&client, &store, &order).await {
                Ok(s) => s,
                Err(e) => return tool_err("Failed to set aisle order", e),
            };
        }
        tool_ok(json!({"updated": supermarket_view(&store)}))
    }

    #[tool(
        description = "Delete a supermarket/store and its aisle order. Permanent — confirm with the user first. Categories and foods are kept.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_supermarket(
        &self,
        Parameters(params): Parameters<DeleteSupermarketParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let stores = match client.api_list_all("supermarket").await {
            Ok(s) => s,
            Err(e) => return tool_err("Failed to load supermarkets", e),
        };
        let store = match resolve_named_exact(&stores, &params.supermarket, "supermarket") {
            Ok(s) => s,
            Err(e) => return tool_err("Supermarket not found", e),
        };
        match client
            .api_delete("supermarket", store["id"].as_i64().unwrap_or(0))
            .await
        {
            Ok(()) => tool_ok(json!({"deleted": {"id": store["id"], "name": store["name"]}})),
            Err(e) => tool_err("Failed to delete supermarket", e),
        }
    }

    #[tool(
        description = "List supermarket categories (aisles/sections such as Produce or Dairy) that foods are grouped by on the shopping list",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_supermarket_categories(
        &self,
        Parameters(_params): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        match client.api_list_all("supermarket-category").await {
            Ok(categories) => {
                let categories: Vec<_> = categories.iter().map(category_view).collect();
                tool_ok(json!({"total": categories.len(), "categories": categories}))
            }
            Err(e) => tool_err("Failed to get categories", e),
        }
    }

    #[tool(
        description = "Create a supermarket category (aisle/section), e.g. \"Bulk Bins\"",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_supermarket_category(
        &self,
        Parameters(params): Parameters<CreateSupermarketCategoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let name = params.name.trim();
        if name.is_empty() {
            return tool_err("Invalid category", "name cannot be empty");
        }
        let existing = match client.api_list_all("supermarket-category").await {
            Ok(c) => c,
            Err(e) => return tool_err("Failed to load categories", e),
        };
        if let Some(c) = existing.iter().find(|c| {
            c["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        }) {
            return tool_err(
                "Category already exists",
                format!(
                    "'{}' (ID {}) exists; use update_supermarket_category",
                    c["name"].as_str().unwrap_or(""),
                    c["id"]
                ),
            );
        }
        match client
            .api_create(
                "supermarket-category",
                json!({"name": name, "description": params.description}),
            )
            .await
        {
            Ok(c) => tool_ok(json!({"created": category_view(&c)})),
            Err(e) => tool_err("Failed to create category", e),
        }
    }

    #[tool(
        description = "Rename or describe a supermarket category (aisle/section)",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_supermarket_category(
        &self,
        Parameters(params): Parameters<UpdateSupermarketCategoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let mut body = serde_json::Map::new();
        if let Some(name) = params.name.filter(|n| !n.trim().is_empty()) {
            body.insert("name".to_string(), json!(name.trim()));
        }
        if let Some(description) = params.description {
            body.insert("description".to_string(), json!(description));
        }
        if body.is_empty() {
            return tool_err("Nothing to change", "Pass name and/or description");
        }
        let categories = match client.api_list_all("supermarket-category").await {
            Ok(c) => c,
            Err(e) => return tool_err("Failed to load categories", e),
        };
        let category = match resolve_named_exact(&categories, &params.category, "category") {
            Ok(c) => c,
            Err(e) => return tool_err("Category not found", e),
        };
        match client
            .api_update(
                "supermarket-category",
                category["id"].as_i64().unwrap_or(0),
                serde_json::Value::Object(body),
            )
            .await
        {
            Ok(c) => tool_ok(json!({"updated": category_view(&c)})),
            Err(e) => tool_err("Failed to update category", e),
        }
    }

    #[tool(
        description = "Delete a supermarket category (aisle/section). Permanent — confirm with the user first. Foods in it become uncategorized and it is removed from every store's aisle order.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_supermarket_category(
        &self,
        Parameters(params): Parameters<DeleteSupermarketCategoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let categories = match client.api_list_all("supermarket-category").await {
            Ok(c) => c,
            Err(e) => return tool_err("Failed to load categories", e),
        };
        let category = match resolve_named_exact(&categories, &params.category, "category") {
            Ok(c) => c,
            Err(e) => return tool_err("Category not found", e),
        };
        match client
            .api_delete("supermarket-category", category["id"].as_i64().unwrap_or(0))
            .await
        {
            Ok(()) => tool_ok(json!({"deleted": {"id": category["id"], "name": category["name"]}})),
            Err(e) => tool_err("Failed to delete category", e),
        }
    }

    #[tool(
        description = "Put foods in a supermarket category (aisle), e.g. foods: [\"lemons\", \"parsley\"], category: \"Produce\", so they group together on the shopping list. The category is created if new; omit it to clear.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn set_food_category(
        &self,
        Parameters(params): Parameters<SetFoodCategoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let category = params
            .category
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(|name| json!({"name": name}));
        let mut updated = Vec::new();
        let mut errors = Vec::new();
        for name in params.foods {
            let id = match find_food_by_name(&client, &name).await {
                Ok((Some(id), _)) => id,
                Ok((None, similar)) => {
                    errors.push(
                        json!({"food": name, "error": "Food not found", "similar_foods": similar}),
                    );
                    continue;
                }
                Err(e) => {
                    errors.push(json!({"food": name, "error": e.to_string()}));
                    continue;
                }
            };
            match client
                .api_update(
                    "food",
                    id as i64,
                    json!({"supermarket_category": category.clone()}),
                )
                .await
            {
                Ok(food) => updated.push(json!({
                    "food": food["name"],
                    "category": food["supermarket_category"]["name"]
                })),
                Err(e) => errors.push(json!({"food": name, "error": e.to_string()})),
            }
        }
        tool_ok(json!({"updated": updated, "errors": errors}))
    }

    #[tool(
        description = "List named shopping lists (e.g. \"Costco\", \"Party\"). Items can be on several lists; see add_to_shopping_list's shopping_list option.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_shopping_lists(
        &self,
        Parameters(_params): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let lists = match client.api_list_all("shopping-list").await {
            Ok(l) => l,
            Err(e) => return tool_err("Failed to get shopping lists", e),
        };
        let entries = match client.get_all_shopping_entries().await {
            Ok(entries) => entries,
            Err(e) => return tool_err("Failed to get shopping-list entries", e),
        };
        let lists: Vec<_> = lists
            .iter()
            .map(|l| {
                let id = l["id"].as_i64();
                let mut view = shopping_list_view(l);
                view["unchecked_items"] = json!(entries
                    .iter()
                    .filter(
                        |e| !e.checked && e.shopping_lists.iter().any(|s| Some(s.id as i64) == id)
                    )
                    .count());
                view
            })
            .collect();
        tool_ok(json!({"total": lists.len(), "shopping_lists": lists}))
    }

    #[tool(
        description = "Create a named shopping list, e.g. \"Costco\" or \"Party\"",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_shopping_list(
        &self,
        Parameters(params): Parameters<CreateShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let name = params.name.trim();
        if name.is_empty() {
            return tool_err("Invalid shopping list", "name cannot be empty");
        }
        let existing = match client.api_list_all("shopping-list").await {
            Ok(l) => l,
            Err(e) => return tool_err("Failed to load shopping lists", e),
        };
        if existing.iter().any(|l| {
            l["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        }) {
            return tool_err(
                "Shopping list already exists",
                format!("'{name}' exists; use update_shopping_list"),
            );
        }
        let mut body = json!({"name": name, "description": params.description.unwrap_or_default()});
        if let Some(color) = params.color {
            body["color"] = json!(color);
        }
        match client.api_create("shopping-list", body).await {
            Ok(l) => tool_ok(json!({"created": shopping_list_view(&l)})),
            Err(e) => tool_err("Failed to create shopping list", e),
        }
    }

    #[tool(
        description = "Rename a named shopping list or change its description/color",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_shopping_list(
        &self,
        Parameters(params): Parameters<UpdateShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let mut body = serde_json::Map::new();
        if let Some(name) = params.name.filter(|n| !n.trim().is_empty()) {
            body.insert("name".to_string(), json!(name.trim()));
        }
        if let Some(description) = params.description {
            body.insert("description".to_string(), json!(description));
        }
        if let Some(color) = params.color {
            body.insert("color".to_string(), json!(color));
        }
        if body.is_empty() {
            return tool_err("Nothing to change", "Pass name, description, and/or color");
        }
        let lists = match client.api_list_all("shopping-list").await {
            Ok(l) => l,
            Err(e) => return tool_err("Failed to load shopping lists", e),
        };
        let list = match resolve_named_exact(&lists, &params.shopping_list, "shopping list") {
            Ok(l) => l,
            Err(e) => return tool_err("Shopping list not found", e),
        };
        match client
            .api_update(
                "shopping-list",
                list["id"].as_i64().unwrap_or(0),
                serde_json::Value::Object(body),
            )
            .await
        {
            Ok(l) => tool_ok(json!({"updated": shopping_list_view(&l)})),
            Err(e) => tool_err("Failed to update shopping list", e),
        }
    }

    #[tool(
        description = "Delete a named shopping list. Permanent — confirm with the user first. Items on it stay on the main shopping list, just no longer tagged with this list.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_shopping_list(
        &self,
        Parameters(params): Parameters<DeleteShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let lists = match client.api_list_all("shopping-list").await {
            Ok(l) => l,
            Err(e) => return tool_err("Failed to load shopping lists", e),
        };
        let list = match resolve_named_exact(&lists, &params.shopping_list, "shopping list") {
            Ok(l) => l,
            Err(e) => return tool_err("Shopping list not found", e),
        };
        match client
            .api_delete("shopping-list", list["id"].as_i64().unwrap_or(0))
            .await
        {
            Ok(()) => tool_ok(json!({"deleted": {"id": list["id"], "name": list["name"]}})),
            Err(e) => tool_err("Failed to delete shopping list", e),
        }
    }

    #[tool(
        description = "List the recipes (and meal plans) on the shopping list, with their servings and the items each one added",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_shopping_list_recipes(
        &self,
        Parameters(_params): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let groups = match client.api_list_all("shopping-list-recipe").await {
            Ok(g) => g,
            Err(e) => return tool_err("Failed to get shopping list recipes", e),
        };
        let entries = match client.get_all_shopping_entries().await {
            Ok(e) => e,
            Err(e) => return tool_err("Failed to get shopping list", e),
        };
        let groups: Vec<_> = groups
            .iter()
            .map(|g| recipe_group_view(g, &entries))
            .collect();
        tool_ok(json!({"total": groups.len(), "recipes": groups}))
    }

    #[tool(
        description = "Change the servings of a recipe on the shopping list; its items are rescaled (e.g. 2 → 6 servings triples them). Other items are untouched.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn update_shopping_list_recipe(
        &self,
        Parameters(params): Parameters<UpdateShoppingListRecipeParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        if params.servings <= 0.0 {
            return tool_err(
                "Invalid servings",
                "servings must be positive; use remove_recipe_from_shopping_list to remove it",
            );
        }
        let (group, _) = match find_recipe_group(&client, &params.recipe, false).await {
            Ok(g) => g,
            Err(e) => return tool_err("Recipe not on shopping list", e),
        };
        let id = group["id"].as_i64().unwrap_or(0);
        if let Err(e) = client
            .api_update(
                "shopping-list-recipe",
                id,
                json!({"servings": params.servings}),
            )
            .await
        {
            return tool_err("Failed to update servings", e);
        }
        let entries = client.get_all_shopping_entries().await.unwrap_or_default();
        let group = client
            .api_get("shopping-list-recipe", id)
            .await
            .unwrap_or(group);
        tool_ok(json!({"updated": recipe_group_view(&group, &entries)}))
    }

    #[tool(
        description = "Remove a recipe from the shopping list together with all the items it added. Items you added yourself are kept. Confirm with the user first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn remove_recipe_from_shopping_list(
        &self,
        Parameters(params): Parameters<RemoveRecipeFromShoppingListParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = auth_or_return!(self);
        let (group, view) = match find_recipe_group(&client, &params.recipe, true).await {
            Ok(g) => g,
            Err(e) => return tool_err("Recipe not on shopping list", e),
        };
        match client
            .api_delete("shopping-list-recipe", group["id"].as_i64().unwrap_or(0))
            .await
        {
            Ok(()) => tool_ok(json!({"removed": view})),
            Err(e) => tool_err("Failed to remove recipe from shopping list", e),
        }
    }

    #[tool(
        description = "List unit conversions, optionally filtered by food ID",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_unit_conversions(
        &self,
        Parameters(params): Parameters<GetUnitConversionsParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = match self.ensure_authenticated().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    json!({"error": "Authentication Error", "details": e.to_string()}).to_string(),
                )]));
            }
        };

        match client.get_unit_conversions(params.food_id).await {
            Ok(response) => {
                let conversions: Vec<serde_json::Value> = response
                    .results
                    .into_iter()
                    .map(|c| {
                        let base_unit = c
                            .base_unit
                            .as_ref()
                            .and_then(|u| u.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("?");
                        let conv_unit = c
                            .converted_unit
                            .as_ref()
                            .and_then(|u| u.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("?");
                        let food_name = c
                            .food
                            .as_ref()
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("?");
                        json!({
                            "id": c.id,
                            "food": food_name,
                            "base_amount": c.base_amount,
                            "base_unit": base_unit,
                            "converted_amount": c.converted_amount,
                            "converted_unit": conv_unit
                        })
                    })
                    .collect();
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(
                        &json!({"conversions": conversions, "total": response.count}),
                    )
                    .unwrap(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                json!({"error": "Failed to get unit conversions", "details": e.to_string()})
                    .to_string(),
            )])),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TandoorMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("tandoor", env!("CARGO_PKG_VERSION")))
        .with_instructions(SERVER_INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25])
    }

    /// Same dispatch as the `#[tool_handler]` default, plus one INFO line per
    /// call (tool name, argument keys, outcome). Argument values are not
    /// logged: they can contain signed download URLs and recipe text. Without
    /// this line, a `tools/call` that fails before touching Tandoor leaves no
    /// trace, which made it impossible to tell whether ChatGPT reached the
    /// server at all when its file hand-off failed.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let tool = request.name.clone();
        let arg_keys: Vec<String> = request
            .arguments
            .as_ref()
            .map(|args| args.keys().cloned().collect())
            .unwrap_or_default();
        tracing::info!(%tool, ?arg_keys, "tools/call");
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await;
        match &result {
            Err(error) => tracing::warn!(%tool, %error, "tools/call rejected"),
            Ok(CallToolResponse::Complete(result)) if result.is_error == Some(true) => {
                tracing::warn!(%tool, "tools/call returned a tool error")
            }
            Ok(_) => tracing::info!(%tool, "tools/call ok"),
        }
        result
    }

    fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<DiscoverResult, McpError>> + Send + '_ {
        std::future::ready(Ok(DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        )
        .with_ttl_ms(300_000)
        .with_cache_scope(CacheScope::Public)))
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(RECIPE_GALLERY_URI, "tandoor-recipe-gallery")
                .with_title("Tandoor recipe gallery")
                .with_description("Inline gallery for authoritative recipe thumbnails")
                .with_mime_type("text/html;profile=mcp-app"),
            Resource::new(MEAL_PLAN_GALLERY_URI, "tandoor-meal-plan-gallery")
                .with_title("Tandoor meal-plan gallery")
                .with_description("Inline meal-plan summary with authoritative recipe thumbnails")
                .with_mime_type("text/html;profile=mcp-app"),
        ])))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, McpError>> + Send + '_ {
        // Any version of a known widget URI resolves to the current HTML; the
        // requested URI is echoed back so the client's cache key stays consistent.
        let widget = if widget_uri_matches(&request.uri, RECIPE_GALLERY_URI_PREFIX) {
            Some((recipe_gallery_html(), self.gallery_resource_meta()))
        } else if widget_uri_matches(&request.uri, MEAL_PLAN_GALLERY_URI_PREFIX) {
            Some((
                meal_plan_gallery_html(),
                self.meal_plan_gallery_resource_meta(),
            ))
        } else {
            None
        };
        let result = match widget {
            Some((html, meta)) => Ok(ReadResourceResponse::from(
                ReadResourceResult::new(vec![ResourceContents::text(html, request.uri)
                    .with_mime_type("text/html;profile=mcp-app")
                    .with_meta(meta)])
                .with_ttl_ms(300_000)
                .with_cache_scope(CacheScope::Public),
            )),
            None => Err(McpError::invalid_params(
                format!("Unknown resource URI: {}", request.uri),
                None,
            )),
        };
        std::future::ready(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ChatGPT Apps SDK rejects file-param schemas that deviate from its
    /// template; this pins the generated schema to that template.
    #[test]
    fn set_recipe_image_from_file_declares_a_valid_file_param() {
        let tool = TandoorMcpServer::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "set_recipe_image_from_file")
            .expect("tool should be registered");
        let meta = serde_json::to_value(tool.meta.as_ref().expect("tool meta")).unwrap();
        assert_eq!(meta["openai/fileParams"], json!(["image"]));

        let schema = serde_json::to_value(&*tool.input_schema).unwrap();
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("image")));
        // The property may be inlined or a $ref into $defs/definitions.
        let property = schema["properties"]["image"].clone();
        let file = match property["$ref"].as_str().map(str::to_owned) {
            Some(reference) => {
                let name = reference.rsplit('/').next().unwrap();
                let def = &schema["$defs"][name];
                if def.is_null() {
                    schema["definitions"][name].clone()
                } else {
                    def.clone()
                }
            }
            None => property,
        };
        assert_eq!(file["type"], "object", "{file}");
        for property in ["download_url", "file_id", "mime_type", "file_name"] {
            let spec = &file["properties"][property];
            assert_eq!(
                spec["type"], "string",
                "{property} must be a plain string: {file}"
            );
            // The SDK template has only `type` (plus text); a `"default": null`
            // from `#[serde(default)]` or an `anyOf` from `Option` would differ.
            for forbidden in ["default", "anyOf", "oneOf", "nullable"] {
                assert!(
                    spec.get(forbidden).is_none(),
                    "{property} must not declare `{forbidden}`: {spec}"
                );
            }
        }
        let required: std::collections::BTreeSet<_> = file["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, ["download_url", "file_id"].into_iter().collect());
        assert_eq!(file["additionalProperties"], false);
    }

    #[test]
    fn chat_file_rejects_mobile_placeholder_strings() {
        assert!(serde_json::from_value::<ChatFile>(json!("chat_upload")).is_err());
        assert!(serde_json::from_value::<ChatFile>(json!("attached")).is_err());
        let file: ChatFile = serde_json::from_value(json!({
            "download_url": "https://files.oaiusercontent.com/x?sig=1",
            "file_id": "file_123"
        }))
        .unwrap();
        assert_eq!(file.file_id, "file_123");
        assert!(file.mime_type.is_none());
    }

    #[test]
    fn widget_html_inlines_shared_fragments() {
        for html in [recipe_gallery_html(), meal_plan_gallery_html()] {
            assert!(!html.contains(SHARED_CSS_MARKER));
            assert!(!html.contains(SHARED_JS_MARKER));
            // One representative line from each shared file.
            assert!(html.contains("grid-template-columns: minmax(210px, 36%)"));
            assert!(html.contains("function createGallery(config)"));
            assert!(html.contains("gallery.start();"));
        }
        assert!(recipe_gallery_html().contains("tandoor-recipe-gallery"));
        assert!(meal_plan_gallery_html().contains("tandoor-meal-plan-gallery"));
    }

    #[test]
    fn keyword_names_lists_source_keyword_first() {
        use crate::client::types::Keyword;
        let keyword = |name: &str, description: Option<&str>| Keyword {
            id: 0,
            name: name.into(),
            label: None,
            description: description.map(Into::into),
            created: None,
            updated: None,
            parent: None,
            numchild: None,
            full_name: None,
        };
        let keywords = vec![
            keyword("Gluten Free", Some("Dietary restriction")),
            keyword("Dinner", None),
            keyword("Ottolenghi", Some("  source: cookbook Simple")),
            keyword("Source Web", None),
            keyword("Quick", Some("Resourceful cooks")),
        ];
        assert_eq!(
            keyword_names(&keywords),
            vec!["Ottolenghi", "Source Web", "Gluten Free", "Dinner", "Quick"]
        );
        assert!(keyword_names(&[]).is_empty());
    }

    #[test]
    fn merge_nutrition_keeps_id_and_fills_missing_values() {
        let current: crate::client::types::Nutrition = serde_json::from_value(json!({
            "id": 9,
            "calories": 100,
            "carbohydrates": 20,
            "fats": 3,
            "proteins": 4,
            "source": "old"
        }))
        .unwrap();
        let input: NutritionInput =
            serde_json::from_value(json!({"calories": 125, "source": "new"})).unwrap();
        let merged = merge_nutrition(Some(&current), &input);
        assert_eq!(merged.id, Some(9));
        assert_eq!(merged.calories, 125.0);
        assert_eq!(merged.carbohydrates, 20.0);
        assert_eq!(merged.source.as_deref(), Some("new"));

        let empty: NutritionInput = serde_json::from_value(json!({})).unwrap();
        let defaults = merge_nutrition(None, &empty);
        assert_eq!(defaults.calories, 0.0);
        assert_eq!(defaults.proteins, 0.0);
    }

    #[test]
    fn property_changes_upsert_remove_and_reject_unknown_types() {
        let types: Vec<crate::client::types::PropertyType> = serde_json::from_value(json!([
            {"id": 1, "name": "Fiber", "unit": "g"},
            {"id": 2, "name": "Cost", "unit": "EUR"}
        ]))
        .unwrap();
        let current: Vec<crate::client::types::Property> = serde_json::from_value(json!([
            {"id": 10, "property_amount": 2.0, "property_type": {"id": 1, "name": "Fiber", "unit": "g"}},
            {"id": 11, "property_amount": 5.0, "property_type": {"id": 2, "name": "Cost", "unit": "EUR"}}
        ]))
        .unwrap();
        let set = vec![RecipePropertyInput {
            property_type: NameOrId::Name("fiber".to_string()),
            amount: Some(3.5),
        }];
        let planned = plan_property_changes(&current, &set, &["cost".to_string()], &types).unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id, Some(10));
        assert_eq!(planned[0].property_amount, Some(3.5));

        let unknown = vec![RecipePropertyInput {
            property_type: NameOrId::Name("Unknown".to_string()),
            amount: Some(1.0),
        }];
        let error = plan_property_changes(&current, &unknown, &[], &types).unwrap_err();
        assert!(error.contains("Available types: Fiber, Cost"), "{error}");
    }

    #[test]
    fn scalar_recipe_patch_omits_unset_fields_and_source_title() {
        let params: UpdateRecipeParams = serde_json::from_value(json!({
            "id": 7,
            "servings_text": "Makes a jar",
            "source_url": "https://example.com"
        }))
        .unwrap();
        let body = recipe_scalar_body(&params);
        assert_eq!(body["servings_text"], "Makes a jar");
        assert_eq!(body["source_url"], "https://example.com");
        assert!(!body.contains_key("description"));
        assert!(!body.contains_key("source_title"));
    }

    #[test]
    fn recipe_fields_view_includes_extended_recipe_fields() {
        let recipe = recipe(42, "Soup", json!([]));
        let mut value = serde_json::to_value(&recipe).unwrap();
        value["servings_text"] = json!("Makes a pot");
        value["source_url"] = json!("https://example.com/soup");
        value["nutrition"] = json!({
            "id": 3, "calories": 100, "carbohydrates": 10,
            "fats": 2, "proteins": 4, "source": "Calculated"
        });
        let recipe: crate::client::types::Recipe = serde_json::from_value(value).unwrap();
        let client = TandoorClient::new("https://recipes.example".to_string());
        let view = recipe_fields_view(&recipe, &client);
        assert_eq!(view["servings_text"], "Makes a pot");
        assert_eq!(view["source_url"], "https://example.com/soup");
        assert_eq!(view["nutrition"]["carbohydrates"], 10.0);
    }

    #[test]
    fn gallery_result_keeps_urls_private_and_stays_small() {
        let result = gallery_result(
            json!({"items": [{"recipe_id": 7, "name": "Soup"}]}),
            vec![json!({
                "recipe_id": 7,
                "thumbnail_url": "https://example.test/private.jpg?signature=secret"
            })],
            1234,
        );
        let wire = serde_json::to_value(result).unwrap();

        assert!(!wire["structuredContent"]
            .to_string()
            .contains("signature=secret"));
        assert_eq!(
            wire["_meta"]["tandoor/gallery"]["items"][0]["thumbnail_url"],
            "https://example.test/private.jpg?signature=secret"
        );
        assert_eq!(wire["_meta"]["tandoor/gallery"]["urls_expire_at"], 1234);
        // No inline image blocks: large results are not delivered to the widget by ChatGPT.
        assert!(wire["content"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .all(|block| block["type"] != "image"));
    }

    #[test]
    fn widget_uri_matching_accepts_any_version_of_a_known_widget() {
        assert!(widget_uri_matches(
            MEAL_PLAN_GALLERY_URI,
            MEAL_PLAN_GALLERY_URI_PREFIX
        ));
        assert!(widget_uri_matches(
            "ui://tandoor/meal-plan-gallery-v8.html",
            MEAL_PLAN_GALLERY_URI_PREFIX
        ));
        assert!(widget_uri_matches(
            "ui://tandoor/recipe-gallery.html",
            RECIPE_GALLERY_URI_PREFIX
        ));
        assert!(!widget_uri_matches(
            "ui://tandoor/recipe-gallery-v12.html",
            MEAL_PLAN_GALLERY_URI_PREFIX
        ));
        assert!(!widget_uri_matches(
            "ui://tandoor/meal-plan-gallery-evil.html",
            MEAL_PLAN_GALLERY_URI_PREFIX
        ));
        assert!(!widget_uri_matches(
            "ui://tandoor/meal-plan-gallery-v9.htm",
            MEAL_PLAN_GALLERY_URI_PREFIX
        ));
    }

    #[test]
    fn build_step_requests_orders_steps_from_one_and_ingredients_from_zero() {
        let steps = vec![
            RecipeStepInput {
                instruction: "Chop vegetables".to_string(),
                name: Some("Prep".to_string()),
                ingredients: Some(vec![
                    RecipeStepIngredientInput {
                        food: Some("Onion".to_string()),
                        header: None,
                        unit: Some("cup".to_string()),
                        amount: Some(1.5),
                        note: Some("diced".to_string()),
                    },
                    RecipeStepIngredientInput {
                        food: Some("Garlic".to_string()),
                        header: None,
                        unit: None,
                        amount: Some(2.0),
                        note: None,
                    },
                ]),
                time: Some(10),
            },
            RecipeStepInput {
                instruction: "Cook".to_string(),
                name: None,
                ingredients: None,
                time: None,
            },
        ];

        let requests = build_step_requests(steps);

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].order, 1);
        assert_eq!(requests[1].order, 2);
        assert!(requests[1].ingredients.is_empty());

        let onion = &requests[0].ingredients[0];
        assert_eq!(onion.food.as_ref().unwrap().name, "Onion");
        assert_eq!(onion.unit.as_ref().unwrap().name, "cup");
        assert_eq!(onion.amount, "1.5");
        assert_eq!(onion.note.as_deref(), Some("diced"));
        assert_eq!(onion.order, 0);
        assert!(!onion.is_header);
        assert!(!onion.no_amount);

        let garlic = &requests[0].ingredients[1];
        assert_eq!(garlic.food.as_ref().unwrap().name, "Garlic");
        assert!(garlic.unit.is_none());
        assert_eq!(garlic.order, 1);
    }

    #[test]
    fn optional_step_fields_are_omitted_not_null() {
        // Tandoor rejects null for a step's name/time, so unset fields must be left out
        let steps = build_step_requests(vec![RecipeStepInput {
            instruction: "Mix".to_string(),
            name: None,
            ingredients: None,
            time: None,
        }]);
        let body = serde_json::to_value(&steps[0]).unwrap();
        assert!(body.get("name").is_none(), "{body}");
        assert!(body.get("time").is_none(), "{body}");
        assert_eq!(body["instruction"], "Mix");
    }

    #[test]
    fn build_step_requests_empty_input_yields_empty_output() {
        assert!(build_step_requests(vec![]).is_empty());
    }

    #[test]
    fn ingredient_without_amount_is_no_amount() {
        let req = build_ingredient_request(
            RecipeStepIngredientInput {
                food: Some("Salt".to_string()),
                header: None,
                unit: None,
                amount: None,
                note: Some("to taste".to_string()),
            },
            0,
        );
        assert!(req.no_amount);
        assert_eq!(req.amount, "0");
    }

    #[test]
    fn source_import_preview_becomes_a_create_payload() {
        let preview = json!({
            "name": "Imported Soup",
            "internal": false,
            "image_url": "https://images.example/soup.jpg",
            "keywords": [
                {"id": null, "name": "quick", "label": "quick", "import_keyword": true},
                {"id": null, "name": "skip", "label": "skip", "import_keyword": false}
            ],
            "steps": []
        });

        let (prepared, image_url) = prepare_source_import_recipe(preview).unwrap();
        assert_eq!(
            image_url.as_deref(),
            Some("https://images.example/soup.jpg")
        );
        assert!(prepared.get("image_url").is_none());
        assert_eq!(prepared["internal"], true);
        let keywords = prepared["keywords"].as_array().unwrap();
        assert_eq!(keywords.len(), 1);
        assert_eq!(keywords[0]["name"], "quick");
        assert!(keywords[0].get("import_keyword").is_none());
    }

    fn ingredient(id: i32, food: &str, order: i32) -> serde_json::Value {
        json!({
            "id": id, "amount": 1.0, "note": null, "order": order, "is_header": false,
            "no_amount": false, "unit": null,
            "food": {"id": id, "name": food, "plural_name": null, "description": null,
                     "recipe": null, "food_onhand": false, "supermarket_category": null,
                     "inherit_fields": [], "properties": []}
        })
    }

    fn recipe(id: i32, name: &str, steps: serde_json::Value) -> crate::client::types::Recipe {
        serde_json::from_value(json!({
            "id": id,
            "name": name,
            "description": null,
            "instructions": null,
            "servings": 2,
            "working_time": 0,
            "waiting_time": 0,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "internal": false,
            "steps": steps
        }))
        .unwrap()
    }

    /// Two steps, deliberately listed out of order: step_number 1 is id 10 (order 1),
    /// step_number 2 is id 20 (order 2).
    fn sample_steps() -> Vec<crate::client::types::Step> {
        serde_json::from_value(json!([
            {"id": 20, "name": "", "instruction": "Bake", "time": null, "order": 2, "file": null,
             "ingredients": [ingredient(201, "Egg", 0)]},
            {"id": 10, "name": "", "instruction": "Mix", "time": null, "order": 1, "file": null,
             "ingredients": [ingredient(102, "Sugar", 1), ingredient(101, "Flour", 0)]}
        ]))
        .unwrap()
    }

    fn update(step_number: usize) -> StepUpdateInput {
        StepUpdateInput {
            step_number,
            instruction: None,
            name: None,
            time: None,
            add_ingredients: None,
            remove_ingredients: None,
            update_ingredients: None,
            replace_ingredients: None,
        }
    }

    fn new_step(instruction: &str, after_step: Option<usize>) -> AddStepInput {
        AddStepInput {
            after_step,
            step: RecipeStepInput {
                instruction: instruction.to_string(),
                name: None,
                ingredients: None,
                time: None,
            },
        }
    }

    #[test]
    fn step_update_patches_only_the_numbered_step() {
        let mut u = update(2);
        u.instruction = Some("Bake at 350F".to_string());
        let plan = plan_step_changes(&sample_steps(), vec![u], vec![], vec![], vec![]).unwrap();
        assert_eq!(plan.patches.len(), 1);
        assert_eq!(plan.patches[0].0, 20);
        assert_eq!(plan.patches[0].1, json!({"instruction": "Bake at 350F"}));
        assert!(plan.steps_body.is_none());
    }

    #[test]
    fn step_update_adds_and_removes_ingredients_by_reference() {
        let mut u = update(1);
        u.remove_ingredients = Some(vec!["flour".to_string()]);
        u.add_ingredients = Some(vec![RecipeStepIngredientInput {
            food: Some("Butter".to_string()),
            header: None,
            unit: Some("g".to_string()),
            amount: Some(50.0),
            note: None,
        }]);
        let plan = plan_step_changes(&sample_steps(), vec![u], vec![], vec![], vec![]).unwrap();
        let ings = plan.patches[0].1["ingredients"].as_array().unwrap();
        assert_eq!(ings[0], json!({"id": 102, "order": 0}));
        assert_eq!(ings[1]["food"]["name"], "Butter");
        assert_eq!(ings[1]["order"], 1);
        assert_eq!(ings.len(), 2);
    }

    #[test]
    fn step_update_rejects_unknown_ingredient_and_bad_step() {
        let mut u = update(1);
        u.remove_ingredients = Some(vec!["Milk".to_string()]);
        let err = plan_step_changes(&sample_steps(), vec![u], vec![], vec![], vec![]).unwrap_err();
        assert!(err.contains("no ingredient 'Milk'"), "{err}");

        let mut u = update(3);
        u.instruction = Some("x".to_string());
        let err = plan_step_changes(&sample_steps(), vec![u], vec![], vec![], vec![]).unwrap_err();
        assert!(err.contains("step 3 does not exist"), "{err}");

        let err = plan_step_changes(&sample_steps(), vec![update(1)], vec![], vec![], vec![])
            .unwrap_err();
        assert!(err.contains("no changes"), "{err}");
    }

    #[test]
    fn keyword_changes_add_and_remove_without_touching_others() {
        let current: Vec<crate::client::types::Keyword> = serde_json::from_value(json!([
            {"id": 1, "name": "Dinner"},
            {"id": 2, "name": "Quick"}
        ]))
        .unwrap();
        let known = std::collections::HashMap::from([("vegetarian".to_string(), 7)]);
        let add = vec![
            "vegetarian".to_string(), // existing keyword → by id
            "Spicy".to_string(),      // new keyword → by name
            "dinner".to_string(),     // already on recipe → skipped
        ];
        let list = plan_keyword_changes(&current, &add, &["QUICK".to_string()], &known).unwrap();
        assert_eq!(list, json!([{"id": 1}, {"id": 7}, {"name": "Spicy"}]));

        let err = plan_keyword_changes(&current, &[], &["Lunch".to_string()], &known).unwrap_err();
        assert!(err.contains("no keyword 'Lunch'"), "{err}");
    }

    #[test]
    fn add_and_remove_steps_keep_existing_steps_by_id() {
        let adds = vec![
            new_step("Preheat", Some(0)),
            new_step("Rest", Some(1)),
            new_step("Serve", None),
        ];
        let plan = plan_step_changes(&sample_steps(), vec![], adds, vec![2], vec![]).unwrap();
        let steps = plan.steps_body.unwrap();
        let steps = steps.as_array().unwrap();
        let summary: Vec<String> = steps
            .iter()
            .map(|s| match s.get("id") {
                Some(id) => format!("id{id}@{}", s["order"]),
                None => format!("{}@{}", s["instruction"].as_str().unwrap(), s["order"]),
            })
            .collect();
        assert_eq!(summary, ["Preheat@1", "id10@2", "Rest@3", "Serve@4"]);
    }

    fn header(id: i32, text: &str, order: i32) -> serde_json::Value {
        json!({
            "id": id, "amount": 0.0, "note": text, "order": order, "is_header": true,
            "no_amount": true, "unit": null, "food": null
        })
    }

    fn food_input(name: &str) -> RecipeStepIngredientInput {
        RecipeStepIngredientInput {
            food: Some(name.to_string()),
            header: None,
            unit: None,
            amount: None,
            note: None,
        }
    }

    fn edit(food: &str) -> IngredientEdit {
        IngredientEdit {
            food: food.to_string(),
            amount: None,
            no_amount: None,
            unit: None,
            note: None,
            new_food: None,
        }
    }

    /// One step: header "For the sauce", Tomato, Salt
    fn steps_with_header() -> Vec<crate::client::types::Step> {
        serde_json::from_value(json!([
            {"id": 30, "name": "", "instruction": "Sauce", "time": null, "order": 1, "file": null,
             "ingredients": [header(300, "For the sauce", 0), ingredient(301, "Tomato", 1),
                             ingredient(302, "Salt", 2)]}
        ]))
        .unwrap()
    }

    #[test]
    fn header_ingredient_parses_and_renders_as_header() {
        let steps = steps_with_header();
        let client = TandoorClient::new("https://recipes.example".to_string());
        let view = steps_view(&steps, 1.0, &client);
        assert_eq!(
            view[0]["ingredients"][0],
            json!({"ingredient_id": 300, "header": "For the sauce"})
        );
        assert_eq!(view[0]["ingredients"][1]["food"], "Tomato");
    }

    #[test]
    fn positioned_header_patch_keeps_existing_ingredients_by_id() {
        let steps = sample_steps();
        let step = steps.iter().find(|step| step.id == 10).unwrap();
        let patch = build_header_patch(step, "For the sauce", Some(102)).unwrap();
        assert_eq!(patch.len(), 3);
        assert_eq!(patch[0], json!({"id": 101, "order": 0}));
        assert_eq!(patch[1]["note"], "For the sauce");
        assert_eq!(patch[1]["is_header"], true);
        assert_eq!(patch[2], json!({"id": 102, "order": 2}));
        assert!(build_header_patch(step, "Missing", Some(999)).is_err());
    }

    #[test]
    fn relation_candidates_match_exact_names_and_report_ambiguity() {
        let parent_steps = json!([{
            "id": 10,
            "name": "Assemble",
            "instruction": "",
            "ingredients": [ingredient(100, " Sauce ", 0)],
            "time": null,
            "order": 0,
            "file": null
        }]);
        let client = TandoorClient::new("https://recipes.example".to_string());
        let recipes = vec![
            recipe(1, "Salad", parent_steps.clone()),
            recipe(2, "sauce", json!([])),
            recipe(3, "Sauce extra", json!([])),
        ];
        let (candidates, ambiguous) = find_relation_candidates(&recipes, None, &client);
        assert_eq!(candidates.len(), 1);
        assert!(ambiguous.is_empty());
        assert_eq!(candidates[0]["source"]["step_id"], 10);
        assert_eq!(candidates[0]["source"]["ingredient_id"], 100);
        assert_eq!(candidates[0]["proposed_recipe"]["id"], 2);

        let duplicate = vec![
            recipe(1, "Salad", parent_steps),
            recipe(2, "Sauce", json!([])),
            recipe(3, " sauce ", json!([])),
        ];
        let (candidates, ambiguous) = find_relation_candidates(&duplicate, None, &client);
        assert!(candidates.is_empty());
        assert_eq!(ambiguous.len(), 1);
    }

    #[test]
    fn header_input_validation_and_request() {
        let mut h = food_input("x");
        h.food = None;
        h.header = Some("For the sauce".to_string());
        assert!(validate_ingredient(&h).is_ok());
        let req = build_ingredient_request(h, 0);
        assert!(req.is_header && req.food.is_none());
        assert_eq!(req.note.as_deref(), Some("For the sauce"));

        let mut both = food_input("Tomato");
        both.header = Some("Sauce".to_string());
        assert!(validate_ingredient(&both).is_err());

        let mut neither = food_input("x");
        neither.food = None;
        assert!(validate_ingredient(&neither).is_err());
    }

    #[test]
    fn update_ingredients_changes_only_given_fields() {
        let mut e = edit("tomato");
        e.amount = Some(250.0);
        e.unit = Some("".to_string());
        let mut s = edit("Salt");
        s.no_amount = Some(true);
        let mut h = edit("for the sauce");
        h.note = Some("Sauce".to_string());
        let mut u = update(1);
        u.update_ingredients = Some(vec![e, s, h]);
        let plan =
            plan_step_changes(&steps_with_header(), vec![u], vec![], vec![], vec![]).unwrap();
        let ings = &plan.patches[0].1["ingredients"];
        assert_eq!(ings[0], json!({"id": 300, "order": 0, "note": "Sauce"}));
        assert_eq!(
            ings[1],
            json!({"id": 301, "order": 1, "amount": "250", "no_amount": false, "unit": null})
        );
        assert_eq!(ings[2], json!({"id": 302, "order": 2, "no_amount": true}));
    }

    #[test]
    fn update_ingredients_rejects_bad_edits() {
        let run = |e: IngredientEdit| {
            let mut u = update(1);
            u.update_ingredients = Some(vec![e]);
            plan_step_changes(&steps_with_header(), vec![u], vec![], vec![], vec![]).unwrap_err()
        };
        let mut amount_on_header = edit("For the sauce");
        amount_on_header.amount = Some(1.0);
        assert!(run(amount_on_header).contains("section header"));
        let mut missing = edit("Milk");
        missing.amount = Some(1.0);
        assert!(run(missing).contains("no ingredient 'Milk'"));
        assert!(run(edit("Tomato")).contains("no changes"));

        // Same food twice in one step is ambiguous
        let dup: Vec<crate::client::types::Step> = serde_json::from_value(json!([
            {"id": 40, "name": "", "instruction": "x", "time": null, "order": 1, "file": null,
             "ingredients": [ingredient(401, "Salt", 0), ingredient(402, "Salt", 1)]}
        ]))
        .unwrap();
        let mut e = edit("salt");
        e.amount = Some(1.0);
        let mut u = update(1);
        u.update_ingredients = Some(vec![e]);
        let err = plan_step_changes(&dup, vec![u], vec![], vec![], vec![]).unwrap_err();
        assert!(err.contains("2 ingredients named"), "{err}");
    }

    #[test]
    fn move_steps_reorders_by_reference() {
        // sample_steps: step 1 = id 10, step 2 = id 20
        let moves = vec![MoveStepInput {
            step_number: 2,
            after_step: 0,
        }];
        let plan = plan_step_changes(&sample_steps(), vec![], vec![], vec![], moves).unwrap();
        assert_eq!(
            plan.steps_body.unwrap(),
            json!([{"id": 20, "order": 1}, {"id": 10, "order": 2}])
        );

        let bad = vec![MoveStepInput {
            step_number: 1,
            after_step: 5,
        }];
        assert!(plan_step_changes(&sample_steps(), vec![], vec![], vec![], bad).is_err());
    }

    #[test]
    fn duplicate_food_groups_match_case_and_plurals() {
        let foods: Vec<crate::client::types::Food> = serde_json::from_value(
            json!([
                {"id": 1, "name": "Tomato", "plural_name": null},
                {"id": 2, "name": "tomatoes", "plural_name": null},
                {"id": 3, "name": "Egg", "plural_name": "Eggs"},
                {"id": 4, "name": "eggs", "plural_name": null},
                {"id": 5, "name": "Butter", "plural_name": null},
                {"id": 6, "name": "Peanut  Butter", "plural_name": null},
                {"id": 7, "name": "peanut butter", "plural_name": null}
            ])
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let mut f = f.clone();
                for (k, v) in [
                    ("description", json!(null)),
                    ("recipe", json!(null)),
                    ("food_onhand", json!(false)),
                    ("supermarket_category", json!(null)),
                    ("inherit_fields", json!([])),
                    ("properties", json!([])),
                ] {
                    f[k] = v;
                }
                f
            })
            .collect(),
        )
        .unwrap();
        let ids: Vec<Vec<i32>> = find_duplicate_food_groups(&foods)
            .iter()
            .map(|g| g.iter().map(|f| f.id).collect())
            .collect();
        assert_eq!(ids, vec![vec![1, 2], vec![3, 4], vec![6, 7]]);
    }

    fn parsed(food: &str, amount: Option<f64>, unit: Option<&str>) -> ParsedShoppingItem {
        ParsedShoppingItem {
            food: food.to_string(),
            amount,
            unit: unit.map(str::to_string),
        }
    }

    #[test]
    fn parse_shopping_text_reads_amount_unit_and_food() {
        let p = |t: &str| parse_shopping_text(t, &["bunch".to_string(), "head".to_string()]);
        assert_eq!(p("3 lemons"), parsed("lemons", Some(3.0), None));
        assert_eq!(p("milk"), parsed("milk", None, None));
        assert_eq!(
            p("2 lb chicken thighs"),
            parsed("chicken thighs", Some(2.0), Some("lb"))
        );
        assert_eq!(
            p("1 1/2 cups flour"),
            parsed("flour", Some(1.5), Some("cups"))
        );
        assert_eq!(
            p("1/2 cup parsley"),
            parsed("parsley", Some(0.5), Some("cup"))
        );
        assert_eq!(p("½ lb butter"), parsed("butter", Some(0.5), Some("lb")));
        assert_eq!(p("a dozen eggs"), parsed("eggs", Some(12.0), None));
        assert_eq!(
            p("2 cans of tomatoes"),
            parsed("tomatoes", Some(2.0), Some("cans"))
        );
        assert_eq!(p("lemons x3"), parsed("lemons", Some(3.0), None));
        assert_eq!(p("two avocados"), parsed("avocados", Some(2.0), None));
        assert_eq!(
            p("1.5 kg potatoes."),
            parsed("potatoes", Some(1.5), Some("kg"))
        );
        assert_eq!(
            p("a head of lettuce"),
            parsed("lettuce", Some(1.0), Some("head"))
        );
        // A unit word with nothing after it is the food, not a unit
        assert_eq!(p("3 cans"), parsed("cans", Some(3.0), None));
        // Unit words are only units after an amount
        assert_eq!(p("can opener"), parsed("can opener", None, None));
    }

    #[test]
    fn resolve_unit_name_reuses_existing_units() {
        let units: Vec<crate::client::types::Unit> = serde_json::from_value(json!([
            {"id": 1, "name": "pound", "plural_name": "pounds", "description": null, "base_unit": null, "type_": null},
            {"id": 2, "name": "Bunch", "plural_name": null, "description": null, "base_unit": null, "type_": null}
        ]))
        .unwrap();
        assert_eq!(resolve_unit_name("lbs", &units), "pound");
        assert_eq!(resolve_unit_name("bunches", &units), "Bunch");
        assert_eq!(resolve_unit_name("ounces", &units), "oz");
        assert_eq!(resolve_unit_name("sleeve", &units), "sleeve");
    }

    fn shopping_entries() -> Vec<crate::client::types::ShoppingListEntry> {
        let entry = |id: i32, food_id: i32, food: &str, checked: bool, list_recipe: Option<i32>| {
            json!({
                "id": id, "amount": 1.0, "checked": checked, "unit": null,
                "list_recipe": list_recipe,
                "food": {"id": food_id, "name": food, "plural_name": null}
            })
        };
        serde_json::from_value(json!([
            entry(1, 10, "Lemon", false, None),
            entry(2, 10, "Lemon", false, Some(5)),
            entry(3, 11, "Lemon Juice", false, None),
            entry(4, 12, "Milk", true, None),
            entry(5, 13, "Lime Zest", false, None)
        ]))
        .unwrap()
    }

    #[test]
    fn match_shopping_entries_prefers_exact_and_rejects_ambiguous() {
        let entries = shopping_entries();
        let ids = |r: ShoppingRef| -> Result<Vec<i32>, String> {
            match_shopping_entries(&entries, &r).map(|m| m.iter().map(|e| e.id).collect())
        };
        // "lemons" is Lemon (both lines), not Lemon Juice
        assert_eq!(ids(ShoppingRef::Name("lemons".into())), Ok(vec![1, 2]));
        assert_eq!(ids(ShoppingRef::Name("3 lemons".into())), Ok(vec![1, 2]));
        assert_eq!(ids(ShoppingRef::Id(4)), Ok(vec![4]));
        // Unique partial match is fine; ambiguous one is an error
        assert_eq!(ids(ShoppingRef::Name("zest".into())), Ok(vec![5]));
        assert_eq!(ids(ShoppingRef::Name("juice".into())), Ok(vec![3]));
        assert!(ids(ShoppingRef::Name("L".into()))
            .unwrap_err()
            .contains("several"));
        assert!(ids(ShoppingRef::Name("bread".into()))
            .unwrap_err()
            .contains("not on"));
        assert!(ids(ShoppingRef::Id(99)).is_err());
    }

    #[test]
    fn mergeable_entry_skips_recipe_lines_checked_lines_and_other_units() {
        let entries = shopping_entries();
        assert_eq!(
            find_mergeable_entry(&entries, 10, None).map(|e| e.id),
            Some(1)
        );
        assert!(find_mergeable_entry(&entries, 10, Some("bag")).is_none());
        assert!(find_mergeable_entry(&entries, 12, None).is_none()); // checked
    }

    #[test]
    fn shopping_ref_accepts_names_and_ids() {
        let refs: Vec<ShoppingRef> = serde_json::from_value(json!(["lemons", 42])).unwrap();
        assert!(matches!(&refs[0], ShoppingRef::Name(n) if n == "lemons"));
        assert!(matches!(refs[1], ShoppingRef::Id(42)));
        let items: Vec<ShoppingItemInput> =
            serde_json::from_value(json!(["3 lemons", {"name": "milk", "amount": 2}])).unwrap();
        assert!(matches!(&items[1], ShoppingItemInput::Structured(i) if i.food == "milk"));
    }

    #[test]
    fn resolve_named_exact_then_unique_partial() {
        let items = vec![
            json!({"id": 1, "name": "Costco"}),
            json!({"id": 2, "name": "Trader Joe's"}),
            json!({"id": 3, "name": "Costco Business"}),
        ];
        let id =
            |t: NameOrId| resolve_named(&items, &t, "store").map(|v| v["id"].as_i64().unwrap());
        assert_eq!(id(NameOrId::Name("costco".into())), Ok(1)); // exact beats partial
        assert_eq!(id(NameOrId::Name("trader".into())), Ok(2));
        assert_eq!(id(NameOrId::Id(3)), Ok(3));
        assert!(id(NameOrId::Name("co".into()))
            .unwrap_err()
            .contains("several"));
        assert!(id(NameOrId::Name("aldi".into()))
            .unwrap_err()
            .contains("No store"));
    }

    #[test]
    fn supermarket_view_orders_aisles() {
        let store = json!({
            "id": 5, "name": "Store", "description": null,
            "category_to_supermarket": [
                {"id": 9, "order": 2, "category": {"name": "Dairy"}},
                {"id": 8, "order": 0, "category": {"name": "Produce"}},
                {"id": 7, "order": 1, "category": {"name": "Bakery"}}
            ]
        });
        assert_eq!(
            supermarket_view(&store)["category_order"],
            json!(["Produce", "Bakery", "Dairy"])
        );
    }

    #[test]
    fn recipe_group_view_uses_recipe_name_and_its_items() {
        let entries = shopping_entries(); // entry 2 belongs to group 5
        let group = json!({"id": 5, "name": "", "recipe": 1, "mealplan": null, "servings": 4.0,
                           "recipe_data": {"name": "Lemon Chicken"}});
        let view = recipe_group_view(&group, &entries);
        assert_eq!(view["name"], "Lemon Chicken");
        assert_eq!(view["items"].as_array().unwrap().len(), 1);
        assert_eq!(view["items"][0]["food"], "Lemon");
    }

    #[test]
    fn image_extension_detects_by_signature() {
        use crate::client::client::image_extension;
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0, 0]), Some("jpg"));
        assert_eq!(image_extension(b"\x89PNG\r\n\x1a\n"), Some("png"));
        assert_eq!(image_extension(b"GIF89a"), Some("gif"));
        assert_eq!(image_extension(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_extension(b"<!doctype html>"), None);
        assert_eq!(image_extension(b""), None);
    }
}

#[cfg(test)]
mod annotation_tests {
    use super::*;

    #[test]
    fn every_tool_declares_annotations() {
        let tools = TandoorMcpServer::tool_router().list_all();
        assert_eq!(tools.len(), 66, "unexpected tool count");

        for tool in &tools {
            let name = tool.name.as_ref();
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{name} has no annotations"));

            let read_only = ["get_", "search_", "find_", "suggest_"]
                .iter()
                .any(|prefix| name.starts_with(prefix));
            if read_only {
                assert_eq!(
                    annotations.read_only_hint,
                    Some(true),
                    "{name} should be read-only"
                );
            }

            let destructive = name.starts_with("delete_")
                || name.starts_with("remove_from_")
                || name == "merge_foods"
                || name == "clear_shopping_list";
            if destructive {
                assert_eq!(
                    annotations.destructive_hint,
                    Some(true),
                    "{name} should be destructive"
                );
            }
        }
    }
}
