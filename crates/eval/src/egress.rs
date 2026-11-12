//! Outbound HTTP for judges and webhooks, with the SSRF guard (D166).
//!
//! `no_proxy` is required. An `HTTP_PROXY` would connect to the proxy and skip
//! this resolver, so the guard would never see the target address.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::{StatusCode, Url};

use crate::error::EgressError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether resolved and literal addresses in a private range may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressPolicy {
    /// Keep addresses that [`is_denied`] would drop.
    pub allow_private_network: bool,
}

/// One shared client. Redirects are not followed.
#[derive(Debug, Clone)]
pub struct EgressClient {
    http: reqwest::Client,
    policy: EgressPolicy,
}

/// One POST. The caller owns the URL, the timeout, and the body cap.
pub(crate) struct EgressRequest<'a> {
    pub(crate) url: &'a Url,
    pub(crate) headers: Vec<(HeaderName, HeaderValue)>,
    pub(crate) body: Vec<u8>,
    pub(crate) timeout: Duration,
    pub(crate) max_response_bytes: usize,
}

/// The status and body of one response. The body is at most the request's cap.
#[derive(Debug)]
pub(crate) struct EgressResponse {
    pub status: StatusCode,
    pub body: Bytes,
}

impl EgressClient {
    /// Builds a rustls client that resolves through [`GuardedResolver`].
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::Request`] when the TLS backend cannot be initialized.
    pub fn new(policy: EgressPolicy) -> Result<Self, EgressError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(user_agent())
            .use_rustls_tls()
            .dns_resolver(Arc::new(GuardedResolver {
                allow_private_network: policy.allow_private_network,
            }))
            .build()
            .map_err(|_| EgressError::Request)?;
        Ok(Self { http, policy })
    }

    /// POSTs `req` and reads the body until the cap.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::Scheme`] for a non-HTTP URL, [`EgressError::Denied`]
    /// when the literal address or every resolved address is denied, and
    /// [`EgressError::TooLarge`] when the body grows past the cap. Other client
    /// failures become [`EgressError::Timeout`], [`EgressError::Connect`], or
    /// [`EgressError::Request`]. Their text is not the client error's `Display`,
    /// which contains the URL.
    pub(crate) async fn post(&self, req: EgressRequest<'_>) -> Result<EgressResponse, EgressError> {
        if req.url.scheme() != "http" && req.url.scheme() != "https" {
            return Err(EgressError::Scheme);
        }
        let host = host_label(req.url);
        if !self.policy.allow_private_network && literal_ip(req.url).is_some_and(is_denied) {
            return Err(EgressError::Denied { host });
        }

        let mut builder = self
            .http
            .post(req.url.clone())
            .timeout(req.timeout)
            .body(req.body);
        for (name, value) in req.headers {
            builder = builder.header(name, value);
        }
        let response = builder.send().await.map_err(|err| classify(&err, &host))?;

        let status = response.status();
        let mut response = response;
        let mut collected = Vec::new();
        loop {
            let Some(chunk) = response
                .chunk()
                .await
                .map_err(|err| classify(&err, &host))?
            else {
                break;
            };
            let next = collected.len().saturating_add(chunk.len());
            if next > req.max_response_bytes {
                return Err(EgressError::TooLarge {
                    limit: req.max_response_bytes,
                });
            }
            collected.extend_from_slice(&chunk);
        }
        Ok(EgressResponse {
            status,
            body: Bytes::from(collected),
        })
    }
}

fn user_agent() -> String {
    format!(
        "{}/{}",
        llmobs_core::names::PRODUCT,
        env!("CARGO_PKG_VERSION")
    )
}

fn host_label(url: &Url) -> String {
    url.host_str().unwrap_or("").to_owned()
}

/// A literal address in the URL host. Bracketed IPv6 is unwrapped. A name is `None`.
fn literal_ip(url: &Url) -> Option<IpAddr> {
    let host = url.host_str()?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.parse().ok()
}

fn classify(err: &reqwest::Error, host: &str) -> EgressError {
    if caused_by_denied(err) {
        return EgressError::Denied {
            host: host.to_owned(),
        };
    }
    if err.is_timeout() {
        return EgressError::Timeout;
    }
    if err.is_connect() {
        return EgressError::Connect;
    }
    EgressError::Request
}

fn caused_by_denied(err: &reqwest::Error) -> bool {
    let mut current = std::error::Error::source(err);
    while let Some(cause) = current {
        if cause.is::<DeniedAddress>() {
            return true;
        }
        current = cause.source();
    }
    false
}

/// Returned from the resolver when every address was dropped.
#[derive(Debug)]
struct DeniedAddress;

impl fmt::Display for DeniedAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("address denied")
    }
}

impl std::error::Error for DeniedAddress {}

struct GuardedResolver {
    allow_private_network: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private_network = self.allow_private_network;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let looked_up = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|_| Box::new(LookupFailed) as Box<dyn std::error::Error + Send + Sync>)?;
            let kept: Vec<SocketAddr> = looked_up
                .filter(|addr| allow_private_network || !is_denied(addr.ip()))
                .collect();
            if kept.is_empty() {
                return Err(Box::new(DeniedAddress) as Box<dyn std::error::Error + Send + Sync>);
            }
            let addrs: Addrs = Box::new(kept.into_iter());
            Ok(addrs)
        })
    }
}

#[derive(Debug)]
struct LookupFailed;

impl fmt::Display for LookupFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("lookup failed")
    }
}

impl std::error::Error for LookupFailed {}

/// Whether `ip` is in a D166 range.
///
/// Documentation addresses are the IANA ranges: `192.0.2.0/24`, `198.51.100.0/24`,
/// `203.0.113.0/24`, and `2001:db8::/32`. IPv4-mapped (`::ffff:0:0/96`) and NAT64
/// (`64:ff9b::/96`) addresses are judged by the embedded IPv4 address.
#[must_use]
pub(crate) fn is_denied(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => denied_v4(ip),
        IpAddr::V6(ip) => denied_v6(ip),
    }
}

fn denied_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
}

fn denied_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return denied_v4(v4);
    }
    if let Some(v4) = nat64_v4(ip) {
        return denied_v4(v4);
    }
    let [first, second, ..] = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || first & 0xffc0 == 0xfe80
        || first & 0xfe00 == 0xfc00
        || (first == 0x2001 && second == 0x0db8)
}

/// The IPv4 address embedded in `64:ff9b::/96`, the well-known NAT64 prefix.
fn nat64_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let octets = ip.octets();
    if octets[..12] == [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0] {
        Some(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::Router;
    use axum::extract::State;
    use axum::http::{StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use reqwest::Url;

    use super::{EgressClient, EgressPolicy, EgressRequest, is_denied};
    use crate::error::{EgressError, assert_no_payload};

    const PLANTED: &str = "SECRET-PAYLOAD";

    fn addr(text: &str) -> IpAddr {
        text.parse().expect(text)
    }

    #[test]
    fn denies_each_range_at_its_ends() {
        let denied = [
            "127.0.0.0",
            "127.255.255.255",
            "10.0.0.0",
            "10.255.255.255",
            "172.16.0.0",
            "172.31.255.255",
            "192.168.0.0",
            "192.168.255.255",
            "100.64.0.0",
            "100.127.255.255",
            "169.254.0.0",
            "169.254.255.255",
            "0.0.0.0",
            "224.0.0.0",
            "239.255.255.255",
            "255.255.255.255",
            "192.0.0.0",
            "192.0.0.255",
            "198.18.0.0",
            "198.19.255.255",
            "192.0.2.0",
            "192.0.2.255",
            "198.51.100.0",
            "198.51.100.255",
            "203.0.113.0",
            "203.0.113.255",
            "::1",
            "::",
            "fe80::",
            "febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "fc00::",
            "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "ff00::",
            "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:db8::",
            "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff",
            "::ffff:127.0.0.1",
            "::ffff:10.255.255.255",
            "::ffff:0.0.0.0",
            "::ffff:192.0.2.1",
            "::ffff:169.254.169.254",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:0",
            "64:ff9b::ffff:ffff",
            "64:ff9b::c000:201",
        ];
        let allowed = [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "9.255.255.255",
            "11.0.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.167.255.255",
            "192.169.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "169.253.255.255",
            "169.255.0.0",
            "0.0.0.1",
            "223.255.255.255",
            "240.0.0.0",
            "192.0.1.0",
            "198.17.255.255",
            "198.20.0.0",
            "192.0.3.0",
            "198.51.101.0",
            "203.0.114.0",
            "2001:4860:4860::8888",
            "fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "fec0::",
            "fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "fe00::",
            "2001:db7:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:db9::",
            "::ffff:1.1.1.1",
            "64:ff9b::101:101",
            "64:ff9b:1::7f00:1",
        ];
        for text in denied {
            assert!(is_denied(addr(text)), "{text} is allowed");
        }
        for text in allowed {
            assert!(!is_denied(addr(text)), "{text} is denied");
        }
    }

    #[tokio::test]
    async fn blocks_loopback_unless_the_policy_allows_it() {
        let mock = Mock::start().await;
        let blocked = client(false);
        let literal = mock.url(false);
        let err = post(&blocked, &literal, Duration::from_secs(2), 64)
            .await
            .expect_err("literal loopback");
        assert_eq!(err, EgressError::Denied { host: mock.ip() });
        assert_no_secret(&err);

        let name = mock.url(true);
        let err = post(&blocked, &name, Duration::from_secs(2), 64)
            .await
            .expect_err("localhost");
        assert_eq!(
            err,
            EgressError::Denied {
                host: "localhost".to_owned(),
            }
        );
        assert_no_secret(&err);
        assert_eq!(mock.hits(), 0);

        let allowed = client(true);
        let response = post(&allowed, &literal, Duration::from_secs(2), 64)
            .await
            .expect("allowed literal");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body.as_ref(), b"ok");
        let response = post(&allowed, &name, Duration::from_secs(2), 64)
            .await
            .expect("allowed localhost");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(mock.hits(), 2);
    }

    #[tokio::test]
    async fn returns_a_redirect_and_caps_the_body() {
        let mock = Mock::start().await;
        let client = client(true);
        let redirect = mock.path("/redirect");
        let response = post(&client, &redirect, Duration::from_secs(2), 64)
            .await
            .expect("redirect");
        assert_eq!(response.status, StatusCode::FOUND);
        assert_eq!(mock.hits(), 1);

        let big = mock.path("/big");
        let err = post(&client, &big, Duration::from_secs(2), 4)
            .await
            .expect_err("body cap");
        assert_eq!(err, EgressError::TooLarge { limit: 4 });
        assert_no_secret(&err);

        let exact = post(&client, &mock.path("/"), Duration::from_secs(2), 2)
            .await
            .expect("exact cap");
        assert_eq!(exact.body.as_ref(), b"ok");
    }

    #[tokio::test]
    async fn times_out_and_refuses_other_schemes() {
        let mock = Mock::start().await;
        let client = client(true);
        let err = post(&client, &mock.path("/slow"), Duration::from_millis(200), 64)
            .await
            .expect_err("timeout");
        assert_eq!(err, EgressError::Timeout);
        assert_no_secret(&err);

        let ftp = Url::parse(&format!("ftp://example.com/hook?token={PLANTED}")).unwrap();
        let err = post(&client, &ftp, Duration::from_secs(2), 64)
            .await
            .expect_err("scheme");
        assert_eq!(err, EgressError::Scheme);
        assert_no_secret(&err);
        assert!(!err.to_string().contains("example.com"));
    }

    fn client(allow_private_network: bool) -> EgressClient {
        EgressClient::new(EgressPolicy {
            allow_private_network,
        })
        .expect("client")
    }

    async fn post(
        client: &EgressClient,
        url: &Url,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<super::EgressResponse, EgressError> {
        client
            .post(EgressRequest {
                url,
                headers: Vec::new(),
                body: Vec::new(),
                timeout,
                max_response_bytes,
            })
            .await
    }

    fn assert_no_secret(err: &EgressError) {
        let message = err.to_string();
        assert_no_payload(&message, PLANTED);
        assert_no_payload(&message, "/hook");
        assert_no_payload(&message, "token");
    }

    struct Mock {
        port: u16,
        hits: Arc<AtomicUsize>,
    }

    impl Mock {
        async fn start() -> Self {
            let hits = Arc::new(AtomicUsize::new(0));
            let app = Router::new().fallback(hit).with_state(Arc::clone(&hits));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self { port, hits }
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::Relaxed)
        }

        fn ip(&self) -> String {
            "127.0.0.1".to_owned()
        }

        fn url(&self, localhost: bool) -> Url {
            let host = if localhost { "localhost" } else { "127.0.0.1" };
            Url::parse(&format!("http://{host}:{}/hook?token={PLANTED}", self.port)).unwrap()
        }

        fn path(&self, path: &str) -> Url {
            Url::parse(&format!(
                "http://127.0.0.1:{}{path}?token={PLANTED}",
                self.port
            ))
            .unwrap()
        }
    }

    async fn hit(State(hits): State<Arc<AtomicUsize>>, uri: Uri) -> Response {
        hits.fetch_add(1, Ordering::Relaxed);
        match uri.path() {
            "/slow" => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                StatusCode::OK.into_response()
            }
            "/big" => (StatusCode::OK, PLANTED).into_response(),
            "/redirect" => {
                let mut response = StatusCode::FOUND.into_response();
                response.headers_mut().insert(
                    axum::http::header::LOCATION,
                    axum::http::HeaderValue::from_static("/landed"),
                );
                response
            }
            _ => (StatusCode::OK, "ok").into_response(),
        }
    }
}
