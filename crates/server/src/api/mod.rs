//! Request and response types for the HTTP API.
//!
//! The rules live in `docs/spec/09-api.md` § Conventions and § Response shapes.
//! The generated contract file is `crates/server/openapi.json`.

pub mod account;
pub mod admin;
pub mod common;
pub mod connections;
pub(crate) mod convert;
pub mod entitlements;
pub mod evaluators;
pub mod invites;
pub mod keys;
pub mod members;
pub mod metrics;
pub mod openapi;
pub mod payload;
pub mod saved_views;
pub mod schema;
pub mod score_definitions;
pub mod scores;
pub mod sessions;
pub mod sql;
pub mod traces;
pub mod users;
