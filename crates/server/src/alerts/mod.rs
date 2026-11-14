//! Alerting: conditions, channels, evaluation and delivery (spec 07, track K).
#![cfg_attr(not(test), allow(dead_code))] // the routes use this from K5 on, the loops from K9 on

pub(crate) mod channel;
pub(crate) mod condition;
pub(crate) mod quota;
