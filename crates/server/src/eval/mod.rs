//! Online evaluation in the server: connections now; test runs, the queue and triggers later (track I).
#![cfg_attr(not(test), allow(dead_code))] // the connection test route is the first non-test caller (I1.6)

pub(crate) mod connection;
