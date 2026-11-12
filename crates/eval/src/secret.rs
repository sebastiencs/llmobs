//! A decrypted secret held in memory.
//!
//! Callers build this from a value the server already decrypted. The type does
//! not serialize, and [`Debug`] prints no characters of the secret.

use std::fmt;

/// A secret string whose [`Debug`] output is `Secret([redacted])`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a decrypted secret.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrows the secret.
    ///
    /// Do not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn debug_redacts_the_value() {
        let secret = Secret::new("sk-live-123".to_owned());
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret([redacted])");
        assert!(!rendered.contains("sk-live"));
        assert_eq!(secret.expose(), "sk-live-123");
    }
}
