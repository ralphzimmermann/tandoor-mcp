//! # Tandoor HTTP Client
//!
//! This module provides a direct HTTP client for the Tandoor API, handling API tokens,
//! recipe management, shopping lists, and other Tandoor functionality.
//!
//! ## Modules
//!
//! - [`auth`] - Authentication handling for Tandoor OAuth2 tokens
//! - [`client`] - Main HTTP client implementation with all API methods
//! - [`types`] - Type definitions for API requests and responses
//!
//! ## Quick Start
//!
//! ```no_run
//! use mcp_tandoor::client::TandoorClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let mut client = TandoorClient::new("http://localhost:8080".to_string());
//!
//! // Create a token in Tandoor under Settings → API.
//! client.set_token("tda_your_api_token".to_string());
//!
//! // Search for recipes
//! let recipes = client
//!     .search_recipes(Some("pasta"), Some(10), None, None, None, None, None)
//!     .await?;
//! println!("Found {} recipes", recipes.count);
//! # Ok(())
//! # }
//! ```

pub mod auth;
#[allow(clippy::module_inception)]
pub mod client;
pub mod types;

pub use client::TandoorClient;
pub use types::*;
