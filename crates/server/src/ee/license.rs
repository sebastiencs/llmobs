// SPDX-License-Identifier: Elastic-2.0
//! Licence keys (D172, D-P-7): claims, the status over time, and the features they grant.

#![cfg_attr(not(test), allow(dead_code))] // P7 uses these outside tests.

use serde::Deserialize;
use uuid::Uuid;

use crate::ee::token::{self, TokenError};
use crate::entitlements::FeatureSet;

/// The payload of a `license` token, after [`Kind`] matched. Unknown fields are ignored (D-P-5).
#[derive(Debug, Deserialize)]
struct Claims {
    id: Uuid,
    #[serde(with = "llmobs_core::time_serde")]
    issued_at: i64,
    customer: String,
    features: Vec<String>,
    #[serde(with = "llmobs_core::time_serde")]
    expires_at: i64,
    #[serde(with = "llmobs_core::time_serde")]
    grace_ends_at: i64,
}

/// A licence whose signature and claims are valid. Its dates may be past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct License {
    /// Payload `id`.
    pub(crate) id: Uuid,
    /// The key that signed it.
    pub(crate) kid: String,
    /// Display name of the customer. Shown to instance admins only; never logged.
    pub(crate) customer: String,
    /// The known features it lists.
    pub(crate) features: FeatureSet,
    /// ns.
    pub(crate) issued_at: i64,
    /// ns. Active before this instant.
    pub(crate) expires_at: i64,
    /// ns. In grace before this instant, expired from it on.
    pub(crate) grace_ends_at: i64,
}

/// Why a configured key was refused. Shown to instance admins and logged; never the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InvalidLicense {
    /// The token itself was refused.
    #[error(transparent)]
    Token(#[from] TokenError),
    /// The payload is not a licence JSON object with every required field.
    #[error("the licence payload is invalid")]
    Payload,
    /// The token is not of kind `license`.
    #[error("the token is not a licence key")]
    WrongKind,
    /// `grace_ends_at` is before `expires_at`, or `customer` is empty or over 200 characters.
    #[error("the licence fields are inconsistent")]
    Fields,
}

/// The configured licence, read once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LicenseState {
    /// `license_key` is unset.
    Absent,
    /// A key is set and was refused.
    Invalid(InvalidLicense),
    /// A genuine licence.
    Valid(License),
}

/// Where a licence stands at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LicenseStatus {
    /// No key.
    Absent,
    /// The key was refused.
    Invalid,
    /// Before `expires_at`.
    Active,
    /// From `expires_at` to before `grace_ends_at`: features on, warnings shown.
    Grace,
    /// From `grace_ends_at` on: features off.
    Expired,
}

impl LicenseState {
    /// Verifies and parses `key`. `None` is [`Self::Absent`].
    pub(crate) fn from_key(key: Option<&str>) -> Self {
        match key {
            None => Self::Absent,
            Some(key) => match parse(key) {
                Ok(license) => Self::Valid(license),
                Err(reason) => Self::Invalid(reason),
            },
        }
    }

    /// The status at `now_ns`.
    pub(crate) fn status(&self, now_ns: i64) -> LicenseStatus {
        match self {
            Self::Absent => LicenseStatus::Absent,
            Self::Invalid(_) => LicenseStatus::Invalid,
            Self::Valid(license) if now_ns < license.expires_at => LicenseStatus::Active,
            Self::Valid(license) if now_ns < license.grace_ends_at => LicenseStatus::Grace,
            Self::Valid(_) => LicenseStatus::Expired,
        }
    }

    /// Features in force at `now_ns`: the licence's while active or in grace, else none.
    pub(crate) fn active_features(&self, now_ns: i64) -> FeatureSet {
        match (self, self.status(now_ns)) {
            (Self::Valid(license), LicenseStatus::Active | LicenseStatus::Grace) => {
                license.features
            }
            _ => FeatureSet::EMPTY,
        }
    }

    /// Features a genuine licence lists, expired or not (D-P-8: restrictions keep applying).
    pub(crate) fn listed_features(&self) -> FeatureSet {
        match self {
            Self::Valid(license) => license.features,
            Self::Absent | Self::Invalid(_) => FeatureSet::EMPTY,
        }
    }
}

/// The `kind` field alone, read before the full claims.
#[derive(Debug, Deserialize)]
struct Kind {
    kind: String,
}

fn parse(key: &str) -> Result<License, InvalidLicense> {
    let verified = token::verify(key)?;
    let kind: Kind =
        serde_json::from_slice(&verified.payload).map_err(|_| InvalidLicense::Payload)?;
    if kind.kind != "license" {
        return Err(InvalidLicense::WrongKind);
    }
    let claims: Claims =
        serde_json::from_slice(&verified.payload).map_err(|_| InvalidLicense::Payload)?;
    let customer = claims.customer.trim();
    if claims.grace_ends_at < claims.expires_at
        || customer.is_empty()
        || customer.chars().count() > 200
    {
        return Err(InvalidLicense::Fields);
    }
    Ok(License {
        id: claims.id,
        kid: verified.kid,
        customer: customer.to_owned(),
        features: FeatureSet::from_names(claims.features.iter().map(String::as_str)),
        issued_at: claims.issued_at,
        expires_at: claims.expires_at,
        grace_ends_at: claims.grace_ends_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sign_token;
    use serde_json::{Value, json};

    const ACTIVE_TOKEN: &str = include_str!("../../../../fixtures/license/tokens/active.token");
    const FUTURE_FEATURE_TOKEN: &str =
        include_str!("../../../../fixtures/license/tokens/future-feature.token");
    const WRONG_KIND_TOKEN: &str =
        include_str!("../../../../fixtures/license/tokens/wrong-kind.token");

    fn ns(text: &str) -> i64 {
        llmobs_core::time_serde::parse_rfc3339(text).unwrap()
    }

    fn claims(expires_at: &str, grace_ends_at: &str) -> Value {
        json!({
            "kind": "license",
            "id": "01900000-0000-7000-8000-000000000600",
            "issued_at": "2026-01-01T00:00:00Z",
            "customer": "Example Corp",
            "features": ["project_roles", "sso_enforcement"],
            "expires_at": expires_at,
            "grace_ends_at": grace_ends_at,
        })
    }

    fn refused(key: &str) -> InvalidLicense {
        match LicenseState::from_key(Some(key)) {
            LicenseState::Invalid(reason) => reason,
            other => panic!("expected a refused key, got {other:?}"),
        }
    }

    #[test]
    fn absent_and_status_over_time() {
        let absent = LicenseState::from_key(None);
        assert_eq!(absent, LicenseState::Absent);
        assert_eq!(absent.status(0), LicenseStatus::Absent);
        assert_eq!(absent.active_features(0), FeatureSet::EMPTY);
        assert_eq!(absent.listed_features(), FeatureSet::EMPTY);

        let token = sign_token(&claims("2026-02-01T00:00:00Z", "2026-03-01T00:00:00Z"));
        let state = LicenseState::from_key(Some(token.as_str()));
        let both = FeatureSet::from_names(["project_roles", "sso_enforcement"]);

        assert_eq!(
            state.status(ns("2026-01-15T00:00:00Z")),
            LicenseStatus::Active
        );
        assert_eq!(
            state.status(ns("2026-02-01T00:00:00Z")),
            LicenseStatus::Grace
        );
        assert_eq!(
            state.status(ns("2026-02-15T00:00:00Z")),
            LicenseStatus::Grace
        );
        assert_eq!(
            state.status(ns("2026-03-01T00:00:00Z")),
            LicenseStatus::Expired
        );

        for text in [
            "2026-01-15T00:00:00Z",
            "2026-02-01T00:00:00Z",
            "2026-02-15T00:00:00Z",
        ] {
            assert_eq!(state.active_features(ns(text)), both, "{text}");
            assert_eq!(state.listed_features(), both, "{text}");
        }
        let expired = ns("2026-03-01T00:00:00Z");
        assert_eq!(state.active_features(expired), FeatureSet::EMPTY);
        assert_eq!(state.listed_features(), both);
    }

    #[test]
    fn fixture_licences() {
        match LicenseState::from_key(Some(ACTIVE_TOKEN)) {
            LicenseState::Valid(license) => {
                assert_eq!(license.customer, "Example Corp");
                assert_eq!(
                    license.id,
                    Uuid::parse_str("01900000-0000-7000-8000-000000000500").unwrap()
                );
                assert_eq!(license.kid, "dev-1");
                assert_eq!(
                    license.features,
                    FeatureSet::from_names(["project_roles", "sso_enforcement"])
                );
            }
            other => panic!("expected a valid licence, got {other:?}"),
        }

        match LicenseState::from_key(Some(FUTURE_FEATURE_TOKEN)) {
            LicenseState::Valid(license) => {
                assert_eq!(license.features, FeatureSet::from_names(["project_roles"]));
            }
            other => panic!("expected a valid licence, got {other:?}"),
        }

        assert_eq!(
            LicenseState::from_key(Some(WRONG_KIND_TOKEN)),
            LicenseState::Invalid(InvalidLicense::WrongKind)
        );
    }

    #[test]
    fn invalid_keys() {
        assert_eq!(
            refused("garbage"),
            InvalidLicense::Token(TokenError::Malformed)
        );
        assert_eq!(
            refused(&sign_token(&json!({"kind": "license"}))),
            InvalidLicense::Payload
        );

        let swapped = claims("2026-03-01T00:00:00Z", "2026-02-01T00:00:00Z");
        assert_eq!(refused(&sign_token(&swapped)), InvalidLicense::Fields);

        let mut blank = claims("2026-02-01T00:00:00Z", "2026-03-01T00:00:00Z");
        blank["customer"] = json!("  ");
        assert_eq!(refused(&sign_token(&blank)), InvalidLicense::Fields);

        let mut long = claims("2026-02-01T00:00:00Z", "2026-03-01T00:00:00Z");
        long["customer"] = json!("x".repeat(201));
        assert_eq!(refused(&sign_token(&long)), InvalidLicense::Fields);

        let mut bad_time = claims("2026-02-01T00:00:00Z", "2026-03-01T00:00:00Z");
        bad_time["expires_at"] = json!("next tuesday");
        assert_eq!(refused(&sign_token(&bad_time)), InvalidLicense::Payload);
    }

    #[test]
    fn invalid_text_is_generic() {
        let reasons = [
            InvalidLicense::Token(TokenError::Malformed),
            InvalidLicense::Token(TokenError::TooLarge),
            InvalidLicense::Token(TokenError::UnknownKey),
            InvalidLicense::Token(TokenError::BadSignature),
            InvalidLicense::Payload,
            InvalidLicense::WrongKind,
            InvalidLicense::Fields,
        ];
        for reason in reasons {
            let text = reason.to_string();
            assert!(!text.contains("llmobs1."), "{text}");
            assert!(!text.contains("Example"), "{text}");
        }
    }
}
