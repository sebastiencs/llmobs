//! OAuth and OIDC providers (spec 08, D148).
//!
//! Discovery runs on first use, not at startup, so an identity provider outage
//! does not stop the server. The HTTP client refuses redirects.
//!
//! The login flow reads [`Providers::get`]. Until that lands, only [`Providers::list`]
//! is called from the running server.
#![cfg_attr(not(test), allow(dead_code))]

mod github;
mod provider;

use std::time::Duration;

use crate::api::account::AuthProvider;
use crate::config::AuthSettings;

pub(crate) use github::GithubProvider;
pub(crate) use provider::OidcProvider;

/// One hour, in nanoseconds. Metadata older than this is fetched again.
pub(crate) const METADATA_TTL_NS: i64 = 3_600 * 1_000_000_000;

/// Failure from provider setup or discovery.
///
/// The text is static. It does not include an issuer, a client secret, or a
/// response body.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OidcError {
    /// Discovery failed and there is no cached document.
    #[error("identity provider metadata is unavailable")]
    Unavailable,
    /// `reqwest` could not build the client.
    #[error("could not build the identity provider client")]
    Client,
    /// A provider that passed the type system still has an issuer or role the
    /// runtime cannot use. [`crate::config::ServerConfig::load`] rejects those
    /// before this is reached.
    #[error("identity provider configuration is invalid")]
    Invalid,
}

/// A configured provider, borrowed from [`Providers`].
#[derive(Debug)]
pub(crate) enum ProviderRef<'a> {
    /// GitHub's OAuth endpoints.
    Github(&'a GithubProvider),
    /// Google or a generic OIDC provider.
    Oidc(&'a OidcProvider),
}

/// The providers from `[auth]`, in list order.
#[derive(Debug)]
pub(crate) struct Providers {
    github: Option<GithubProvider>,
    /// Google first, then `[[auth.oidc]]` in config order.
    oidc: Vec<OidcProvider>,
}

impl Providers {
    /// Builds providers without discovering metadata.
    ///
    /// # Errors
    ///
    /// Returns [`OidcError::Invalid`] when an issuer or auto-provision role
    /// cannot be used. Load-time validation rejects those inputs.
    pub(crate) fn from_config(
        cfg: &AuthSettings,
        http: &reqwest::Client,
    ) -> Result<Self, OidcError> {
        let github = cfg.github.as_ref().map(GithubProvider::from_settings);
        let mut oidc = Vec::new();
        if let Some(google) = &cfg.google {
            oidc.push(OidcProvider::from_google(google, http.clone())?);
        }
        for entry in &cfg.oidc {
            oidc.push(OidcProvider::from_generic(entry, http.clone())?);
        }
        Ok(Self { github, oidc })
    }

    /// `github`, `google`, or `oidc:<name>`.
    pub(crate) fn get(&self, id: &str) -> Option<ProviderRef<'_>> {
        if id == "github" {
            return self.github.as_ref().map(ProviderRef::Github);
        }
        self.oidc
            .iter()
            .find(|provider| provider.id() == id)
            .map(ProviderRef::Oidc)
    }

    /// GitHub, then Google, then generic OIDC providers in config order.
    pub(crate) fn list(&self) -> Vec<AuthProvider> {
        let mut providers = Vec::new();
        if self.github.is_some() {
            providers.push(AuthProvider {
                id: "github".to_owned(),
                display_name: "GitHub".to_owned(),
            });
        }
        for provider in &self.oidc {
            providers.push(AuthProvider {
                id: provider.id().to_owned(),
                display_name: provider.display_name().to_owned(),
            });
        }
        providers
    }
}

/// The client used for discovery and, later, the code exchange.
///
/// Redirects stay off. `openidconnect` warns that following them enables SSRF.
/// The timeout is 10 seconds. Issuers are operator configuration, so spec 12's
/// private-network guard is not applied.
///
/// # Errors
///
/// Returns [`OidcError::Client`] when the client cannot be built.
pub(crate) fn identity_client() -> Result<reqwest::Client, OidcError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| OidcError::Client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AutoProvision, GithubAuth, GoogleAuth, OidcAuth, Secret};

    fn secret(value: &str) -> Secret<String> {
        serde_json::from_str(&format!("\"{value}\"")).expect("secret")
    }

    fn settings() -> AuthSettings {
        AuthSettings {
            github: Some(GithubAuth {
                client_id: "gh".to_owned(),
                client_secret: secret("gh-secret"),
                link_existing_by_email: true,
            }),
            google: Some(GoogleAuth {
                client_id: "g".to_owned(),
                client_secret: secret("g-secret"),
                allowed_domains: vec!["acme.test".to_owned()],
                link_existing_by_email: true,
            }),
            oidc: vec![OidcAuth {
                name: "okta".to_owned(),
                display_name: "Okta".to_owned(),
                issuer: "https://acme.okta.com".to_owned(),
                client_id: "o".to_owned(),
                client_secret: secret("o-secret"),
                scopes: vec!["openid".to_owned(), "email".to_owned()],
                allowed_domains: Vec::new(),
                auto_provision: Some(AutoProvision {
                    org: "acme".to_owned(),
                    role: "member".to_owned(),
                }),
                link_existing_by_email: false,
            }],
            ..AuthSettings::default()
        }
    }

    #[test]
    fn list_and_get_follow_config_order() {
        let http = identity_client().expect("client");
        let providers = Providers::from_config(&settings(), &http).expect("providers");
        assert_eq!(
            providers
                .list()
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["github", "google", "oidc:okta"]
        );
        match providers.get("github") {
            Some(ProviderRef::Github(provider)) => {
                assert_eq!(provider.endpoints().api, "https://api.github.com");
            }
            other => panic!("expected github, got {other:?}"),
        }
        match providers.get("google") {
            Some(ProviderRef::Oidc(provider)) => assert_eq!(provider.id(), "google"),
            other => panic!("expected google, got {other:?}"),
        }
        match providers.get("oidc:okta") {
            Some(ProviderRef::Oidc(provider)) => assert_eq!(provider.display_name(), "Okta"),
            other => panic!("expected okta, got {other:?}"),
        }
        assert!(providers.get("oidc:missing").is_none());
        let rendered = format!("{providers:?}");
        assert!(!rendered.contains("gh-secret"), "{rendered}");
        assert!(!rendered.contains("g-secret"), "{rendered}");
        assert!(!rendered.contains("o-secret"), "{rendered}");
    }

    #[tokio::test]
    async fn identity_client_does_not_follow_redirects() {
        let idp = crate::test_support::mock_idp::MockIdp::start().await;
        let http = identity_client().expect("client");
        let response = http
            .get(format!("{}/redirect", idp.issuer()))
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert_eq!(idp.landed_hits(), 0);
    }
}
