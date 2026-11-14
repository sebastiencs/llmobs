//! `instance_settings` rows (spec 04): one text value per key.

use sqlx::{SqliteConnection, SqlitePool};

use super::MetaError;

/// Key of the encryption check value (D-I-2).
pub(crate) const ENCRYPTION_CHECK: &str = "encryption.check";

/// The value of `key`, or `None`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, MetaError> {
    sqlx::query_scalar::<_, String>("SELECT value FROM instance_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(MetaError::from)
}

/// The same read on the writer, inside a transaction.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn get_setting_tx(
    conn: &mut SqliteConnection,
    key: &str,
) -> Result<Option<String>, MetaError> {
    sqlx::query_scalar::<_, String>("SELECT value FROM instance_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(MetaError::from)
}

/// Inserts or replaces `key`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the write fails.
pub(crate) async fn put_setting(
    conn: &mut SqliteConnection,
    key: &str,
    value: &str,
) -> Result<(), MetaError> {
    sqlx::query(
        "INSERT INTO instance_settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_on_an_empty_table_is_none() {
        let (_dir, db) = crate::meta::test_db().await;
        assert_eq!(
            get_setting(db.reader(), ENCRYPTION_CHECK).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn put_then_get_round_trips() {
        let (_dir, db) = crate::meta::test_db().await;
        db.write(|conn| Box::pin(async move { put_setting(conn, ENCRYPTION_CHECK, "abc").await }))
            .await
            .unwrap();
        assert_eq!(
            get_setting(db.reader(), ENCRYPTION_CHECK).await.unwrap(),
            Some("abc".to_owned())
        );
    }

    #[tokio::test]
    async fn put_twice_keeps_the_second_value() {
        let (_dir, db) = crate::meta::test_db().await;
        db.write(|conn| {
            Box::pin(async move {
                put_setting(conn, "k", "first").await?;
                put_setting(conn, "k", "second").await
            })
        })
        .await
        .unwrap();
        assert_eq!(
            get_setting(db.reader(), "k").await.unwrap(),
            Some("second".to_owned())
        );
    }
}
