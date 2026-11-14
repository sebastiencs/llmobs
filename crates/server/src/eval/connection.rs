//! The decrypted-connection cache (D-I-4). Sealed columns are opened once and
//! reused while `updated_at` says the row did not change.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use llmobs_eval::{Adapter, JudgeConnection, StructuredOutput};
use uuid::Uuid;

use crate::crypto::{Aad, CryptoError, SecretBox};
use crate::meta::connections::ConnectionRow;

/// At most this many decrypted connections are kept.
const CACHE_ENTRIES: usize = 256;

/// Decrypted connections keyed by id, holding the `updated_at` they were built
/// from.
#[derive(Default)]
pub(crate) struct ConnectionCache {
    entries: Mutex<HashMap<Uuid, (i64, Arc<JudgeConnection>)>>,
}

impl ConnectionCache {
    /// The decrypted connection for `row`, from the cache or by opening its
    /// sealed columns.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError`] when a sealed column does not open or a
    /// stored value does not parse.
    pub(crate) fn get(
        &self,
        secrets: &SecretBox,
        row: &ConnectionRow,
    ) -> Result<Arc<JudgeConnection>, ConnectionError> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((updated_at, connection)) = entries.get(&row.id)
            && *updated_at == row.updated_at
        {
            return Ok(Arc::clone(connection));
        }
        let connection = Arc::new(build(secrets, row)?);
        if entries.len() >= CACHE_ENTRIES {
            entries.clear();
        }
        entries.insert(row.id, (row.updated_at, Arc::clone(&connection)));
        Ok(connection)
    }

    /// Drops `id`. Called after every update and delete of a connection.
    pub(crate) fn forget(&self, id: Uuid) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }
}

impl fmt::Debug for ConnectionCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("ConnectionCache")
            .field("entries", &count)
            .finish()
    }
}

/// Opens `row`'s sealed columns into a [`JudgeConnection`].
fn build(secrets: &SecretBox, row: &ConnectionRow) -> Result<JudgeConnection, ConnectionError> {
    let aad = |column: &'static str| Aad {
        table: "llm_connections",
        column,
        row_id: &row.id,
    };
    let key = secrets.open(&row.secret_enc, aad("secret_enc"))?;
    // A provider key is header text; anything else cannot be sent.
    let api_key =
        String::from_utf8(key).map_err(|_error| ConnectionError::Corrupt("secret_enc"))?;
    let extra_headers = match &row.extra_headers_enc {
        Some(sealed) => {
            let bytes = secrets.open(sealed, aad("extra_headers_enc"))?;
            let map: BTreeMap<String, String> = serde_json::from_slice(&bytes)
                .map_err(|_error| ConnectionError::Corrupt("extra_headers_enc"))?;
            map.into_iter()
                .map(|(name, value)| (name, llmobs_eval::Secret::new(value)))
                .collect()
        }
        None => Vec::new(),
    };
    Ok(JudgeConnection {
        adapter: Adapter::from_name(&row.adapter)
            .map_err(|_error| ConnectionError::Corrupt("adapter"))?,
        base_url: match &row.base_url {
            Some(base) => {
                Some(url::Url::parse(base).map_err(|_error| ConnectionError::Corrupt("base_url"))?)
            }
            None => None,
        },
        api_key: llmobs_eval::Secret::new(api_key),
        extra_headers,
        structured_output: StructuredOutput::from_name(&row.structured_output)
            .map_err(|_error| ConnectionError::Corrupt("structured_output"))?,
    })
}

/// Why a connection could not be decrypted. No variant holds key material.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectionError {
    /// A sealed column did not open.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// A stored value is not one of the connection's own values.
    #[error("corrupt llm_connections.{0}")]
    Corrupt(&'static str),
}

#[cfg(test)]
mod tests {
    use llmobs_core::ids::OrgId;

    use super::ConnectionCache;
    use crate::crypto::{Aad, SecretBox};
    use crate::meta::connections::ConnectionRow;

    /// A row whose `secret_enc` seals `key`, at `updated_at`.
    fn row(secrets: &SecretBox, id: uuid::Uuid, key: &str, updated_at: i64) -> ConnectionRow {
        let secret_enc = secrets
            .seal(
                key.as_bytes(),
                Aad {
                    table: "llm_connections",
                    column: "secret_enc",
                    row_id: &id,
                },
            )
            .unwrap();
        ConnectionRow {
            id,
            org_id: OrgId::from_uuid(uuid::Uuid::nil()),
            name: "One".to_owned(),
            adapter: "openai".to_owned(),
            base_url: Some("https://provider.example/v1".to_owned()),
            secret_enc,
            api_key_hint: "6789".to_owned(),
            extra_headers_enc: None,
            extra_header_names: Vec::new(),
            default_model: Some("gpt-test".to_owned()),
            structured_output: "native".to_owned(),
            max_concurrency: 4,
            requests_per_minute: 300,
            status: "ok".to_owned(),
            last_error: None,
            last_error_at: None,
            created_by: None,
            created_at: 1_000,
            updated_at,
        }
    }

    #[test]
    fn cache_follows_updates() {
        let secrets = SecretBox::new(&[7; 32]);
        let cache = ConnectionCache::default();
        let id = uuid::Uuid::now_v7();
        let first = row(&secrets, id, "sk-first-0123456789", 1_000);
        assert_eq!(
            cache.get(&secrets, &first).unwrap().api_key.expose(),
            "sk-first-0123456789"
        );

        // Same id and `updated_at`: the cached entry wins over the new sealed key.
        let resealed = row(&secrets, id, "sk-second-0123456789", 1_000);
        assert_eq!(
            cache.get(&secrets, &resealed).unwrap().api_key.expose(),
            "sk-first-0123456789"
        );

        cache.forget(id);
        assert_eq!(
            cache.get(&secrets, &resealed).unwrap().api_key.expose(),
            "sk-second-0123456789"
        );
    }
}
