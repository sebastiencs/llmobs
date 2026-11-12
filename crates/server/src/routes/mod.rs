//! HTTP handlers. [`crate::http::access::routes`] is the access table.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod admin;
pub(crate) mod api_keys;
pub(crate) mod auth;
pub(crate) mod invites;
pub(crate) mod me;
pub(crate) mod members;
pub(crate) mod orgs;
pub(crate) mod projects;
pub(crate) mod tokens;
