//! One resolved `Entitlements` value per org (D179, D-P-10).
//!
//! M1 has one layer: the instance licence's features, while it is active or in grace.
//! M2 adds the instance defaults and the org's token.

#![cfg_attr(not(test), allow(dead_code))] // P5 wires the licence into AppState.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
