//! HTTP handlers. [`crate::http::access::routes`] is the access table.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod admin;
pub(crate) mod api_keys;
pub(crate) mod auth;
pub(crate) mod connections;
pub(crate) mod evaluators;
pub(crate) mod feedback;
pub(crate) mod invites;
pub(crate) mod me;
pub(crate) mod media;
pub(crate) mod members;
pub(crate) mod metrics;
pub(crate) mod oauth;
pub(crate) mod orgs;
pub(crate) mod otlp;
pub(crate) mod projects;
pub(crate) mod read;
pub(crate) mod saved_views;
pub(crate) mod schema;
pub(crate) mod score_definitions;
pub(crate) mod scores;
pub(crate) mod sessions;
pub(crate) mod sql;
pub(crate) mod streams;
pub(crate) mod tokens;
pub(crate) mod traces;
pub(crate) mod users;
