// SPDX-License-Identifier: Elastic-2.0
//! Instance-wide SSO enforcement, `auth.enforce_sso_domains` (spec 08, D-P-4, D-P-8).
//!
//! It applies when the configured key is a genuine licence that lists `sso_enforcement`,
//! expired or not: a lapse never re-opens password login for those domains.

use crate::entitlements::Feature;
use crate::http::AppState;

/// Whether `email` (already normalized) must sign in through SSO.
pub(crate) fn requires_sso(state: &AppState, email: &str) -> bool {
    applies(state)
        && crate::routes::auth::domain_listed(&state.config.auth.enforce_sso_domains, email)
}

/// Whether the configured domains are enforced at all.
pub(crate) fn applies(state: &AppState) -> bool {
    state
        .entitlements
        .license()
        .listed_features()
        .contains(Feature::SsoEnforcement)
}

/// Warns once at startup when domains are configured but not enforced. No domain is logged.
pub(crate) fn warn_if_unlicensed(state: &AppState) {
    if !state.config.auth.enforce_sso_domains.is_empty() && !applies(state) {
        tracing::warn!(
            domains = state.config.auth.enforce_sso_domains.len(),
            "auth.enforce_sso_domains is set but not enforced: it needs an enterprise licence"
        );
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;

    use crate::api::common::ProblemCode;
    use crate::bootstrap;
    use crate::test_support::{TestApp, capture_logs, license_key, license_key_until};

    const PASSWORD: &str = "llmobs-ok1";

    fn login_body(email: &str) -> serde_json::Value {
        serde_json::json!({ "email": email, "password": PASSWORD })
    }

    fn sso_config(config: &mut crate::config::ServerConfig) {
        config.auth.enforce_sso_domains = vec!["sso.example".to_owned()];
    }

    #[tokio::test]
    async fn domains_without_a_license_are_not_enforced() {
        let app = TestApp::with_config(sso_config).await;
        let response = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("x@sso.example"))
            .send()
            .await;
        assert_eq!(response.status, 401);
        assert_ne!(response.problem().code, ProblemCode::SsoRequired);
        assert!(!super::applies(&app.state));
    }

    #[tokio::test]
    async fn expired_license_keeps_enforcing() {
        let key = license_key_until(
            &["sso_enforcement"],
            "2025-12-01T00:00:00Z",
            "2025-12-31T00:00:00Z",
        );
        let app = TestApp::with_config(move |config| {
            sso_config(config);
            config.license_key = Some(crate::config::Secret::new(key));
        })
        .await;
        let response = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("x@sso.example"))
            .send()
            .await;
        assert_eq!(response.status, 403);
        assert_eq!(response.problem().code, ProblemCode::SsoRequired);
    }

    #[tokio::test]
    async fn license_without_the_feature_is_not_enough() {
        let key = license_key(&["project_roles"]);
        let app = TestApp::with_config(move |config| {
            sso_config(config);
            config.license_key = Some(crate::config::Secret::new(key));
        })
        .await;
        assert!(!super::applies(&app.state));
        let response = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("x@sso.example"))
            .send()
            .await;
        assert_eq!(response.status, 401);
        assert_ne!(response.problem().code, ProblemCode::SsoRequired);
    }

    #[tokio::test]
    async fn warning_is_logged_without_domains() {
        let app = TestApp::with_config(sso_config).await;
        let ((), logs) = capture_logs(async { super::warn_if_unlicensed(&app.state) }).await;
        assert!(logs.contains("not enforced"), "{logs}");
        assert!(logs.contains("domains=1"), "{logs}");
        assert!(!logs.contains("sso.example"), "{logs}");
    }

    #[tokio::test]
    async fn admin_settings_report_enforcement() {
        let unlicensed = TestApp::with_config(sso_config).await;
        assert!(!admin_settings_applied(&unlicensed).await);

        let key = license_key(&["sso_enforcement"]);
        let licensed = TestApp::with_config(move |config| {
            sso_config(config);
            config.license_key = Some(crate::config::Secret::new(key));
        })
        .await;
        assert!(admin_settings_applied(&licensed).await);
    }

    /// Sets up the instance admin and returns the `GET /api/v1/admin/settings` answer's
    /// `enforce_sso_applied`.
    async fn admin_settings_applied(app: &TestApp) -> bool {
        bootstrap::prepare(&app.state).await.unwrap();
        let token = app.state.setup.token().unwrap().expect("setup token");
        let created = app
            .request(Method::POST, "/api/auth/setup")
            .json(&serde_json::json!({
                "setup_token": token,
                "email": "ada@example.com",
                "password": PASSWORD,
                "name": "Ada",
                "org_name": "Acme",
                "project_name": "Support bot",
            }))
            .send()
            .await;
        assert_eq!(created.status, 201);
        let cookie = created
            .header("set-cookie")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let settings = app
            .request(Method::GET, "/api/v1/admin/settings")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(settings.status, 200);
        settings.json::<serde_json::Value>()["enforce_sso_applied"]
            .as_bool()
            .unwrap()
    }
}
