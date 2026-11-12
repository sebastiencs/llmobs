//! GitHub OAuth. GitHub is not an OIDC provider (spec 08).
//!
//! The code exchange reads the client and the endpoints. Until that lands, the
//! running server only stores them.
#![cfg_attr(not(test), allow(dead_code))]

use crate::config::{GithubAuth, Secret};

/// Production GitHub URLs, or the mock IdP in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubEndpoints {
    /// Authorization redirect.
    pub(crate) authorize: String,
    /// Code exchange.
    pub(crate) token: String,
    /// `GET /user` and `GET /user/emails`.
    pub(crate) api: String,
}

impl GithubEndpoints {
    fn production() -> Self {
        Self {
            authorize: "https://github.com/login/oauth/authorize".to_owned(),
            token: "https://github.com/login/oauth/access_token".to_owned(),
            api: "https://api.github.com".to_owned(),
        }
    }
}

/// One GitHub OAuth app.
#[derive(Debug, Clone)]
pub(crate) struct GithubProvider {
    client_id: String,
    client_secret: Secret<String>,
    link_existing_by_email: bool,
    endpoints: GithubEndpoints,
}

impl GithubProvider {
    pub(crate) fn from_settings(settings: &GithubAuth) -> Self {
        Self {
            client_id: settings.client_id.clone(),
            client_secret: settings.client_secret.clone(),
            link_existing_by_email: settings.link_existing_by_email,
            endpoints: GithubEndpoints::production(),
        }
    }

    /// Points the three URLs at a mock IdP base, for example `http://127.0.0.1:9`.
    #[cfg(test)]
    pub(crate) fn for_mock(base: &str, client_id: String, client_secret: Secret<String>) -> Self {
        Self {
            client_id,
            client_secret,
            link_existing_by_email: true,
            endpoints: GithubEndpoints {
                authorize: format!("{base}/login/oauth/authorize"),
                token: format!("{base}/login/oauth/access_token"),
                api: base.to_owned(),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn client_id(&self) -> &str {
        &self.client_id
    }

    #[cfg(test)]
    pub(crate) fn client_secret(&self) -> &str {
        self.client_secret.expose()
    }

    #[cfg(test)]
    pub(crate) fn link_existing_by_email(&self) -> bool {
        self.link_existing_by_email
    }

    #[cfg(test)]
    pub(crate) fn endpoints(&self) -> &GithubEndpoints {
        &self.endpoints
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_urls_and_the_mock_base() {
        let settings = GithubAuth {
            client_id: "gh".to_owned(),
            client_secret: serde_json::from_str("\"gh-secret\"").expect("secret"),
            link_existing_by_email: true,
        };
        let provider = GithubProvider::from_settings(&settings);
        assert_eq!(provider.client_id(), "gh");
        assert_eq!(provider.client_secret(), "gh-secret");
        assert!(provider.link_existing_by_email());
        assert_eq!(
            provider.endpoints().authorize,
            "https://github.com/login/oauth/authorize"
        );
        assert_eq!(
            provider.endpoints().token,
            "https://github.com/login/oauth/access_token"
        );
        assert_eq!(provider.endpoints().api, "https://api.github.com");
        assert!(!format!("{provider:?}").contains("gh-secret"));

        let mock = GithubProvider::for_mock(
            "http://127.0.0.1:9",
            "test-client".to_owned(),
            serde_json::from_str("\"test-secret\"").expect("secret"),
        );
        assert_eq!(
            mock.endpoints().token,
            "http://127.0.0.1:9/login/oauth/access_token"
        );
        assert_eq!(mock.endpoints().api, "http://127.0.0.1:9");
        assert_eq!(
            mock.endpoints().authorize,
            "http://127.0.0.1:9/login/oauth/authorize"
        );
    }
}
