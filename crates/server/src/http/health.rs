//! `/healthz` and `/readyz` (spec 09, spec 12).
//!
//! spec 12 also lists object-store reachability and free disk. Those checks
//! arrive with the tracks that own them.
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::AppState;
use super::error;
use crate::api::common::{Problem, ProblemCode};

#[derive(Serialize)]
struct Probe {
    status: &'static str,
}

/// Process is alive.
pub(crate) async fn healthz() -> Response {
    axum::Json(Probe { status: "ok" }).into_response()
}

/// `meta.db` answers and neither WAL has failed.
pub(crate) async fn readyz(State(state): State<AppState>) -> Response {
    let mut failed = Vec::new();
    if state.meta.ping().await.is_err() {
        failed.push("meta.db");
    }
    let health = state.storage.health();
    if health.spans_wal_failed {
        failed.push("spans WAL");
    }
    if health.scores_wal_failed {
        failed.push("scores WAL");
    }
    if failed.is_empty() {
        axum::Json(Probe { status: "ready" }).into_response()
    } else {
        let problem = Problem::new(ProblemCode::Busy, Some(failed.join(", ")));
        error::problem_response(&problem)
    }
}
