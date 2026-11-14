// SPDX-License-Identifier: Elastic-2.0
//! Signed tokens: licence keys, org entitlement tokens and control-plane requests (D-P-5).
//!
//! A token is `llmobs1.<kid>.<payload>.<signature>`. `<payload>` and `<signature>` are
//! base64url without padding. The signature is ed25519 over the ASCII bytes of
//! `llmobs1.<kid>.<payload>`, checked before the payload is decoded as JSON, so the JSON
//! is never re-serialised. Release builds trust only [`RELEASE_KEYS`]; debug builds also
//! trust [`DEV_KEYS`] (D-P-6). No config can add a key.

#![cfg_attr(not(test), allow(dead_code))] // P4 and P5 use these outside tests.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};

/// Format tag. Renamed with the product (D46).
pub(crate) const TOKEN_TAG: &str = "llmobs1";

/// Largest accepted token, in bytes.
pub(crate) const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Keys every build trusts: `(kid, ed25519 public key in standard base64)`.
/// `prod-1`'s private half is kept outside the repository (generated 2026-11-14).
const RELEASE_KEYS: &[(&str, &str)] = &[("prod-1", "AvIxTEduiC/amifl8QFxLYjXjPYON8LsreI7sL7UDWE=")];

/// Keys only debug builds trust. The private half is `fixtures/license/dev-1.key`.
const DEV_KEYS: &[(&str, &str)] = &[("dev-1", "kMTjOGaR2ry6zgwZ2p2nk0kMsOoyisgt3qfRm4JH36Y=")];

/// Why a token was refused. The text never quotes the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TokenError {
    /// More than [`MAX_TOKEN_BYTES`].
    #[error("the token is longer than 16 KiB")]
    TooLarge,
    /// Not four dot-separated parts, a wrong tag, a bad key id, or bad base64.
    #[error("the token is malformed")]
    Malformed,
    /// The key id is not trusted by this build.
    #[error("the token is signed with a key this build does not trust")]
    UnknownKey,
    /// The signature does not match.
    #[error("the token signature does not verify")]
    BadSignature,
}

/// A token whose signature verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verified {
    /// The key that signed it.
    pub(crate) kid: String,
    /// The decoded payload: UTF-8 JSON, not parsed yet.
    pub(crate) payload: Vec<u8>,
}

/// Verifies `token` (surrounding whitespace ignored) and returns its payload bytes.
///
/// # Errors
///
/// Returns [`TokenError`] when the token is too large, malformed, signed by an unknown key,
/// or its signature does not verify.
pub(crate) fn verify(token: &str) -> Result<Verified, TokenError> {
    let token = token.trim();
    if token.len() > MAX_TOKEN_BYTES {
        return Err(TokenError::TooLarge);
    }
    let mut parts = token.split('.');
    let (Some(tag), Some(kid), Some(payload), Some(signature), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(TokenError::Malformed);
    };
    if tag != TOKEN_TAG || !valid_kid(kid) {
        return Err(TokenError::Malformed);
    }
    let key = trusted_key(kid).ok_or(TokenError::UnknownKey)?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| TokenError::Malformed)?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| TokenError::Malformed)?;
    let signature = Signature::from_slice(&signature_bytes).map_err(|_| TokenError::Malformed)?;
    let signed_len = tag.len() + 1 + kid.len() + 1 + payload.len();
    let signed = token
        .as_bytes()
        .get(..signed_len)
        .ok_or(TokenError::Malformed)?;
    key.verify_strict(signed, &signature)
        .map_err(|_| TokenError::BadSignature)?;
    Ok(Verified {
        kid: kid.to_owned(),
        payload: payload_bytes,
    })
}

/// `[a-z0-9-]{1,32}`.
fn valid_kid(kid: &str) -> bool {
    (1..=32).contains(&kid.len())
        && kid
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The public key named `kid`, if this build trusts it.
fn trusted_key(kid: &str) -> Option<VerifyingKey> {
    let dev: &[(&str, &str)] = if cfg!(debug_assertions) {
        DEV_KEYS
    } else {
        &[]
    };
    RELEASE_KEYS
        .iter()
        .chain(dev)
        .find(|(name, _)| *name == kid)
        .and_then(|(_, key)| decode_key(key))
}

/// A standard-base64 ed25519 public key. `None` when it is not 32 valid bytes; the
/// `keys_decode` test proves every built-in key decodes.
fn decode_key(key: &str) -> Option<VerifyingKey> {
    let bytes = STANDARD.decode(key).ok()?;
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sign_token;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    /// The development seed, as shipped in `fixtures/license/dev-1.key`.
    const DEV_KEY_FILE: &str = include_str!("../../../../fixtures/license/dev-1.key");

    #[test]
    fn keys_decode() {
        for (kid, key) in RELEASE_KEYS.iter().chain(DEV_KEYS) {
            assert!(decode_key(key).is_some(), "{kid} does not decode");
            assert!(valid_kid(kid), "{kid} is not a valid kid");
        }
    }

    #[test]
    fn dev_seed_matches_dev_key() {
        let seed: [u8; 32] = STANDARD
            .decode(DEV_KEY_FILE.trim())
            .unwrap()
            .try_into()
            .unwrap();
        let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        assert_eq!(STANDARD.encode(public), DEV_KEYS[0].1);
    }

    #[test]
    fn round_trip() {
        let payload = json!({"kind": "license"});
        let token = sign_token(&payload);
        let verified = verify(&token).unwrap();
        assert_eq!(verified.kid, "dev-1");
        assert_eq!(verified.payload, serde_json::to_vec(&payload).unwrap());

        let padded = format!("  {token}\n");
        assert_eq!(verify(&padded).unwrap(), verified);
    }

    #[test]
    fn tampering_is_refused() {
        let token = sign_token(&json!({"kind": "license"}));
        let parts: Vec<&str> = token.split('.').collect();

        // Old signature, different payload.
        let body = URL_SAFE_NO_PAD.encode(br#"{"kind":"other"}"#);
        let forged = format!("{}.{}.{body}.{}", parts[0], parts[1], parts[3]);
        assert_eq!(verify(&forged), Err(TokenError::BadSignature));

        // Last character of the signature changed to another base64url symbol.
        let signature = parts[3];
        let last = signature.chars().last().unwrap();
        let replacement = if last == 'A' { 'B' } else { 'A' };
        let altered = format!(
            "{}.{}.{}.{}{replacement}",
            parts[0],
            parts[1],
            parts[2],
            &signature[..signature.len() - 1]
        );
        assert!(matches!(
            verify(&altered),
            Err(TokenError::BadSignature | TokenError::Malformed)
        ));
    }

    #[test]
    fn malformed_tokens() {
        let body = URL_SAFE_NO_PAD.encode(b"{}");
        let sig = URL_SAFE_NO_PAD.encode([0_u8; 64]);
        let cases = [
            String::new(),
            "llmobs1.dev-1.abc".to_owned(),
            format!("llmobs1.dev-1.{body}.{sig}.extra"),
            format!("llmobs2.dev-1.{body}.{sig}"),
            format!("llmobs1.Dev-1.{body}.{sig}"),
            format!("llmobs1.{}.{body}.{sig}", "a".repeat(33)),
            format!("llmobs1.dev-1.!!.{sig}"),
            format!(
                "llmobs1.dev-1.{body}.{}",
                URL_SAFE_NO_PAD.encode([0_u8; 10])
            ),
        ];
        for token in cases {
            assert_eq!(verify(&token), Err(TokenError::Malformed), "{token}");
        }
        let oversized = "a".repeat(MAX_TOKEN_BYTES + 1);
        assert_eq!(verify(&oversized), Err(TokenError::TooLarge));
    }

    #[test]
    fn unknown_key() {
        let token = sign_token(&json!({"kind": "license"})).replacen(".dev-1.", ".nope-1.", 1);
        assert_eq!(verify(&token), Err(TokenError::UnknownKey));
    }

    #[test]
    fn release_key_needs_prod_signature() {
        let token = sign_token(&json!({"kind": "license"})).replacen(".dev-1.", ".prod-1.", 1);
        assert_eq!(verify(&token), Err(TokenError::BadSignature));
    }

    #[test]
    fn error_text_never_quotes_the_token() {
        for error in [
            TokenError::TooLarge,
            TokenError::Malformed,
            TokenError::UnknownKey,
            TokenError::BadSignature,
        ] {
            assert!(!error.to_string().contains("llmobs1."), "{error}");
        }
    }
}
