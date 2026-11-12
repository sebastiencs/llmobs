//! Google and generic OIDC providers. Metadata is fetched lazily and cached.
//!
//! The login flow reads the client, scopes, and linking fields. Until that
//! lands, the running server stores them and lists the display name.
#![cfg_attr(not(test), allow(dead_code))]

use oauth2::ClientId;
use openidconnect::IssuerUrl;
use openidconnect::core::CoreProviderMetadata;
use tokio::sync::RwLock;

use super::OidcError;
use crate::config::{AutoProvision as ConfigProvision, GoogleAuth, OidcAuth, Secret};

/// How the issuer and the hosted-domain claim are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OidcKind {
    /// Issuer is fixed to `https://accounts.google.com`.
    Google,
    /// Issuer comes from `[[auth.oidc]]`.
    Generic,
}

/// Org and role created for a new user when the provider says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutoProvision {
    /// Org slug. Existence is checked at login, not at startup.
    pub(crate) org: String,
    /// Never [`ProvisionRole`]'s owner. The config check rejects `owner`.
    pub(crate) role: ProvisionRole,
}

/// `auto_provision.role`. Owner is not a value here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProvisionRole {
    /// Org admin.
    Admin,
    /// Org member.
    Member,
    /// Org viewer.
    Viewer,
}

/// One OIDC client, Google or generic.
#[derive(Debug)]
pub(crate) struct OidcProvider {
    id: String,
    kind: OidcKind,
    display_name: String,
    issuer: IssuerUrl,
    client_id: ClientId,
    client_secret: Secret<String>,
    scopes: Vec<String>,
    allowed_domains: Vec<String>,
    auto_provision: Option<AutoProvision>,
    link_existing_by_email: bool,
    http: reqwest::Client,
    metadata: RwLock<Option<(CoreProviderMetadata, i64)>>,
}

impl OidcProvider {
    pub(crate) fn from_google(
        settings: &GoogleAuth,
        http: reqwest::Client,
    ) -> Result<Self, OidcError> {
        Self::new(
            OidcKind::Google,
            "google".to_owned(),
            "Google".to_owned(),
            "https://accounts.google.com",
            &settings.client_id,
            settings.client_secret.clone(),
            default_scopes(),
            settings.allowed_domains.clone(),
            None,
            settings.link_existing_by_email,
            http,
        )
    }

    pub(crate) fn from_generic(
        settings: &OidcAuth,
        http: reqwest::Client,
    ) -> Result<Self, OidcError> {
        Self::new(
            OidcKind::Generic,
            format!("oidc:{}", settings.name),
            settings.display_name.trim().to_owned(),
            &settings.issuer,
            &settings.client_id,
            settings.client_secret.clone(),
            settings.scopes.clone(),
            settings.allowed_domains.clone(),
            settings
                .auto_provision
                .as_ref()
                .map(provision)
                .transpose()?,
            settings.link_existing_by_email,
            http,
        )
    }

    /// A generic provider aimed at the mock IdP.
    #[cfg(test)]
    pub(crate) fn for_test(issuer: &str, http: reqwest::Client) -> Result<Self, OidcError> {
        let secret = serde_json::from_str("\"test-secret\"").expect("secret");
        Self::new(
            OidcKind::Generic,
            "oidc:mock".to_owned(),
            "Mock".to_owned(),
            issuer,
            "test-client",
            secret,
            default_scopes(),
            Vec::new(),
            None,
            false,
            http,
        )
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Discovery document, fetched on first use and when older than an hour.
    ///
    /// A failed refresh keeps the previous document. With nothing cached, the
    /// error is [`OidcError::Unavailable`] and the issuer is not included.
    ///
    /// `now` is nanoseconds since the Unix epoch. The provider does not read
    /// the clock, so tests choose the timestamp.
    pub(crate) async fn metadata(&self, now: i64) -> Result<CoreProviderMetadata, OidcError> {
        if let Some((cached, fetched)) = self.metadata.read().await.as_ref()
            && now.saturating_sub(*fetched) < super::METADATA_TTL_NS
        {
            return Ok(cached.clone());
        }
        match CoreProviderMetadata::discover_async(self.issuer.clone(), &self.http).await {
            Ok(fresh) => {
                *self.metadata.write().await = Some((fresh.clone(), now));
                Ok(fresh)
            }
            Err(_error) => {
                // The discovery error names the issuer and can quote the body.
                // Neither is logged or returned.
                tracing::warn!("identity provider discovery failed");
                self.metadata
                    .read()
                    .await
                    .as_ref()
                    .map(|(cached, _fetched)| cached.clone())
                    .ok_or(OidcError::Unavailable)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        kind: OidcKind,
        id: String,
        display_name: String,
        issuer: &str,
        client_id: &str,
        client_secret: Secret<String>,
        scopes: Vec<String>,
        allowed_domains: Vec<String>,
        auto_provision: Option<AutoProvision>,
        link_existing_by_email: bool,
        http: reqwest::Client,
    ) -> Result<Self, OidcError> {
        let issuer = IssuerUrl::new(issuer.to_owned()).map_err(|_| OidcError::Invalid)?;
        Ok(Self {
            id,
            kind,
            display_name,
            issuer,
            client_id: ClientId::new(client_id.to_owned()),
            client_secret,
            scopes,
            allowed_domains,
            auto_provision,
            link_existing_by_email,
            http,
            metadata: RwLock::new(None),
        })
    }
}

fn default_scopes() -> Vec<String> {
    ["openid", "email", "profile"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn provision(value: &ConfigProvision) -> Result<AutoProvision, OidcError> {
    let role = match value.role.as_str() {
        "admin" => ProvisionRole::Admin,
        "member" => ProvisionRole::Member,
        "viewer" => ProvisionRole::Viewer,
        _ => return Err(OidcError::Invalid),
    };
    Ok(AutoProvision {
        org: value.org.clone(),
        role,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oidc::{METADATA_TTL_NS, identity_client};
    use crate::config::{AutoProvision as ConfigProvision, GoogleAuth, OidcAuth};
    use crate::test_support::mock_idp::MockIdp;

    fn secret(value: &str) -> Secret<String> {
        serde_json::from_str(&format!("\"{value}\"")).expect("secret")
    }

    #[test]
    fn google_and_generic_keep_their_settings() {
        let http = identity_client().expect("client");
        let google = OidcProvider::from_google(
            &GoogleAuth {
                client_id: "g".to_owned(),
                client_secret: secret("g-secret"),
                allowed_domains: vec!["acme.test".to_owned()],
                link_existing_by_email: true,
            },
            http.clone(),
        )
        .expect("google");
        assert_eq!(google.id, "google");
        assert_eq!(google.kind, OidcKind::Google);
        assert_eq!(google.display_name, "Google");
        assert_eq!(google.issuer.as_str(), "https://accounts.google.com");
        assert_eq!(google.client_id.as_str(), "g");
        assert_eq!(google.client_secret.expose(), "g-secret");
        assert_eq!(google.scopes, default_scopes());
        assert_eq!(google.allowed_domains, vec!["acme.test".to_owned()]);
        assert!(google.auto_provision.is_none());
        assert!(google.link_existing_by_email);
        assert!(!format!("{google:?}").contains("g-secret"));

        let generic = OidcProvider::from_generic(
            &OidcAuth {
                name: "okta".to_owned(),
                display_name: " Okta ".to_owned(),
                issuer: "https://acme.okta.com".to_owned(),
                client_id: "o".to_owned(),
                client_secret: secret("o-secret"),
                scopes: vec!["openid".to_owned()],
                allowed_domains: vec!["okta.test".to_owned()],
                auto_provision: Some(ConfigProvision {
                    org: "acme".to_owned(),
                    role: "viewer".to_owned(),
                }),
                link_existing_by_email: false,
            },
            http,
        )
        .expect("generic");
        assert_eq!(generic.id, "oidc:okta");
        assert_eq!(generic.kind, OidcKind::Generic);
        assert_eq!(generic.display_name, "Okta");
        assert_eq!(generic.client_secret.expose(), "o-secret");
        assert_eq!(
            generic.auto_provision,
            Some(AutoProvision {
                org: "acme".to_owned(),
                role: ProvisionRole::Viewer,
            })
        );
        assert!(!generic.link_existing_by_email);
        assert!(!format!("{generic:?}").contains("o-secret"));
    }

    #[tokio::test]
    async fn metadata_is_cached_then_refreshed() {
        let idp = MockIdp::start().await;
        let provider = OidcProvider::for_test(idp.issuer(), identity_client().expect("client"))
            .expect("provider");
        assert_eq!(idp.discovery_hits(), 0);
        let first = provider.metadata(0).await.expect("first");
        assert_eq!(idp.discovery_hits(), 1);
        assert_eq!(first.issuer().as_str(), idp.issuer());
        let cached = provider
            .metadata(METADATA_TTL_NS - 1)
            .await
            .expect("cached");
        assert_eq!(idp.discovery_hits(), 1);
        assert_eq!(cached.issuer().as_str(), first.issuer().as_str());
        let refreshed = provider.metadata(METADATA_TTL_NS).await.expect("refresh");
        assert_eq!(idp.discovery_hits(), 2);
        assert_eq!(refreshed.issuer().as_str(), idp.issuer());
    }

    #[tokio::test]
    async fn stale_metadata_is_kept_when_discovery_fails() {
        let idp = MockIdp::start().await;
        let provider = OidcProvider::for_test(idp.issuer(), identity_client().expect("client"))
            .expect("provider");
        let first = provider.metadata(0).await.expect("first");
        idp.fail_discovery();
        let stale = provider.metadata(METADATA_TTL_NS).await.expect("stale");
        assert_eq!(stale.issuer().as_str(), first.issuer().as_str());
        assert!(idp.discovery_hits() >= 2);

        let fresh = OidcProvider::for_test(idp.issuer(), identity_client().expect("client"))
            .expect("fresh");
        let error = fresh.metadata(0).await.expect_err("unavailable");
        assert!(matches!(error, OidcError::Unavailable));
        let text = error.to_string();
        assert_eq!(text, "identity provider metadata is unavailable");
        assert!(!text.contains("127.0.0.1"), "{text}");
    }
}
