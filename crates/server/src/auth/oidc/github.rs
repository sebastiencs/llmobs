//! GitHub OAuth. GitHub is not an OIDC provider (spec 08).
//!
//! The subject is the numeric user id. The email is the address that is both
//! primary and verified. The user API is called with `User-Agent` set, which
//! GitHub requires.
#![cfg_attr(not(test), allow(dead_code))]

use llmobs_core::names::PRODUCT;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RequestTokenError, Scope, TokenResponse,
    TokenUrl,
};
use serde::Deserialize;

use super::ExchangeError;
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
    http: reqwest::Client,
}

/// The account GitHub returned. `email` is set only when it is primary and verified.
#[derive(Debug)]
pub(crate) struct GithubIdentity {
    /// Decimal user id. Not the login.
    pub(crate) subject: String,
    /// Primary verified address.
    pub(crate) email: Option<String>,
    /// Profile name, before trimming.
    pub(crate) name: Option<String>,
}

type GithubClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

impl GithubProvider {
    pub(crate) fn from_settings(settings: &GithubAuth, http: reqwest::Client) -> Self {
        Self {
            client_id: settings.client_id.clone(),
            client_secret: settings.client_secret.clone(),
            link_existing_by_email: settings.link_existing_by_email,
            endpoints: GithubEndpoints::production(),
            http,
        }
    }

    /// Points the three URLs at a mock IdP base, for example `http://127.0.0.1:9`.
    #[cfg(test)]
    pub(crate) fn for_mock(
        base: &str,
        client_id: String,
        client_secret: Secret<String>,
        http: reqwest::Client,
    ) -> Self {
        let mut provider = Self {
            client_id,
            client_secret,
            link_existing_by_email: true,
            endpoints: GithubEndpoints::production(),
            http,
        };
        provider.point_at(base);
        provider
    }

    /// Retargets authorize, token, and the API at one mock base.
    #[cfg(test)]
    pub(crate) fn point_at(&mut self, base: &str) {
        self.endpoints = GithubEndpoints {
            authorize: format!("{base}/login/oauth/authorize"),
            token: format!("{base}/login/oauth/access_token"),
            api: base.to_owned(),
        };
    }

    /// Authorization URL and the PKCE verifier to store in the state cookie.
    ///
    /// # Errors
    ///
    /// Returns [`ExchangeError::Rejected`] when an endpoint URL is unusable.
    pub(crate) fn authorization_url(
        &self,
        redirect_uri: &str,
        state: &str,
    ) -> Result<(String, String), ExchangeError> {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let verifier = verifier.secret().clone();
        let state = state.to_owned();
        let (url, _csrf) = self
            .client(redirect_uri)?
            .authorize_url(move || CsrfToken::new(state))
            .add_scope(Scope::new("read:user".to_owned()))
            .add_scope(Scope::new("user:email".to_owned()))
            .set_pkce_challenge(challenge)
            .url();
        Ok((url.to_string(), verifier))
    }

    /// Exchanges `code` and reads `/user` plus `/user/emails`.
    ///
    /// A connection failure is [`ExchangeError::Unavailable`]. A rejected token
    /// or user response is [`ExchangeError::Rejected`]. Neither error includes
    /// the token or the response body.
    ///
    /// # Errors
    ///
    /// Returns [`ExchangeError`] when GitHub cannot be reached or rejects the login.
    pub(crate) async fn identity(
        &self,
        redirect_uri: &str,
        code: &str,
        verifier: &str,
    ) -> Result<GithubIdentity, ExchangeError> {
        let token = self
            .client(redirect_uri)?
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.to_owned()))
            .request_async(&self.http)
            .await
            .map_err(|error| token_error(&error))?;
        let access = token.access_token().secret().clone();
        let user: UserBody = self.get_json("/user", &access).await?;
        let emails: Vec<EmailBody> = self.get_json("/user/emails", &access).await?;
        let email = emails
            .into_iter()
            .find(|email| email.primary && email.verified)
            .map(|email| email.email);
        Ok(GithubIdentity {
            subject: user.id.to_string(),
            email,
            name: user.name,
        })
    }

    fn client(&self, redirect_uri: &str) -> Result<GithubClient, ExchangeError> {
        let redirect =
            RedirectUrl::new(redirect_uri.to_owned()).map_err(|_| ExchangeError::Rejected)?;
        let authorize =
            AuthUrl::new(self.endpoints.authorize.clone()).map_err(|_| ExchangeError::Rejected)?;
        let token =
            TokenUrl::new(self.endpoints.token.clone()).map_err(|_| ExchangeError::Rejected)?;
        Ok(BasicClient::new(ClientId::new(self.client_id.clone()))
            .set_client_secret(ClientSecret::new(self.client_secret.expose().clone()))
            .set_auth_uri(authorize)
            .set_token_uri(token)
            .set_redirect_uri(redirect))
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        access_token: &str,
    ) -> Result<T, ExchangeError> {
        let response = self
            .http
            .get(format!("{}{path}", self.endpoints.api))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, PRODUCT)
            .send()
            .await
            .map_err(|_| ExchangeError::Unavailable)?;
        if !response.status().is_success() {
            return Err(ExchangeError::Rejected);
        }
        response.json().await.map_err(|_| ExchangeError::Rejected)
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

#[derive(Deserialize)]
struct UserBody {
    id: u64,
    name: Option<String>,
}

#[derive(Deserialize)]
struct EmailBody {
    email: String,
    primary: bool,
    verified: bool,
}

fn token_error<E, T>(error: &RequestTokenError<E, T>) -> ExchangeError
where
    E: std::error::Error + 'static,
    T: oauth2::ErrorResponse + 'static,
{
    match error {
        RequestTokenError::Request(_) => ExchangeError::Unavailable,
        RequestTokenError::ServerResponse(_)
        | RequestTokenError::Parse(_, _)
        | RequestTokenError::Other(_) => ExchangeError::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_urls_and_the_mock_base() {
        let http = super::super::identity_client().expect("client");
        let settings = GithubAuth {
            client_id: "gh".to_owned(),
            client_secret: serde_json::from_str("\"gh-secret\"").expect("secret"),
            link_existing_by_email: true,
        };
        let provider = GithubProvider::from_settings(&settings, http.clone());
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
            http,
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
