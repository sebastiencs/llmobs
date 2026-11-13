//! HTTP API types, server configuration, metadata DB, and the OpenAPI document.
//!
//! [`config`] loads `ServerConfig` from defaults, `llmobs.toml`, and `LLMOBS_*`
//! (spec 12). [`meta`] opens `meta.db` (spec 04). [`api`] holds the request and
//! response types for the observability read API (`docs/spec/09-api.md`).
//! [`api::openapi::openapi`] is the contract written to `crates/server/openapi.json`.
//! `http` is the axum router. [`serve`] binds it for `llmobs serve`.

pub mod admin;
pub mod api;
pub(crate) mod auth;
pub(crate) mod bootstrap;
pub(crate) mod clock;
pub mod config;
pub(crate) mod email;
pub(crate) mod http;
pub(crate) mod ingest;
pub(crate) mod meta;
pub(crate) mod routes;
pub mod serve;

#[cfg(test)]
mod api_examples;
#[cfg(test)]
pub(crate) mod test_support;
