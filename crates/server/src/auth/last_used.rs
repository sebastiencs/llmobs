//! Coalesced `last_used_at` updates (D140).
//!
//! [`LastUsed::record`] keeps the latest timestamp per id in memory.
//! [`LastUsed::flush`] writes that map and clears it. `llmobs serve` flushes
//! every 60 s and once at shutdown.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

use crate::meta::MetaDb;
use crate::meta::MetaError;
use crate::meta::api_keys::set_last_used;

/// In-memory `last_used_at` updates, one timestamp per key.
pub(crate) struct LastUsed {
    times: Mutex<HashMap<Uuid, i64>>,
}

impl LastUsed {
    pub(crate) fn new() -> Self {
        Self {
            times: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, i64>> {
        self.times
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Remembers `at` when it is later than the timestamp already stored for `id`.
    pub(crate) fn record(&self, id: Uuid, at: i64) {
        let mut times = self.lock();
        times
            .entry(id)
            .and_modify(|current| *current = (*current).max(at))
            .or_insert(at);
    }

    /// Writes the remembered timestamps and clears them.
    ///
    /// A failed write puts the timestamps back. A timestamp recorded during the
    /// write is kept when it is later.
    ///
    /// # Errors
    ///
    /// Returns [`MetaError`] when the write fails.
    pub(crate) async fn flush(&self, meta: &MetaDb) -> Result<(), MetaError> {
        let rows = {
            let mut times = self.lock();
            if times.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut *times).into_iter().collect::<Vec<_>>()
        };
        let saved = rows.clone();
        let written = meta
            .write(move |conn| Box::pin(async move { set_last_used(conn, &rows).await }))
            .await;
        if let Err(error) = written {
            let mut times = self.lock();
            for (id, at) in saved {
                times
                    .entry(id)
                    .and_modify(|current| *current = (*current).max(at))
                    .or_insert(at);
            }
            return Err(error);
        }
        Ok(())
    }
}
