//! API keys, personal tokens, and opaque tokens (spec 08, D140).
#![cfg_attr(not(test), allow(dead_code))]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::AuthError;

/// Product name inside `sk-` / `pk-` / `pat-` prefixes. Renaming is
/// [`llmobs_core::names::PRODUCT`] (D46).
const NAME: &str = llmobs_core::names::PRODUCT;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A project API key or a personal access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyKind {
    /// `sk-llmobs-` and 32 base62 characters.
    Secret,
    /// `pk-llmobs-` and 24 base62 characters.
    Public,
    /// `pat-llmobs-` and 32 base62 characters.
    Personal,
}

/// A newly minted key. `prefix` is what the UI stores and shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewKey {
    /// The bearer value. Shown once.
    pub(crate) plaintext: String,
    /// SHA-256 of `plaintext`, lowercase hex.
    pub(crate) hash: String,
    /// Type prefix plus the first 6 tail characters.
    pub(crate) prefix: String,
}

/// A session, invite, or reset token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpaqueToken {
    /// 32 random bytes, base64url without padding.
    pub(crate) plaintext: String,
    /// SHA-256 of `plaintext`, lowercase hex.
    pub(crate) hash: String,
}

/// Mints a key. The tail is base62 with no modulo bias.
///
/// # Errors
///
/// Returns [`AuthError::Rng`] when the OS random source fails.
pub(crate) fn new_key(kind: KeyKind) -> Result<NewKey, AuthError> {
    let mut tail = String::with_capacity(tail_len(kind));
    for _ in 0..tail_len(kind) {
        tail.push(draw_base62()? as char);
    }
    let mut plaintext = String::with_capacity(prefix_len(kind) + tail.len());
    plaintext.push_str(tag(kind));
    plaintext.push('-');
    plaintext.push_str(NAME);
    plaintext.push('-');
    plaintext.push_str(&tail);
    let prefix = display_prefix(&plaintext);
    let hash = hash_token(&plaintext);
    Ok(NewKey {
        plaintext,
        hash,
        prefix,
    })
}

/// The key kind when `bearer` has the exact prefix, the exact length, and a base62 tail.
///
/// A wrong prefix returns [`None`] without allocating. The tail check scans every
/// character, so a bad character does not stop the scan early.
pub(crate) fn parse_key(bearer: &str) -> Option<KeyKind> {
    let (kind, tail) = split_tail(bearer)?;
    if tail.len() != tail_len(kind) {
        return None;
    }
    let mut base62 = true;
    for byte in tail.bytes() {
        let ok = byte.is_ascii_digit() || byte.is_ascii_uppercase() || byte.is_ascii_lowercase();
        base62 &= ok;
    }
    base62.then_some(kind)
}

/// Type prefix plus the first 6 tail characters (D140).
pub(crate) fn display_prefix(plaintext: &str) -> String {
    let Some((_, tail)) = split_tail(plaintext) else {
        return String::new();
    };
    let prefix_len = plaintext.len() - tail.len();
    let mut prefix = plaintext[..prefix_len].to_owned();
    prefix.extend(tail.chars().take(6));
    prefix
}

/// 32 random bytes, base64url without padding (43 characters).
///
/// # Errors
///
/// Returns [`AuthError::Rng`] when the OS random source fails.
pub(crate) fn new_opaque_token() -> Result<OpaqueToken, AuthError> {
    let bytes = random_bytes(32)?;
    let plaintext = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash_token(&plaintext);
    Ok(OpaqueToken { plaintext, hash })
}

/// 43 characters of the base64url alphabet, without padding.
pub(crate) fn is_opaque_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
}

/// SHA-256, lowercase hex.
pub(crate) fn hash_token(plaintext: &str) -> String {
    hex_encode(Sha256::digest(plaintext.as_bytes()).as_slice())
}

/// Compares two strings without stopping at the first different byte.
pub(crate) fn constant_time_eq(left: &str, right: &str) -> bool {
    bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}

/// Fills `buf` from [`rand::rngs::SysRng`].
///
/// # Errors
///
/// Returns [`AuthError::Rng`] when `try_fill_bytes` fails. That error is the
/// OS entropy source being unavailable; the bytes are discarded.
pub(crate) fn random_bytes(len: usize) -> Result<Vec<u8>, AuthError> {
    let mut buf = vec![0u8; len];
    rand::rngs::SysRng
        .try_fill_bytes(&mut buf)
        .map_err(|_| AuthError::Rng)?;
    Ok(buf)
}

fn draw_base62() -> Result<u8, AuthError> {
    // 248 = 4 × 62. Bytes at or above 248 are rejected so `% 62` is uniform.
    let mut byte = [0u8; 1];
    loop {
        rand::rngs::SysRng
            .try_fill_bytes(&mut byte)
            .map_err(|_| AuthError::Rng)?;
        if byte[0] < 248 {
            return Ok(BASE62[usize::from(byte[0] % 62)]);
        }
    }
}

fn tag(kind: KeyKind) -> &'static str {
    match kind {
        KeyKind::Secret => "sk",
        KeyKind::Public => "pk",
        KeyKind::Personal => "pat",
    }
}

fn tail_len(kind: KeyKind) -> usize {
    match kind {
        KeyKind::Secret | KeyKind::Personal => 32,
        KeyKind::Public => 24,
    }
}

fn prefix_len(kind: KeyKind) -> usize {
    tag(kind).len() + 1 + NAME.len() + 1
}

fn split_tail(bearer: &str) -> Option<(KeyKind, &str)> {
    let (kind, rest) = [
        (KeyKind::Personal, "pat-"),
        (KeyKind::Secret, "sk-"),
        (KeyKind::Public, "pk-"),
    ]
    .into_iter()
    .find_map(|(kind, prefix)| bearer.strip_prefix(prefix).map(|rest| (kind, rest)))?;
    let rest = rest.strip_prefix(NAME)?.strip_prefix('-')?;
    Some((kind, rest))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn keys_have_the_right_shape_and_parse() {
        let mut seen = HashSet::new();
        for kind in [KeyKind::Secret, KeyKind::Public, KeyKind::Personal] {
            for _ in 0..1_000 {
                let key = new_key(kind).unwrap();
                let tail = key.plaintext.len() - prefix_len(kind);
                assert_eq!(tail, tail_len(kind));
                assert!(key.plaintext.starts_with(&format!("{}-{NAME}-", tag(kind))));
                assert!(key.plaintext[prefix_len(kind)..].bytes().all(|byte| {
                    byte.is_ascii_digit() || byte.is_ascii_uppercase() || byte.is_ascii_lowercase()
                }));
                assert_eq!(parse_key(&key.plaintext), Some(kind));
                assert_eq!(key.prefix, display_prefix(&key.plaintext));
                assert_eq!(key.prefix.len(), prefix_len(kind) + 6);
                assert_eq!(key.hash, hash_token(&key.plaintext));
                assert!(seen.insert(key.plaintext));
            }
        }

        let key = new_key(KeyKind::Secret).unwrap();
        assert_eq!(parse_key(&key.plaintext[..key.plaintext.len() - 1]), None);
        assert_eq!(parse_key(&format!("{}a", key.plaintext)), None);
        let mut dashed = key.plaintext;
        let last = dashed.len() - 1;
        dashed.replace_range(last.., "-");
        assert_eq!(parse_key(&dashed), None);
        assert_eq!(parse_key(&format!("xx-{NAME}-{}", "a".repeat(32))), None);
        assert_eq!(parse_key(&format!("pat-{NAME}-{}", "a".repeat(24))), None);
        assert_eq!(display_prefix("sk-llmobs-a1B2c3…"), "sk-llmobs-a1B2c3");
    }

    #[test]
    fn opaque_tokens_and_the_sha256_of_abc() {
        let token = new_opaque_token().unwrap();
        assert_eq!(token.plaintext.len(), 43);
        assert!(is_opaque_token(&token.plaintext));
        assert!(!is_opaque_token(&token.plaintext[..42]));
        assert!(!is_opaque_token(&format!("{}+", &token.plaintext[..42])));
        assert_eq!(token.hash, hash_token(&token.plaintext));
        assert!(constant_time_eq(&token.hash, &hash_token(&token.plaintext)));
        assert!(!constant_time_eq(&token.hash, "ab"));
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn base62_draws_cover_every_symbol() {
        let mut seen = HashSet::new();
        for _ in 0..62_000 {
            seen.insert(draw_base62().unwrap());
        }
        assert_eq!(seen.len(), 62);
    }
}
