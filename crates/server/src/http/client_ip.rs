//! Client IP from the socket peer and `X-Forwarded-For` (D142).
#![cfg_attr(not(test), allow(dead_code))]

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use ipnet::IpNet;

use super::AppState;
use crate::http::error::ApiError;

/// The address rate limits and sessions record.
///
/// A peer outside `trusted` is returned as-is. A trusted peer walks
/// `X-Forwarded-For` from the right: the first address that is not itself
/// trusted wins. Every entry trusted means the leftmost one. A malformed
/// entry stops the walk and the last parsed address (or the peer) is used.
fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    if !is_trusted(peer, trusted) {
        return peer;
    }
    let entries = forwarded_entries(headers);
    let mut last_good = None;
    for entry in entries.iter().rev() {
        match entry {
            None => return last_good.unwrap_or(peer),
            Some(ip) if !is_trusted(*ip, trusted) => return *ip,
            Some(ip) => last_good = Some(*ip),
        }
    }
    entries.first().copied().flatten().unwrap_or(peer)
}

fn is_trusted(ip: IpAddr, trusted: &[IpNet]) -> bool {
    trusted.iter().any(|net| net.contains(&ip))
}

/// `None` is a malformed entry. Later entries are to the right.
fn forwarded_entries(headers: &HeaderMap) -> Vec<Option<IpAddr>> {
    let mut entries = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        let Ok(text) = value.to_str() else {
            entries.push(None);
            continue;
        };
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                entries.push(None);
            } else {
                entries.push(parse_forwarded(part));
            }
        }
    }
    entries
}

fn parse_forwarded(token: &str) -> Option<IpAddr> {
    let token = if let Some(rest) = token.strip_prefix('[') {
        rest.split_once(']')?.0
    } else {
        token
    };
    token.parse().ok()
}

/// Socket peer, or the client named by a trusted proxy (D142).
pub(crate) struct ClientIp(pub IpAddr);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(state);
        let Some(ConnectInfo(peer)) = parts.extensions.get::<ConnectInfo<SocketAddr>>() else {
            return Err(ApiError::Internal(Box::new(MissingPeer)));
        };
        Ok(Self(client_ip(
            peer.ip(),
            &parts.headers,
            &state.config.trusted_proxies,
        )))
    }
}

#[derive(Debug)]
struct MissingPeer;

impl std::fmt::Display for MissingPeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("missing socket address")
    }
}

impl std::error::Error for MissingPeer {}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;

    use axum::http::{HeaderMap, HeaderValue};
    use ipnet::IpNet;

    use super::client_ip;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn net(text: &str) -> IpNet {
        IpNet::from_str(text).unwrap()
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn an_untrusted_peer_ignores_forwarded_for() {
        let peer = ip("203.0.113.5");
        let got = client_ip(peer, &headers(&["198.51.100.9"]), &[net("10.0.0.0/8")]);
        assert_eq!(got, peer);
    }

    #[test]
    fn a_trusted_peer_uses_the_rightmost_untrusted_address() {
        let got = client_ip(
            ip("10.0.0.8"),
            &headers(&["203.0.113.9, 10.0.0.2"]),
            &[net("10.0.0.0/8")],
        );
        assert_eq!(got, ip("203.0.113.9"));
    }

    #[test]
    fn every_trusted_entry_uses_the_leftmost() {
        let got = client_ip(
            ip("10.0.0.8"),
            &headers(&["10.1.0.1, 10.0.0.2"]),
            &[net("10.0.0.0/8"), net("10.1.0.0/16")],
        );
        assert_eq!(got, ip("10.1.0.1"));
    }

    #[test]
    fn a_malformed_entry_stops_the_walk() {
        let got = client_ip(
            ip("10.0.0.8"),
            &headers(&["203.0.113.9, not-an-ip, 10.0.0.2"]),
            &[net("10.0.0.0/8")],
        );
        assert_eq!(got, ip("10.0.0.2"));
    }

    #[test]
    fn a_malformed_first_entry_from_the_right_keeps_the_peer() {
        let peer = ip("10.0.0.8");
        let got = client_ip(
            peer,
            &headers(&["203.0.113.9, not-an-ip"]),
            &[net("10.0.0.0/8")],
        );
        assert_eq!(got, peer);
    }

    #[test]
    fn ipv6_is_an_address() {
        let got = client_ip(
            ip("10.0.0.8"),
            &headers(&["[2001:db8::5]"]),
            &[net("10.0.0.0/8")],
        );
        assert_eq!(got, ip("2001:db8::5"));
    }
}
