//! Media blobs extracted from span payloads.
//!
//! Normalization (`llmobs-otel`) produces them and storage (`llmobs-storage`) stores them.
//! Spec 01 § Media blob and spec 02 § Media extraction.

use std::fmt;

/// One extracted media blob, content-addressed by its SHA-256.
///
/// The producer guarantees that `sha256` is the lowercase hex SHA-256 of `bytes` (64
/// characters). Payloads reference the blob as `media://<sha256>`.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaBlob {
    /// Lowercase hex SHA-256 of `bytes`.
    pub sha256: String,
    /// MIME type, declared by the payload or sniffed from the bytes.
    pub mime_type: String,
    /// Decoded content.
    pub bytes: Vec<u8>,
}

/// Prints the hash, the MIME type and the size, never the content.
impl fmt::Debug for MediaBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaBlob")
            .field("sha256", &self.sha256)
            .field("mime_type", &self.mime_type)
            .field("size_bytes", &self.bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_size_not_content() {
        let blob = MediaBlob {
            sha256: "ab".repeat(32),
            mime_type: "image/png".to_owned(),
            bytes: b"SECRET-PAYLOAD".to_vec(),
        };
        let printed = format!("{blob:?}");
        assert!(!printed.contains("SECRET"), "{printed}");
        assert!(printed.contains("size_bytes: 14"), "{printed}");
        assert!(printed.contains("image/png"), "{printed}");
    }
}
