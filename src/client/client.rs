//! HTTP client implementation for the Tandoor API.

use crate::client::{auth::TandoorAuth, types::*};
use anyhow::{Context, Result};
use reqwest::{
    header::{CONTENT_LENGTH, LOCATION},
    redirect::Policy,
    Client, Url,
};
use std::time::Duration;

const MAX_PAGINATION_PAGES: usize = 100;
const MAX_EMBEDDED_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_RECIPE_IMAGE_REDIRECTS: usize = 3;

/// # Tandoor HTTP Client
///
/// A comprehensive HTTP client for interacting with the Tandoor recipe management API.
/// Handles authentication, recipes, shopping lists, meal planning, and more.
///
/// ## Features
///
/// - **OAuth2 Authentication**: Handles Tandoor's token-based authentication
/// - **Recipe Management**: Search, create, retrieve, and manage recipes
/// - **Shopping Lists**: Add items, manage lists, check off completed items
/// - **Meal Planning**: Plan meals and add ingredients to shopping lists
/// - **Food Search**: Find foods and ingredients in the database
/// - **Import Support**: Import recipes from URLs
/// - **Rate Limit Handling**: Works with Tandoor's authentication limits
///
/// ## Example
///
/// ```no_run
/// use mcp_tandoor::client::TandoorClient;
///
/// # async fn example() -> anyhow::Result<()> {
/// let mut client = TandoorClient::new("http://localhost:8080".to_string());
///
/// // Authenticate
/// client.authenticate("username".to_string(), "password".to_string()).await?;
///
/// // Search recipes
/// let recipes = client.search_recipes(Some("pasta"), Some(10)).await?;
///
/// // Get recipe details  
/// if let Some(recipe) = recipes.results.first() {
///     let details = client.get_recipe(recipe.id).await?;
///     println!("Recipe: {}", details.name);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct TandoorClient {
    /// Base URL of the Tandoor server
    base_url: String,
    /// HTTP client for making requests
    client: Client,
    /// Authentication handler
    auth: TandoorAuth,
}

impl TandoorClient {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            auth: TandoorAuth::new(base_url.clone()),
            base_url,
        }
    }

    pub async fn authenticate(&mut self, username: String, password: String) -> Result<()> {
        self.auth.authenticate(username, password).await
    }

    pub fn is_authenticated(&self) -> bool {
        self.auth.is_authenticated()
    }

    pub fn get_token_preview(&self) -> Option<String> {
        self.auth
            .get_token()
            .map(|t| format!("{}...", &t[..t.len().min(10)]))
    }

    pub fn set_token(&mut self, token: String) {
        self.auth.set_token(token);
    }

    pub fn get_token(&self) -> Option<&str> {
        self.auth.get_token()
    }

    /// Browser-facing URL for a recipe in the configured Tandoor instance.
    pub fn recipe_url(&self, recipe_id: i32) -> String {
        format!("{}/recipe/{recipe_id}", self.base_url.trim_end_matches('/'))
    }

    fn get_auth_header(&self) -> Result<String> {
        match self.auth.get_token() {
            Some(token) => {
                tracing::trace!(
                    "Using authentication token: {}...",
                    &token[..token.len().min(10)]
                );
                // Tandoor uses OAuth2 access tokens with Bearer authentication
                Ok(format!("Bearer {token}"))
            }
            None => {
                tracing::warn!("Attempted to make authenticated request without valid token - server will handle re-authentication if credentials are available");
                anyhow::bail!("Not authenticated - please verify credentials and re-authenticate")
            }
        }
    }

    // Recipe operations
    /// Fetches a further page of recipe search results (a `next` URL from Tandoor).
    pub async fn get_recipe_page(&self, url: &str) -> Result<PaginatedResponse<Recipe>> {
        let auth_header = self.get_auth_header()?;
        let response = self
            .client
            .get(url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to search recipes: {status} - {error_body}");
        }
        Ok(response.json().await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn search_recipes(
        &self,
        query: Option<&str>,
        limit: Option<i32>,
        keywords: Option<&[i64]>,
        foods: Option<&[i64]>,
        max_cooking_time: Option<i64>,
        min_rating: Option<i64>,
        random: Option<bool>,
    ) -> Result<PaginatedResponse<Recipe>> {
        let auth_header = self.get_auth_header()?;
        let mut url = format!("{}/api/recipe/", self.base_url);

        let mut params = vec![];
        if let Some(q) = query {
            params.push(format!("query={}", urlencoding::encode(q)));
        }
        if let Some(l) = limit {
            params.push(format!("page_size={l}"));
        }
        if let Some(kws) = keywords {
            for kw in kws {
                params.push(format!("keywords_or={kw}"));
            }
        }
        if let Some(fs) = foods {
            for f in fs {
                params.push(format!("foods_or={f}"));
            }
        }
        if let Some(t) = max_cooking_time {
            params.push(format!("cooking_time={t}"));
        }
        if let Some(r) = min_rating {
            params.push(format!("rating={r}"));
        }
        if let Some(true) = random {
            params.push("random=true".to_string());
        }

        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }

        tracing::debug!("Searching recipes with URL: {}", url);
        tracing::trace!("Search parameters - query: {:?}, limit: {:?}", query, limit);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error searching recipes: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Recipe search response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to search recipes with status {}: {}",
                status,
                error_body
            );
            anyhow::bail!("Failed to search recipes: {status} - {error_body}");
        }

        let recipes = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse recipe search response: {}", e);
            anyhow::anyhow!("Invalid response format: {e}")
        })?;

        tracing::debug!("Recipe search successful");
        Ok(recipes)
    }

    pub async fn get_recipe(&self, id: i32) -> Result<Recipe> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe/{}/", self.base_url, id);

        tracing::debug!("Getting recipe details for ID: {}", id);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error getting recipe {}: {}", id, e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Get recipe response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to get recipe {} with status {}: {}",
                id,
                status,
                error_body
            );

            match status.as_u16() {
                404 => anyhow::bail!("Recipe with ID {id} not found"),
                403 => anyhow::bail!("Access denied to recipe {id}"),
                _ => anyhow::bail!("Failed to get recipe: {status} - {error_body}"),
            }
        }

        let recipe = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse recipe response: {}", e);
            anyhow::anyhow!("Invalid response format: {e}")
        })?;

        tracing::debug!("Successfully retrieved recipe: {}", id);
        Ok(recipe)
    }

    pub async fn create_recipe(&self, request: CreateRecipeRequest) -> Result<Recipe> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe/", self.base_url);

        tracing::debug!("Creating new recipe: {}", request.name);
        tracing::trace!(
            "Recipe details: servings={:?}, working_time={:?}, waiting_time={:?}",
            request.servings,
            request.working_time,
            request.waiting_time
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error creating recipe: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Create recipe response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to create recipe '{}' with status {}: {}",
                request.name,
                status,
                error_body
            );
            anyhow::bail!("Failed to create recipe: {status} - {error_body}");
        }

        let recipe: Recipe = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse create recipe response: {}", e);
            anyhow::anyhow!("Invalid response format: {e}")
        })?;

        tracing::info!(
            "Successfully created recipe '{}' with ID: {}",
            request.name,
            recipe.id
        );
        Ok(recipe)
    }

    // Food operations
    pub async fn search_foods(
        &self,
        query: &str,
        limit: Option<i32>,
    ) -> Result<PaginatedResponse<Food>> {
        let auth_header = self.get_auth_header()?;
        let mut url = format!(
            "{}/api/food/?query={}",
            self.base_url,
            urlencoding::encode(query)
        );

        if let Some(l) = limit {
            url.push_str(&format!("&page_size={l}"));
        }

        tracing::debug!(
            "Searching foods with query: '{}', limit: {:?}",
            query,
            limit
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error searching foods: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Food search response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to search foods '{}' with status {}: {}",
                query,
                status,
                error_body
            );
            anyhow::bail!("Failed to search foods: {status} - {error_body}");
        }

        let foods: PaginatedResponse<Food> = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse food search response: {}", e);
            anyhow::anyhow!("Invalid response format: {e}")
        })?;

        tracing::debug!("Food search successful, found {} results", foods.count);
        Ok(foods)
    }

    /// Create a food. Tandoor returns the existing food unchanged if one with the same
    /// name or plural name already exists, so callers should verify `food_onhand`.
    pub async fn create_food(&self, request: NewFoodRequest) -> Result<Food> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/food/", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to create food: {status} - {error_body}");
        }
        let food = response.json().await?;
        Ok(food)
    }

    pub async fn get_food(&self, id: i32) -> Result<Food> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/food/{}/", self.base_url, id);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to get food {id}: {status} - {error_body}");
        }
        let food = response.json().await?;
        Ok(food)
    }

    /// Every food in the space, following pagination.
    pub async fn list_all_foods(&self) -> Result<Vec<Food>> {
        let auth_header = self.get_auth_header()?;
        let mut url = Some(format!("{}/api/food/?page_size=100", self.base_url));
        let mut foods = Vec::new();

        while let Some(page_url) = url {
            let response = self
                .client
                .get(&page_url)
                .header("Authorization", &auth_header)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

            let status = response.status();
            if !status.is_success() {
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("Failed to list foods: {status} - {error_body}");
            }
            let page: PaginatedResponse<Food> = response.json().await?;
            foods.extend(page.results);
            url = page.next;
        }
        Ok(foods)
    }

    /// Merge `source` into `target`: every recipe, shopping entry, etc. that used the
    /// source food is moved to the target, and the source food is deleted.
    pub async fn merge_food(&self, source: i32, target: i32) -> Result<()> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/food/{}/merge/{}/", self.base_url, source, target);

        let response = self
            .client
            .put(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to merge foods: {status} - {error_body}");
        }
        Ok(())
    }

    /// Add a recipe to the shopping list through Tandoor's recipe shopping endpoint, so
    /// entries are linked to the recipe and scaled to `servings`. Only the given
    /// ingredient IDs are added (must be non-empty: an empty list adds everything).
    pub async fn add_recipe_to_shopping_list(
        &self,
        recipe_id: i32,
        servings: i32,
        ingredient_ids: &[i32],
    ) -> Result<()> {
        anyhow::ensure!(!ingredient_ids.is_empty(), "No ingredients to add");
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe/{}/shopping/", self.base_url, recipe_id);

        let response = self
            .client
            .put(&url)
            .header("Authorization", auth_header)
            .json(&serde_json::json!({"servings": servings, "ingredients": ingredient_ids}))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to add recipe to shopping list: {status} - {error_body}");
        }
        Ok(())
    }

    pub async fn update_food_availability(&self, food_id: i32, available: bool) -> Result<Food> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/food/{}/", self.base_url, food_id);
        let request = UpdateFoodRequest {
            food_onhand: Some(available),
        };

        let response = self
            .client
            .patch(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to update food availability: {}", response.status());
        }

        let food = response.json().await?;
        Ok(food)
    }

    // Shopping list operations
    pub async fn get_shopping_list(&self) -> Result<PaginatedResponse<ShoppingListEntry>> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/shopping-list-entry/", self.base_url);

        tracing::debug!("Fetching shopping list");

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error getting shopping list: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Shopping list response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to get shopping list with status {}: {}",
                status,
                error_body
            );
            anyhow::bail!("Failed to get shopping list: {status} - {error_body}");
        }

        // Handle both paginated response and simple array response
        let response_text = response.text().await.map_err(|e| {
            tracing::error!("Failed to read shopping list response: {}", e);
            anyhow::anyhow!("Failed to read response: {e}")
        })?;

        let shopping_list: PaginatedResponse<ShoppingListEntry> =
            if response_text.trim().starts_with('[') {
                // Simple array response (empty list case)
                let entries: Vec<ShoppingListEntry> = serde_json::from_str(&response_text)
                    .map_err(|e| {
                        tracing::error!("Failed to parse shopping list array response: {}", e);
                        anyhow::anyhow!("Invalid array response format: {e}")
                    })?;
                PaginatedResponse {
                    count: entries.len() as i32,
                    next: None,
                    previous: None,
                    results: entries,
                }
            } else {
                // Paginated response
                serde_json::from_str(&response_text).map_err(|e| {
                    tracing::error!("Failed to parse shopping list paginated response: {}", e);
                    anyhow::anyhow!("Invalid paginated response format: {e}")
                })?
            };

        tracing::debug!(
            "Successfully retrieved shopping list with {} items",
            shopping_list.count
        );
        Ok(shopping_list)
    }

    pub async fn add_to_shopping_list(
        &self,
        request: CreateShoppingListEntryRequest,
    ) -> Result<ShoppingListEntry> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/shopping-list-entry/", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to add to shopping list: {}", response.status());
        }

        let entry = response.json().await?;
        Ok(entry)
    }

    /// Downloads an untrusted image through the SSRF-safe, bounded image loader.
    pub async fn download_image(&self, url: &str) -> Result<(Vec<u8>, &'static str)> {
        crate::image::download_public_image(url).await
    }

    /// Fetches a stored recipe photo for an MCP image content block.
    ///
    /// Tandoor may return either a relative media path or an absolute URL based
    /// on its reverse-proxy configuration. In both cases the path is fetched
    /// from the configured Tandoor origin, preventing token disclosure to an
    /// arbitrary host.
    pub async fn get_recipe_image(&self, image: &str) -> Result<(Vec<u8>, &'static str)> {
        let mut url = match recipe_image_source(&self.base_url, image)? {
            RecipeImageSource::Tandoor(url) => url,
            RecipeImageSource::Public(url) => return public_recipe_image(&url).await,
        };
        let auth_header = self.get_auth_header()?;
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .context("Could not create recipe image client")?;

        for redirect_count in 0..=MAX_RECIPE_IMAGE_REDIRECTS {
            let mut response = client
                .get(url.clone())
                .header("Authorization", &auth_header)
                .send()
                .await
                .context("Failed to fetch recipe image from Tandoor")?;

            if response.status().is_redirection() {
                anyhow::ensure!(
                    redirect_count < MAX_RECIPE_IMAGE_REDIRECTS,
                    "Tandoor recipe image redirected too many times"
                );
                let location = response
                    .headers()
                    .get(LOCATION)
                    .context("Tandoor recipe image redirect omitted Location")?
                    .to_str()
                    .context("Tandoor recipe image redirect Location is invalid")?;
                match redirected_recipe_image_source(&self.base_url, &url, location)? {
                    RecipeImageSource::Tandoor(redirected) => url = redirected,
                    RecipeImageSource::Public(redirected) => {
                        return public_recipe_image(&redirected).await;
                    }
                }
                continue;
            }

            anyhow::ensure!(
                response.status().is_success(),
                "Tandoor recipe image request failed: {}",
                response.status()
            );
            if let Some(length) = response.headers().get(CONTENT_LENGTH) {
                let length: usize = length
                    .to_str()
                    .context("Invalid recipe image Content-Length")?
                    .parse()
                    .context("Invalid recipe image Content-Length")?;
                anyhow::ensure!(
                    length <= MAX_EMBEDDED_IMAGE_BYTES,
                    "Recipe image is larger than 10 MB"
                );
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("unknown")
                .to_string();

            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .context("Failed while reading recipe image")?
            {
                anyhow::ensure!(
                    bytes.len().saturating_add(chunk.len()) <= MAX_EMBEDDED_IMAGE_BYTES,
                    "Recipe image is larger than 10 MB"
                );
                bytes.extend_from_slice(&chunk);
            }

            let mime_type = match image_extension(&bytes) {
                Some("jpg") => "image/jpeg",
                Some("png") => "image/png",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                _ => anyhow::bail!(
                    "Tandoor recipe image has an unsupported file signature \
                     (path: {}, content-type: {content_type})",
                    url.path()
                ),
            };
            return Ok((bytes, mime_type));
        }

        unreachable!("redirect loop returns an image or an error")
    }

    /// Sets a recipe's photo by uploading the image file. (Tandoor 2.6's own
    /// `image_url` option crashes, so callers download the image first.)
    pub async fn upload_recipe_image(
        &self,
        recipe_id: i32,
        bytes: Vec<u8>,
        extension: &str,
    ) -> Result<serde_json::Value> {
        let mime = match extension {
            "png" => "image/png",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => "image/jpeg",
        };
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(format!("recipe.{extension}"))
            .mime_str(mime)?;
        self.send_recipe_image_form(
            recipe_id,
            reqwest::multipart::Form::new().part("image", part),
        )
        .await
    }

    /// Removes a recipe's photo. A form without an `image` field is how Tandoor clears it
    /// (sending an empty `image` crashes Tandoor 2.6).
    pub async fn clear_recipe_image(&self, recipe_id: i32) -> Result<()> {
        self.send_recipe_image_form(
            recipe_id,
            reqwest::multipart::Form::new().text("clear", "1"),
        )
        .await
        .map(|_| ())
    }

    async fn send_recipe_image_form(
        &self,
        recipe_id: i32,
        form: reqwest::multipart::Form,
    ) -> Result<serde_json::Value> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe/{recipe_id}/image/", self.base_url);
        let response = self
            .client
            .put(&url)
            .header("Authorization", auth_header)
            .multipart(form)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to update recipe image: {status} - {error_body}");
        }
        Ok(response.json().await.unwrap_or(serde_json::Value::Null))
    }

    // Generic JSON helpers for simple resources (shopping lists, supermarkets,
    // categories, ...). `resource` is the API path segment, e.g. "supermarket".

    /// Every object of a resource, following pagination.
    pub async fn api_list_all(&self, resource: &str) -> Result<Vec<serde_json::Value>> {
        let auth_header = self.get_auth_header()?;
        let mut url = Some(format!("{}/api/{resource}/?page_size=100", self.base_url));
        let mut items = Vec::new();
        let mut pages = 0;

        while let Some(page_url) = url {
            pages += 1;
            anyhow::ensure!(
                pages <= MAX_PAGINATION_PAGES,
                "Refusing to read more than {MAX_PAGINATION_PAGES} pages of {resource}"
            );
            let response = self
                .client
                .get(&page_url)
                .header("Authorization", &auth_header)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;
            let status = response.status();
            if !status.is_success() {
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("Failed to list {resource}: {status} - {error_body}");
            }
            let body: serde_json::Value = response.json().await?;
            match body {
                serde_json::Value::Array(page) => {
                    items.extend(page);
                    url = None;
                }
                mut page => {
                    if let Some(serde_json::Value::Array(results)) = page.get_mut("results") {
                        items.append(results);
                    }
                    url = page
                        .get("next")
                        .and_then(|n| n.as_str())
                        .map(str::to_string);
                }
            }
        }
        Ok(items)
    }

    pub async fn api_get(&self, resource: &str, id: i64) -> Result<serde_json::Value> {
        self.api_send(reqwest::Method::GET, resource, Some(id), None)
            .await
    }

    pub async fn api_create(
        &self,
        resource: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.api_send(reqwest::Method::POST, resource, None, Some(body))
            .await
    }

    pub async fn api_update(
        &self,
        resource: &str,
        id: i64,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.api_send(reqwest::Method::PATCH, resource, Some(id), Some(body))
            .await
    }

    pub async fn api_delete(&self, resource: &str, id: i64) -> Result<()> {
        self.api_send(reqwest::Method::DELETE, resource, Some(id), None)
            .await
            .map(|_| ())
    }

    async fn api_send(
        &self,
        method: reqwest::Method,
        resource: &str,
        id: Option<i64>,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let auth_header = self.get_auth_header()?;
        let url = match id {
            Some(id) => format!("{}/api/{resource}/{id}/", self.base_url),
            None => format!("{}/api/{resource}/", self.base_url),
        };
        let mut request = self
            .client
            .request(method.clone(), &url)
            .header("Authorization", auth_header);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("{method} {resource} failed: {status} - {error_body}");
        }
        let text = response.text().await.unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Every shopping list entry, following pagination.
    pub async fn get_all_shopping_entries(&self) -> Result<Vec<ShoppingListEntry>> {
        let auth_header = self.get_auth_header()?;
        let mut url = Some(format!(
            "{}/api/shopping-list-entry/?page_size=100",
            self.base_url
        ));
        let mut entries = Vec::new();
        let mut pages = 0;

        while let Some(page_url) = url {
            pages += 1;
            anyhow::ensure!(
                pages <= MAX_PAGINATION_PAGES,
                "Refusing to read more than {MAX_PAGINATION_PAGES} shopping-list pages"
            );
            let response = self
                .client
                .get(&page_url)
                .header("Authorization", &auth_header)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

            let status = response.status();
            if !status.is_success() {
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("Failed to get shopping list: {status} - {error_body}");
            }
            let page: PaginatedResponse<ShoppingListEntry> = response
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("Invalid shopping list response: {e}"))?;
            entries.extend(page.results);
            url = page.next;
        }
        Ok(entries)
    }

    /// Add one entry. The unit is given by name; Tandoor reuses or creates it.
    pub async fn add_shopping_entry(
        &self,
        food_id: i32,
        unit: Option<&str>,
        amount: f64,
        shopping_lists: &[NamedRef],
    ) -> Result<ShoppingListEntry> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/shopping-list-entry/", self.base_url);
        let body = serde_json::json!({
            "food": food_id,
            "unit": unit.map(|name| serde_json::json!({"name": name})),
            "amount": amount,
            "shopping_lists": shopping_lists
        });

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to add to shopping list: {status} - {error_body}");
        }
        let entry = response.json().await?;
        Ok(entry)
    }

    pub async fn add_bulk_to_shopping_list(
        &self,
        entries: Vec<CreateShoppingListEntryRequest>,
    ) -> Result<Vec<ShoppingListEntry>> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/shopping-list-entry/bulk/", self.base_url);
        let entry_count = entries.len();
        tracing::debug!("Adding {} items to shopping list in bulk", entry_count);
        tracing::trace!("Bulk shopping list items: {:?}", entries);

        let request = BulkShoppingListRequest { entries };

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error adding bulk items to shopping list: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Bulk add response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to add bulk items with status {}: {}",
                status,
                error_body
            );
            anyhow::bail!("Failed to add bulk to shopping list: {status} - {error_body}");
        }

        let entries: Vec<ShoppingListEntry> = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse bulk add response: {}", e);
            anyhow::anyhow!("Invalid response format: {e}")
        })?;

        tracing::info!(
            "Successfully added {} items to shopping list",
            entries.len()
        );
        Ok(entries)
    }

    pub async fn update_shopping_list_entry(
        &self,
        entry_id: i32,
        request: UpdateShoppingListEntryRequest,
    ) -> Result<ShoppingListEntry> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/shopping-list-entry/{}/", self.base_url, entry_id);

        let response = self
            .client
            .patch(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to update shopping list entry: {}",
                response.status()
            );
        }

        let entry = response.json().await?;
        Ok(entry)
    }

    pub async fn delete_shopping_list_entry(&self, entry_id: i32) -> Result<()> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/shopping-list-entry/{}/", self.base_url, entry_id);

        let response = self
            .client
            .delete(&url)
            .header("Authorization", auth_header)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to delete shopping list entry: {}",
                response.status()
            );
        }

        Ok(())
    }

    // Meal planning operations
    pub async fn get_meal_plans(
        &self,
        from_date: Option<&str>,
        to_date: Option<&str>,
    ) -> Result<PaginatedResponse<MealPlan>> {
        let auth_header = self.get_auth_header()?;
        let mut url = format!("{}/api/meal-plan/", self.base_url);

        let mut params = vec![];
        if let Some(from) = from_date {
            params.push(format!("from_date={from}"));
        }
        if let Some(to) = to_date {
            params.push(format!("to_date={to}"));
        }

        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }

        tracing::debug!("Getting meal plans from {:?} to {:?}", from_date, to_date);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error getting meal plans: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Meal plans response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to get meal plans with status {}: {}",
                status,
                error_body
            );
            anyhow::bail!("Failed to get meal plans: {status} - {error_body}");
        }

        let meal_plans: PaginatedResponse<MealPlan> = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse meal plans response: {}", e);
            anyhow::anyhow!("Invalid response format: {e}")
        })?;

        tracing::debug!("Successfully retrieved {} meal plans", meal_plans.count);
        Ok(meal_plans)
    }

    pub async fn create_meal_plan(&self, request: CreateMealPlanRequest) -> Result<MealPlan> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/meal-plan/", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to create meal plan: {status} - {error_body}");
        }

        let meal_plan = response.json().await?;
        Ok(meal_plan)
    }

    pub async fn update_meal_plan(
        &self,
        plan_id: i32,
        body: serde_json::Value,
    ) -> Result<MealPlan> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/meal-plan/{}/", self.base_url, plan_id);

        let response = self
            .client
            .patch(&url)
            .header("Authorization", auth_header)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to update meal plan {plan_id}: {status} - {error_body}");
        }

        let meal_plan = response.json().await?;
        Ok(meal_plan)
    }

    pub async fn delete_meal_plan(&self, plan_id: i32) -> Result<()> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/meal-plan/{}/", self.base_url, plan_id);

        let response = self
            .client
            .delete(&url)
            .header("Authorization", auth_header)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to delete meal plan: {}", response.status());
        }

        Ok(())
    }

    // Meal types
    pub async fn get_meal_type(&self, id: i32) -> Result<MealType> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/meal-type/{}/", self.base_url, id);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to get meal type {id}: {status} - {error_body}");
        }

        Ok(response.json().await?)
    }

    pub async fn get_meal_types(&self) -> Result<PaginatedResponse<MealType>> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/meal-type/", self.base_url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to get meal types: {}", response.status());
        }

        let meal_types = response.json().await?;
        Ok(meal_types)
    }

    // Cook log operations
    pub async fn get_cook_log(
        &self,
        recipe_id: Option<i32>,
        days_back: Option<i32>,
    ) -> Result<PaginatedResponse<CookLog>> {
        let auth_header = self.get_auth_header()?;
        let mut url = format!("{}/api/cook-log/", self.base_url);

        let mut params = vec![];
        if let Some(recipe) = recipe_id {
            params.push(format!("recipe={recipe}"));
        }
        if let Some(days) = days_back {
            // Calculate from_date based on days_back
            let from_date = chrono::Utc::now() - chrono::Duration::days(days as i64);
            params.push(format!("from_date={}", from_date.format("%Y-%m-%d")));
        }

        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }

        tracing::debug!(
            "Getting cook log for recipe_id: {:?}, days_back: {:?}",
            recipe_id,
            days_back
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error getting cook log: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::trace!("Cook log response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to get cook log with status {}: {}",
                status,
                error_body
            );
            anyhow::bail!("Failed to get cook log: {status} - {error_body}");
        }

        let cook_log: PaginatedResponse<CookLog> = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse cook log response: {}", e);
            anyhow::anyhow!("Invalid response format: {e}")
        })?;

        tracing::debug!("Successfully retrieved {} cook log entries", cook_log.count);
        Ok(cook_log)
    }

    pub async fn log_cooked_recipe(&self, request: CreateCookLogRequest) -> Result<CookLog> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/cook-log/", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to log cooked recipe: {}", response.status());
        }

        let cook_log = response.json().await?;
        Ok(cook_log)
    }

    // Utility operations
    /// Search keywords by name (Tandoor's fuzzy `query` filter; callers match exactly).
    pub async fn search_keywords(&self, query: &str) -> Result<PaginatedResponse<Keyword>> {
        let auth_header = self.get_auth_header()?;
        let url = format!(
            "{}/api/keyword/?query={}&page_size=25",
            self.base_url,
            urlencoding::encode(query)
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to search keywords: {status} - {error_body}");
        }
        let keywords = response.json().await?;
        Ok(keywords)
    }

    pub async fn get_keywords(&self) -> Result<PaginatedResponse<Keyword>> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/keyword/", self.base_url);

        tracing::debug!("Making request to get keywords: {}", url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error getting keywords: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::debug!("Keywords response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!(
                "Failed to get keywords with status {}: {}",
                status,
                error_body
            );

            match status.as_u16() {
                401 => anyhow::bail!("Authentication expired or invalid. Please re-authenticate."),
                403 => anyhow::bail!("Access denied to keywords endpoint. Check user permissions."),
                404 => anyhow::bail!(
                    "Keywords endpoint not found. Check Tandoor version and API availability."
                ),
                500..=599 => {
                    anyhow::bail!("Tandoor server error getting keywords ({status}): {error_body}")
                }
                _ => anyhow::bail!("Failed to get keywords with status {status}: {error_body}"),
            }
        }

        let keywords: PaginatedResponse<Keyword> = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse keywords response: {}", e);
            anyhow::anyhow!("Invalid response format from Tandoor server: {e}")
        })?;

        tracing::debug!("Successfully retrieved {} keywords", keywords.count);
        Ok(keywords)
    }

    pub async fn get_units(&self) -> Result<PaginatedResponse<Unit>> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/unit/", self.base_url);

        tracing::debug!("Making request to get units: {}", url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Network error getting units: {}", e);
                anyhow::anyhow!("Failed to connect to Tandoor API: {e}")
            })?;

        let status = response.status();
        tracing::debug!("Units response status: {}", status);

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read error response".to_string());
            tracing::error!("Failed to get units with status {}: {}", status, error_body);

            match status.as_u16() {
                401 => anyhow::bail!("Authentication expired or invalid. Please re-authenticate."),
                403 => anyhow::bail!("Access denied to units endpoint. Check user permissions."),
                404 => anyhow::bail!(
                    "Units endpoint not found. Check Tandoor version and API availability."
                ),
                500..=599 => {
                    anyhow::bail!("Tandoor server error getting units ({status}): {error_body}")
                }
                _ => anyhow::bail!("Failed to get units with status {status}: {error_body}"),
            }
        }

        let units: PaginatedResponse<Unit> = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse units response: {}", e);
            anyhow::anyhow!("Invalid response format from Tandoor server: {e}")
        })?;

        tracing::debug!("Successfully retrieved {} units", units.count);
        Ok(units)
    }

    pub async fn import_recipe_from_url(&self, url: &str) -> Result<RecipeFromSourceResponse> {
        let auth_header = self.get_auth_header()?;
        let api_url = format!("{}/api/recipe-from-source/", self.base_url);

        let body = serde_json::json!({ "url": url });
        let response = self
            .client
            .post(&api_url)
            .header("Authorization", auth_header)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let result: RecipeFromSourceResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Invalid response from recipe import endpoint: {e}"))?;
        Ok(result)
    }

    /// Patch a single recipe step. Only the fields in `body` change; passing `ingredients`
    /// replaces this step's ingredients without touching other steps.
    pub async fn update_step(&self, id: i32, body: serde_json::Value) -> Result<Step> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/step/{}/", self.base_url, id);

        let response = self
            .client
            .patch(&url)
            .header("Authorization", auth_header)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to update step {id}: {status} - {error_body}");
        }

        let step = response.json().await?;
        Ok(step)
    }

    pub async fn update_recipe(&self, id: i32, body: serde_json::Value) -> Result<Recipe> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe/{}/", self.base_url, id);

        let response = self
            .client
            .patch(&url)
            .header("Authorization", auth_header)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to update recipe {id}: {status} - {error_body}");
        }

        let recipe = response.json().await?;
        Ok(recipe)
    }

    pub async fn delete_recipe(&self, id: i32) -> Result<()> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe/{}/", self.base_url, id);

        let response = self
            .client
            .delete(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to delete recipe {}: {}", id, response.status());
        }
        Ok(())
    }

    pub async fn get_recipe_books(
        &self,
        query: Option<&str>,
    ) -> Result<PaginatedResponse<RecipeBook>> {
        let auth_header = self.get_auth_header()?;
        let mut url = format!("{}/api/recipe-book/", self.base_url);
        if let Some(q) = query {
            url.push_str(&format!("?query={}", urlencoding::encode(q)));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to get recipe books: {}", response.status());
        }
        let books = response.json().await?;
        Ok(books)
    }

    pub async fn create_recipe_book(&self, request: CreateRecipeBookRequest) -> Result<RecipeBook> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe-book/", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to create recipe book: {status} - {error_body}");
        }
        let book = response.json().await?;
        Ok(book)
    }

    pub async fn update_recipe_book(&self, id: i32, body: serde_json::Value) -> Result<RecipeBook> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe-book/{}/", self.base_url, id);

        let response = self
            .client
            .patch(&url)
            .header("Authorization", auth_header)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to update recipe book {id}: {status} - {error_body}");
        }
        let book = response.json().await?;
        Ok(book)
    }

    pub async fn delete_recipe_book(&self, id: i32) -> Result<()> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe-book/{}/", self.base_url, id);

        let response = self
            .client
            .delete(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to delete recipe book {}: {}", id, response.status());
        }
        Ok(())
    }

    pub async fn get_recipe_book_entries(
        &self,
        book_id: Option<i32>,
    ) -> Result<PaginatedResponse<RecipeBookEntry>> {
        let auth_header = self.get_auth_header()?;
        let mut url = format!("{}/api/recipe-book-entry/", self.base_url);
        if let Some(b) = book_id {
            url.push_str(&format!("?book={b}"));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to get recipe book entries: {}", response.status());
        }
        let entries = response.json().await?;
        Ok(entries)
    }

    pub async fn add_to_recipe_book(
        &self,
        request: CreateRecipeBookEntryRequest,
    ) -> Result<RecipeBookEntry> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe-book-entry/", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", auth_header)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to add recipe to book: {status} - {error_body}");
        }
        let entry = response.json().await?;
        Ok(entry)
    }

    pub async fn remove_from_recipe_book(&self, entry_id: i32) -> Result<()> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/recipe-book-entry/{}/", self.base_url, entry_id);

        let response = self
            .client
            .delete(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to remove recipe book entry {}: {}",
                entry_id,
                response.status()
            );
        }
        Ok(())
    }

    pub async fn get_supermarkets(&self) -> Result<PaginatedResponse<Supermarket>> {
        let auth_header = self.get_auth_header()?;
        let url = format!("{}/api/supermarket/", self.base_url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to get supermarkets: {}", response.status());
        }
        let supermarkets = response.json().await?;
        Ok(supermarkets)
    }

    pub async fn get_unit_conversions(
        &self,
        food_id: Option<i64>,
    ) -> Result<PaginatedResponse<UnitConversion>> {
        let auth_header = self.get_auth_header()?;
        let mut url = format!("{}/api/unit-conversion/", self.base_url);
        if let Some(f) = food_id {
            url.push_str(&format!("?food={f}"));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", auth_header)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Tandoor API: {e}"))?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to get unit conversions: {}", response.status());
        }
        let conversions = response.json().await?;
        Ok(conversions)
    }
}

/// File extension for an image, identified by its signature bytes.
pub fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [0x89, b'P', b'N', b'G', ..] => Some("png"),
        [b'G', b'I', b'F', b'8', ..] => Some("gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("webp"),
        _ => None,
    }
}

enum RecipeImageSource {
    Tandoor(Url),
    Public(Url),
}

fn recipe_image_source(base_url: &str, image: &str) -> Result<RecipeImageSource> {
    anyhow::ensure!(!image.trim().is_empty(), "Recipe image URL is empty");
    let mut base = tandoor_base_url(base_url)?;
    let source = match Url::parse(image) {
        Ok(url) => url,
        Err(_) => base
            .join(image)
            .context("Invalid Tandoor recipe image path")?,
    };
    if source.origin() != base.origin() {
        return Ok(RecipeImageSource::Public(source));
    }
    rebase_onto_tandoor_origin(&mut base, &source);
    Ok(RecipeImageSource::Tandoor(base))
}

fn redirected_recipe_image_source(
    base_url: &str,
    current: &Url,
    location: &str,
) -> Result<RecipeImageSource> {
    let source = current
        .join(location)
        .context("Invalid Tandoor recipe image redirect")?;
    let mut base = tandoor_base_url(base_url)?;
    if source.origin() != base.origin() {
        return Ok(RecipeImageSource::Public(source));
    }
    rebase_onto_tandoor_origin(&mut base, &source);
    Ok(RecipeImageSource::Tandoor(base))
}

fn tandoor_base_url(base_url: &str) -> Result<Url> {
    let mut base = Url::parse(base_url).context("Invalid Tandoor base URL")?;
    anyhow::ensure!(
        matches!(base.scheme(), "http" | "https"),
        "Tandoor base URL must use HTTP or HTTPS"
    );
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    Ok(base)
}

fn rebase_onto_tandoor_origin(base: &mut Url, source: &Url) {
    // Deliberately discard the authority supplied by Tandoor's response. It
    // can reflect an external reverse-proxy hostname while this client uses an
    // internal hostname, and credentials must only go to the configured host.
    base.set_path(source.path());
    base.set_query(source.query());
    base.set_fragment(None);
}

async fn public_recipe_image(url: &Url) -> Result<(Vec<u8>, &'static str)> {
    let (bytes, extension) = crate::image::download_public_image(url.as_str()).await?;
    anyhow::ensure!(
        bytes.len() <= MAX_EMBEDDED_IMAGE_BYTES,
        "Recipe image is larger than 10 MB"
    );
    let mime_type = match extension {
        "jpg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => anyhow::bail!("Tandoor recipe image has an unsupported file signature"),
    };
    Ok((bytes, mime_type))
}

#[cfg(test)]
mod recipe_image_tests {
    use super::*;
    use axum::{http::HeaderMap, response::Redirect, routing::get, Router};

    async fn stored_image(headers: HeaderMap) -> ([(&'static str, &'static str); 1], Vec<u8>) {
        assert_eq!(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-token")
        );
        (
            [("content-type", "image/png")],
            b"\x89PNG\r\n\x1a\nstored-recipe-image".to_vec(),
        )
    }

    async fn redirected_image(headers: HeaderMap) -> Redirect {
        assert_eq!(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-token")
        );
        Redirect::temporary("/media/recipes/porridge.png")
    }

    #[test]
    fn recipe_images_are_pinned_to_the_configured_tandoor_origin() {
        let RecipeImageSource::Tandoor(relative) =
            recipe_image_source("http://tandoor:8080", "/media/recipes/porridge.jpg").unwrap()
        else {
            panic!("relative media path must use Tandoor");
        };
        assert_eq!(
            relative.as_str(),
            "http://tandoor:8080/media/recipes/porridge.jpg"
        );

        let RecipeImageSource::Public(external) = recipe_image_source(
            "http://tandoor:8080",
            "https://public.example/media/recipes/porridge.jpg?size=large",
        )
        .unwrap() else {
            panic!("external image must use the public downloader");
        };
        assert_eq!(
            external.as_str(),
            "https://public.example/media/recipes/porridge.jpg?size=large"
        );

        let signed_s3_url = "https://recipe-images.s3.eu-central-1.amazonaws.com/recipes/porridge.jpg?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=example%2F20261021%2Feu-central-1%2Fs3%2Faws4_request&X-Amz-Signature=abc123";
        let RecipeImageSource::Public(signed_s3) =
            recipe_image_source("https://recipes.example", signed_s3_url).unwrap()
        else {
            panic!("presigned S3 URL must use the public downloader");
        };
        assert_eq!(signed_s3.as_str(), signed_s3_url);

        let current = Url::parse("http://tandoor:8080/media/original.jpg").unwrap();
        let RecipeImageSource::Public(redirected) = redirected_recipe_image_source(
            "http://tandoor:8080",
            &current,
            "https://cdn.example/porridge.jpg",
        )
        .unwrap() else {
            panic!("external redirect must use the public downloader");
        };
        assert_eq!(redirected.as_str(), "https://cdn.example/porridge.jpg");
    }

    #[test]
    fn recipe_links_use_the_configured_tandoor_url() {
        let client = TandoorClient::new("https://recipes.thehealthwarriors.net/".to_string());
        assert_eq!(
            client.recipe_url(1281),
            "https://recipes.thehealthwarriors.net/recipe/1281"
        );
    }

    #[tokio::test]
    async fn stored_recipe_image_is_fetched_with_authentication() {
        let app = Router::new()
            .route("/redirected-image", get(redirected_image))
            .route("/media/recipes/porridge.png", get(stored_image));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut client = TandoorClient::new(format!("http://{address}"));
        client.set_token("test-token".to_string());
        let (bytes, mime_type) = client
            .get_recipe_image("/media/recipes/porridge.png")
            .await
            .unwrap();

        assert_eq!(mime_type, "image/png");
        assert_eq!(bytes, b"\x89PNG\r\n\x1a\nstored-recipe-image");

        let (redirected_bytes, redirected_mime_type) =
            client.get_recipe_image("/redirected-image").await.unwrap();
        assert_eq!(redirected_mime_type, "image/png");
        assert_eq!(redirected_bytes, b"\x89PNG\r\n\x1a\nstored-recipe-image");
        server.abort();
    }
}
