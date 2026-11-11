//! HTTP API types, server configuration, and the OpenAPI document for llmobs.
//!
//! [`config`] loads `ServerConfig` from defaults, `llmobs.toml`, and `LLMOBS_*`
//! (spec 12). [`api`] holds the request and response types for the observability
//! read API (`docs/spec/09-api.md`). [`api::openapi::openapi`] is the contract
//! written to `crates/server/openapi.json`. The crate does not serve HTTP yet.

pub mod api;
pub mod config;
