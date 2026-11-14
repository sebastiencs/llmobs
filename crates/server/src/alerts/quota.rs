//! The per-org alert rule limit (D178, D-K-14).

use llmobs_core::ids::OrgId;

use crate::http::AppState;
use crate::http::error::ApiError;

/// The most enabled alert rules `org_id` may have; `None` means no limit.
///
/// Track P's M4 (D179) replaces this body with
/// `entitlements::of(state, org_id).limits.max_alert_rules`. Until then every org is unlimited,
/// which is the self-hosted behaviour (D172).
pub(crate) fn rule_limit(_state: &AppState, _org_id: OrgId) -> Option<u64> {
    None
}

/// `Err(ApiError::PlanLimit)` when one more enabled rule would exceed `limit`.
pub(crate) fn check_rule_quota(enabled_rules: i64, limit: Option<u64>) -> Result<(), ApiError> {
    // A count is never negative; treat a corrupt negative count as 0.
    let enabled_rules = u64::try_from(enabled_rules).unwrap_or(0);
    match limit {
        Some(limit) if enabled_rules >= limit => Err(ApiError::PlanLimit {
            detail: format!(
                "Your plan allows {limit} enabled alert {}.",
                if limit == 1 { "rule" } else { "rules" }
            ),
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::common::ProblemCode;

    #[test]
    fn no_limit_allows_everything() {
        assert!(check_rule_quota(5, None).is_ok());
    }

    #[test]
    fn limit_one() {
        assert!(check_rule_quota(0, Some(1)).is_ok());
        let Err(ApiError::PlanLimit { detail }) = check_rule_quota(1, Some(1)) else {
            panic!("one enabled rule reaches a limit of one");
        };
        assert_eq!(detail, "Your plan allows 1 enabled alert rule.");
        let Err(ApiError::PlanLimit { detail }) = check_rule_quota(3, Some(3)) else {
            panic!("three enabled rules reach a limit of three");
        };
        assert_eq!(detail, "Your plan allows 3 enabled alert rules.");
    }

    #[test]
    fn plan_limit_problem() {
        let problem = ApiError::PlanLimit {
            detail: "x".to_owned(),
        }
        .problem();
        assert_eq!(problem.status, StatusCode::FORBIDDEN.as_u16());
        assert_eq!(problem.code, ProblemCode::PlanLimit);
        assert_eq!(problem.detail.as_deref(), Some("x"));
        let json = serde_json::to_value(&problem).unwrap();
        assert_eq!(json["code"], "plan_limit");
    }
}
