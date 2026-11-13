//! OTLP/HTTP ingest (spec 02 § Endpoints, D-G-2 to D-G-9).
#![cfg_attr(not(test), allow(dead_code))] // no non-test caller until the route lands (G1.7)

pub(crate) mod body;
