//! Password policy and argon2id (spec 08, D143).
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashSet;
use std::num::NonZero;
use std::sync::{Arc, LazyLock};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use unicode_normalization::UnicodeNormalization;

use super::AuthError;
use super::secret::random_bytes;

/// Memory cost in KiB, iterations, and lanes (D143).
const M_KIB: u32 = 19_456;
const T_COST: u32 = 2;
const P_COST: u32 = 1;
const SALT_LEN: usize = 16;

/// Why a password was rejected. The message does not contain the password or the email.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PolicyError {
    /// Fewer than 10 Unicode scalar values.
    #[error("password is too short")]
    TooShort,
    /// More than 128 Unicode scalar values.
    #[error("password is too long")]
    TooLong,
    /// Equal to the email, ignoring case.
    #[error("password matches the email")]
    EqualsEmail,
    /// Present in the embedded common-password list.
    #[error("password is too common")]
    TooCommon,
}

/// The result of checking a password against a stored PHC string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Verified {
    /// The password matches the hash.
    pub(crate) ok: bool,
    /// The stored parameters are weaker than the current ones, or the algorithm is not argon2id.
    pub(crate) needs_rehash: bool,
}

/// argon2id behind a semaphore. `dummy` is hashed once so an unknown email costs the same.
pub(crate) struct Hasher {
    permits: Arc<Semaphore>,
    dummy: String,
}

impl Hasher {
    /// Builds the hasher and the dummy PHC string.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] when the dummy hash cannot be computed.
    pub(crate) async fn new() -> Result<Self, AuthError> {
        let cores = std::thread::available_parallelism()
            .map(NonZero::get)
            .unwrap_or(1);
        Self::with_permits((cores / 2).max(2)).await
    }

    async fn with_permits(permits: usize) -> Result<Self, AuthError> {
        let dummy =
            tokio::task::spawn_blocking(|| hash_password(b"llmobs dummy password verifier"))
                .await
                .map_err(|_| AuthError::PasswordHash)??;
        Ok(Self {
            permits: Arc::new(Semaphore::new(permits)),
            dummy,
        })
    }

    /// NFC-normalizes `password` and returns its PHC string.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] when the random salt or argon2 fails.
    pub(crate) async fn hash(&self, password: &str) -> Result<String, AuthError> {
        let password = nfc(password);
        let permit = self.permit().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hash_password(password.as_bytes())
        })
        .await
        .map_err(|_| AuthError::PasswordHash)?
    }

    /// Checks `password` against `phc`.
    ///
    /// `None` verifies against the dummy hash and returns `ok: false`. A PHC string
    /// that does not parse also returns `ok: false`. This function does not log.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] when argon2 fails for a reason other than a mismatch
    /// or an unreadable hash.
    pub(crate) async fn verify(
        &self,
        password: &str,
        phc: Option<&str>,
    ) -> Result<Verified, AuthError> {
        let password = nfc(password);
        let unknown = phc.is_none();
        let phc = phc.unwrap_or(&self.dummy).to_owned();
        let permit = self.permit().await?;
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            verify_password(password.as_bytes(), &phc)
        })
        .await
        .map_err(|_| AuthError::PasswordHash)??;
        if unknown {
            Ok(Verified {
                ok: false,
                needs_rehash: false,
            })
        } else {
            Ok(verified)
        }
    }

    async fn permit(&self) -> Result<OwnedSemaphorePermit, AuthError> {
        // The semaphore is never closed, so a closed-semaphore error does not happen
        // in this process. Map it anyway: a panic would abort a login.
        self.permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthError::PasswordHash)
    }
}

/// NFC, then 10–128 scalar values, not the email, not in the common list.
///
/// # Errors
///
/// Returns the first failing rule.
pub(crate) fn check_policy(password: &str, normalized_email: &str) -> Result<(), PolicyError> {
    let composed = nfc(password);
    let length = composed.chars().count();
    if length < 10 {
        return Err(PolicyError::TooShort);
    }
    if length > 128 {
        return Err(PolicyError::TooLong);
    }
    let lower = composed.to_lowercase();
    if lower == normalized_email.to_lowercase() {
        return Err(PolicyError::EqualsEmail);
    }
    if common_passwords().contains(lower.as_str()) {
        return Err(PolicyError::TooCommon);
    }
    Ok(())
}

fn nfc(text: &str) -> String {
    text.nfc().collect()
}

fn common_passwords() -> &'static HashSet<&'static str> {
    static COMMON: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
        include_str!("common-passwords.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect()
    });
    &COMMON
}

fn argon2() -> Result<Argon2<'static>, AuthError> {
    // 19 456 KiB, t = 2, p = 1 is inside argon2's accepted range (D143).
    let params = Params::new(M_KIB, T_COST, P_COST, None).map_err(|_| AuthError::PasswordHash)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

fn hash_password(password: &[u8]) -> Result<String, AuthError> {
    let salt = random_bytes(SALT_LEN)?;
    let hash = argon2()?
        .hash_password_with_salt(password, &salt)
        .map_err(|_| AuthError::PasswordHash)?;
    Ok(hash.to_string())
}

fn verify_password(password: &[u8], phc: &str) -> Result<Verified, AuthError> {
    if PasswordHash::new(phc).is_err() {
        return Ok(Verified {
            ok: false,
            needs_rehash: false,
        });
    }
    let needs_rehash = needs_rehash(phc);
    match argon2()?.verify_password(password, phc) {
        Ok(()) => Ok(Verified {
            ok: true,
            needs_rehash,
        }),
        Err(argon2::password_hash::Error::PasswordInvalid) => Ok(Verified {
            ok: false,
            needs_rehash: false,
        }),
        Err(_) => Err(AuthError::PasswordHash),
    }
}

/// Weaker memory, time, or lanes, a version other than 19, or not argon2id.
fn needs_rehash(phc: &str) -> bool {
    let mut parts = phc.split('$');
    let _ = parts.next();
    let algorithm = parts.next();
    let version = parts.next();
    let params = parts.next().unwrap_or("");
    if algorithm != Some("argon2id") || version != Some("v=19") {
        return true;
    }
    let mut memory = None;
    let mut time = None;
    let mut lanes = None;
    for item in params.split(',') {
        let Some((key, value)) = item.split_once('=') else {
            return true;
        };
        let Ok(value) = value.parse::<u32>() else {
            return true;
        };
        match key {
            "m" => memory = Some(value),
            "t" => time = Some(value),
            "p" => lanes = Some(value),
            _ => {}
        }
    }
    memory.is_none_or(|memory| memory < M_KIB)
        || time.is_none_or(|time| time < T_COST)
        || lanes.is_none_or(|lanes| lanes < P_COST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_counts_scalar_values_and_rejects_the_list() {
        assert_eq!(
            check_policy("too-short", "ada@example.com"),
            Err(PolicyError::TooShort)
        );
        assert!(check_policy("llmobs-ok1", "ada@example.com").is_ok());
        assert!(check_policy(&"é".repeat(10), "ada@example.com").is_ok());
        assert!(check_policy(&"e\u{301}".repeat(10), "ada@example.com").is_ok());
        assert!(check_policy(&"a".repeat(128), "ada@example.com").is_ok());
        assert_eq!(
            check_policy(&"a".repeat(129), "ada@example.com"),
            Err(PolicyError::TooLong)
        );
        assert_eq!(
            check_policy("Ada@Example.COM", "ada@example.com"),
            Err(PolicyError::EqualsEmail)
        );
        let error = check_policy("ABSOLUTELY", "ada@example.com").unwrap_err();
        assert_eq!(error, PolicyError::TooCommon);
        assert!(!error.to_string().contains("ABSOLUTELY"));
        assert!(!error.to_string().contains("ada@"));
        assert_eq!(common_passwords().len(), 51);
    }

    #[tokio::test]
    async fn hashes_round_trip_and_the_dummy_uses_current_params() {
        let hasher = Hasher::new().await.unwrap();
        assert!(hasher.dummy.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        PasswordHash::new(&hasher.dummy).unwrap();

        let phc = hasher.hash("e\u{301}password").await.unwrap();
        let verified = hasher.verify("épassword", Some(&phc)).await.unwrap();
        assert!(verified.ok);
        assert!(!verified.needs_rehash);

        let wrong = hasher.verify("other-password", Some(&phc)).await.unwrap();
        assert!(!wrong.ok);
        assert!(!wrong.needs_rehash);

        let unknown = hasher.verify("e\u{301}password", None).await.unwrap();
        assert!(!unknown.ok);
        assert!(!unknown.needs_rehash);

        let weak = hash_with_time(1, "e\u{301}password");
        let rehash = hasher.verify("épassword", Some(&weak)).await.unwrap();
        assert!(rehash.ok);
        assert!(rehash.needs_rehash);
    }

    #[tokio::test]
    async fn eight_hashes_share_two_permits() {
        let hasher = Hasher::with_permits(2).await.unwrap();
        let mut tasks = Vec::new();
        for index in 0..8 {
            let hasher_permits = Arc::clone(&hasher.permits);
            let dummy = hasher.dummy.clone();
            tasks.push(tokio::spawn(async move {
                let limited = Hasher {
                    permits: hasher_permits,
                    dummy,
                };
                limited.hash(&format!("llmobs-ok{index}")).await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
    }

    fn hash_with_time(time: u32, password: &str) -> String {
        let params = Params::new(M_KIB, time, P_COST, None).unwrap();
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let salt = random_bytes(SALT_LEN).unwrap();
        argon2
            .hash_password_with_salt(nfc(password).as_bytes(), &salt)
            .unwrap()
            .to_string()
    }
}
