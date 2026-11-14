//! One resolved `Entitlements` value per org (D179, D-P-10).
//!
//! M1 has one layer: the instance licence's features, while it is active or in grace.
//! M2 adds the instance defaults and the org's token.

#![cfg_attr(not(test), allow(dead_code))] // P7 uses these outside tests.

use llmobs_core::ids::OrgId;

use crate::ee::license::{LicenseState, LicenseStatus};
use crate::http::AppState;
use crate::http::error::ApiError;

pub(crate) use crate::api::entitlements::Feature;

/// A set of [`Feature`]s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FeatureSet(u32);

impl FeatureSet {
    /// No feature.
    pub(crate) const EMPTY: Self = Self(0);

    fn bit(feature: Feature) -> u32 {
        1 << (feature as u32)
    }

    /// Adds `feature`.
    pub(crate) fn insert(&mut self, feature: Feature) {
        self.0 |= Self::bit(feature);
    }

    /// Whether `feature` is in the set.
    pub(crate) fn contains(self, feature: Feature) -> bool {
        self.0 & Self::bit(feature) != 0
    }

    /// The known features named in `names`; unknown names are skipped (D-P-5).
    pub(crate) fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut set = Self::EMPTY;
        for feature in names.into_iter().filter_map(Feature::from_name) {
            set.insert(feature);
        }
        set
    }

    /// The features in the set, in [`Feature::ALL`] order.
    pub(crate) fn to_vec(self) -> Vec<Feature> {
        Feature::ALL
            .into_iter()
            .filter(|feature| self.contains(*feature))
            .collect()
    }
}

/// What one org may do now (D179).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entitlements {
    features: FeatureSet,
}

impl Entitlements {
    /// Entitlements with exactly `features`.
    pub(crate) fn with_features(features: FeatureSet) -> Self {
        Self { features }
    }

    /// Whether the org may use `feature`.
    pub(crate) fn allows(self, feature: Feature) -> bool {
        self.features.contains(feature)
    }

    /// Every allowed feature, in [`Feature::ALL`] order.
    pub(crate) fn features(self) -> Vec<Feature> {
        self.features.to_vec()
    }
}

/// Entitlement sources held by the process (D-P-10).
#[derive(Debug)]
pub(crate) struct EntitlementsState {
    license: LicenseState,
}

impl EntitlementsState {
    /// From the licence read at startup.
    pub(crate) fn new(license: LicenseState) -> Self {
        Self { license }
    }

    /// The instance licence.
    pub(crate) fn license(&self) -> &LicenseState {
        &self.license
    }
}

/// What `org` may do now. Synchronous and in memory; call it at every check (D-P-10).
///
/// M1: the licence's features while it is active or in grace, for every org. `_org`
/// is used from M2 on, when the org's own token joins the resolution.
pub(crate) fn of(state: &AppState, _org: OrgId) -> Entitlements {
    Entitlements::with_features(
        state
            .entitlements
            .license()
            .active_features(state.clock.now_ns()),
    )
}

/// Ok when `org` may use `feature`; otherwise the `plan_required` error that names it (D-P-14).
pub(crate) fn require(state: &AppState, org: OrgId, feature: Feature) -> Result<(), ApiError> {
    if of(state, org).allows(feature) {
        Ok(())
    } else {
        Err(ApiError::PlanRequired {
            detail: feature.locked_detail(),
        })
    }
}

/// `entitlements.upgrade_url` for `org`, or `None` when unset (D-P-15).
pub(crate) fn upgrade_url(state: &AppState, org: OrgId) -> Option<String> {
    state
        .config
        .entitlements
        .upgrade_url
        .as_deref()
        .map(|template| template.replace("{org}", &org.to_string()))
}

/// Logs the licence status once at startup: status, licence id and key id only (D-P-7).
pub(crate) fn log_license(license: &LicenseState, now_ns: i64) {
    match (license, license.status(now_ns)) {
        (LicenseState::Absent, _) => {
            tracing::info!("no licence key: enterprise features are locked");
        }
        (LicenseState::Invalid(reason), _) => tracing::error!(
            reason = %reason,
            "the licence key was refused: running without a licence"
        ),
        (LicenseState::Valid(license), LicenseStatus::Active) => tracing::info!(
            license_id = %license.id,
            kid = %license.kid,
            "licence active"
        ),
        (LicenseState::Valid(license), LicenseStatus::Grace) => tracing::warn!(
            license_id = %license.id,
            kid = %license.kid,
            "licence expired: enterprise features stop at the end of the grace period"
        ),
        (LicenseState::Valid(license), _) => tracing::warn!(
            license_id = %license.id,
            kid = %license.kid,
            "licence expired: enterprise features are locked"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Secret;
    use crate::ee::license::InvalidLicense;
    use crate::ee::token::TokenError;
    use crate::test_support::{TestApp, capture_logs, license_key, license_key_until};

    #[test]
    fn feature_names_round_trip() {
        for feature in Feature::ALL {
            assert_eq!(Feature::from_name(feature.name()), Some(feature));
        }
        assert_eq!(
            serde_json::to_string(&Feature::ProjectRoles).unwrap(),
            "\"project_roles\""
        );
        assert_eq!(Feature::from_name("audit_logs"), None);
        assert!(Feature::ALL.len() <= 32);
    }

    #[test]
    fn feature_set_ignores_unknown_names() {
        let set = FeatureSet::from_names(["project_roles", "audit_logs", "project_roles"]);
        assert_eq!(set.to_vec(), vec![Feature::ProjectRoles]);
    }

    #[test]
    fn entitlements_allow_listed_features() {
        let entitlements = Entitlements::with_features(FeatureSet::from_names([
            "sso_enforcement",
            "project_roles",
        ]));
        assert!(entitlements.allows(Feature::ProjectRoles));
        assert!(entitlements.allows(Feature::SsoEnforcement));
        assert_eq!(
            entitlements.features(),
            vec![Feature::ProjectRoles, Feature::SsoEnforcement]
        );
        assert!(!Entitlements::with_features(FeatureSet::EMPTY).allows(Feature::ProjectRoles));
    }

    #[tokio::test]
    async fn no_license_allows_nothing() {
        let app = TestApp::new().await;
        assert!(of(&app.state, OrgId::new_v7()).features().is_empty());
    }

    #[tokio::test]
    async fn license_features_apply_to_every_org() {
        let app = TestApp::with_license(&["project_roles"]).await;
        for org in [OrgId::new_v7(), OrgId::new_v7()] {
            let entitlements = of(&app.state, org);
            assert!(entitlements.allows(Feature::ProjectRoles));
            assert!(!entitlements.allows(Feature::SsoEnforcement));
        }
    }

    #[tokio::test]
    async fn expired_license_allows_nothing() {
        let key = license_key_until(
            &["project_roles"],
            "2025-12-01T00:00:00Z",
            "2025-12-31T00:00:00Z",
        );
        let app = TestApp::with_config(move |config| {
            config.license_key = Some(Secret::new(key));
        })
        .await;
        assert!(of(&app.state, OrgId::new_v7()).features().is_empty());
        assert!(
            app.state
                .entitlements
                .license()
                .listed_features()
                .contains(Feature::ProjectRoles)
        );
    }

    #[tokio::test]
    async fn refused_key_runs_without_license() {
        let app = TestApp::with_config(|config| {
            config.license_key = Some(Secret::new("garbage".to_owned()));
        })
        .await;
        assert!(matches!(
            app.state.entitlements.license(),
            LicenseState::Invalid(InvalidLicense::Token(TokenError::Malformed))
        ));
        assert!(of(&app.state, OrgId::new_v7()).features().is_empty());
    }

    #[tokio::test]
    async fn require_maps_to_plan_required() {
        let app = TestApp::new().await;
        let org = OrgId::new_v7();
        assert!(matches!(
            require(&app.state, org, Feature::ProjectRoles),
            Err(ApiError::PlanRequired { detail }) if detail == Feature::ProjectRoles.locked_detail()
        ));

        let app = TestApp::with_license(&["project_roles"]).await;
        assert!(require(&app.state, org, Feature::ProjectRoles).is_ok());
        assert!(require(&app.state, org, Feature::SsoEnforcement).is_err());
    }

    #[tokio::test]
    async fn log_license_never_prints_the_customer() {
        let app = TestApp::new().await;
        let now = app.state.clock.now_ns();
        let valid = LicenseState::from_key(Some(&license_key(&["project_roles"])));
        let (_, logs) = capture_logs(async { log_license(&valid, now) }).await;
        assert!(logs.contains("licence active"), "{logs}");
        assert!(
            logs.contains("01900000-0000-7000-8000-000000000510"),
            "{logs}"
        );
        assert!(!logs.contains("Test Customer"), "{logs}");
        assert!(!logs.contains("llmobs1."), "{logs}");

        let refused = LicenseState::from_key(Some("garbage"));
        let (_, logs) = capture_logs(async { log_license(&refused, now) }).await;
        assert!(logs.contains("refused"), "{logs}");
        assert!(!logs.contains("garbage"), "{logs}");
    }
}
