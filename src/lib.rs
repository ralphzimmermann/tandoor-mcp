#![allow(clippy::uninlined_format_args)]
// The existing Tandoor client predates Rust's captured-format syntax. Keep this
// upgrade focused on MCP behavior; migrate those diagnostics separately.

//! # Tandoor MCP Library
//!
//! This library provides tools for integrating with Tandoor (a recipe management system)
//! through the Model Context Protocol (MCP). It consists of two main components:
//!
//! ## Client Module
//!
//! The [`client`] module provides a direct HTTP client for the Tandoor API, handling
//! authentication, recipe management, shopping lists, and more.
//!
//! ## Server Module
//!
//! The [`server`] module implements an MCP server that exposes Tandoor functionality
//! as standardized tools that AI assistants can use. The [`http`] module mounts it
//! as a Streamable HTTP service at `/mcp`, with MCP 2026-07-28 discovery support.
//!
//! ## Quick Start
//!
//! ```no_run
//! use mcp_tandoor::{TandoorClient, TandoorMcpServer};
//!
//! // Use the client directly
//! let mut client = TandoorClient::new("http://localhost:8080".to_string());
//!
//! // Or create an MCP server
//! let server = TandoorMcpServer::new_with_credentials(
//!     "http://localhost:8080".to_string(),
//!     "username".to_string(),
//!     "password".to_string()
//! );
//! ```

pub mod client;
pub mod config;
mod domain;
pub mod http;
mod image;
pub mod server;
pub mod storage;
pub mod telemetry;
pub mod thumbnail;
pub mod thumbnail_service;

pub use client::TandoorClient;
pub use http::mcp_router;
pub use server::TandoorMcpServer;
