//! Request and response types for the HTTP API.
//!
//! The rules live in `docs/spec/09-api.md` § Conventions and § Response shapes.
//! The generated contract file is `crates/server/openapi.json`.

pub mod account;
pub mod common;
pub mod invites;
pub mod keys;
pub mod members;
pub mod metrics;
pub mod openapi;
pub mod payload;
pub mod saved_views;
pub mod schema;
pub mod scores;
pub mod sessions;
pub mod traces;
pub mod users;
