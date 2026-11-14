//! `llm_connections` rows (spec 04, D-I-4). Sealed columns are opaque bytes here.

use std::fmt;

use llmobs_core::ids::OrgId;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};

/// An org holds at most this many connections (D-I-4).
const ORG_CAP: i64 = 100;

/// One stored connection. `Debug` is written by hand and prints the sealed
/// columns' lengths only.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ConnectionRow {
    /// UUIDv7, chosen by the caller: the AAD of the sealed columns needs it
    /// before the insert.
    pub(crate) id: Uuid,
    /// Owning org.
    pub(crate) org_id: OrgId,
    /// Display name, unique in the org.
    pub(crate) name: String,
    /// Stored adapter name (`LlmAdapter::as_str`).
    pub(crate) adapter: String,
    /// Provider endpoint, absent when the provider's default is used.
    pub(crate) base_url: Option<String>,
    /// Sealed API key.
    pub(crate) secret_enc: Vec<u8>,
    /// Last 4 characters of the key, or an empty string.
    pub(crate) api_key_hint: String,
    /// Sealed extra headers.
    pub(crate) extra_headers_enc: Option<Vec<u8>>,
    /// Extra header names, lowercase, sorted.
    pub(crate) extra_header_names: Vec<String>,
    /// Model used when a caller names none.
    pub(crate) default_model: Option<String>,
    /// Stored mode name (`StructuredOutputMode::as_str`).
    pub(crate) structured_output: String,
    /// Provider calls the connection runs at once.
    pub(crate) max_concurrency: u32,
    /// Provider calls per minute.
    pub(crate) requests_per_minute: u32,
    /// Stored status name (`ConnectionStatus::as_str`).
    pub(crate) status: String,
    /// Last pause-class error.
    pub(crate) last_error: Option<String>,
    /// When `last_error` was recorded.
    pub(crate) last_error_at: Option<i64>,
    /// User who created the connection.
    pub(crate) created_by: Option<Uuid>,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Last change time, nanoseconds.
    pub(crate) updated_at: i64,
}

impl fmt::Debug for ConnectionRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionRow")
            .field("id", &self.id)
            .field("org_id", &self.org_id)
            .field("name", &self.name)
            .field("adapter", &self.adapter)
            .field("base_url", &self.base_url)
            .field("secret_enc", &self.secret_enc.len())
            .field("api_key_hint", &self.api_key_hint)
            .field(
                "extra_headers_enc",
                &self.extra_headers_enc.as_deref().map(<[u8]>::len),
            )
            .field("extra_header_names", &self.extra_header_names)
            .field("default_model", &self.default_model)
            .field("structured_output", &self.structured_output)
            .field("max_concurrency", &self.max_concurrency)
            .field("requests_per_minute", &self.requests_per_minute)
            .field("status", &self.status)
            .field("last_error", &self.last_error)
            .field("last_error_at", &self.last_error_at)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// The editable columns. Insert takes them with the id, org and author;
/// update replaces them all.
#[derive(Clone)]
pub(crate) struct ConnectionFields {
    /// Display name, unique in the org.
    pub(crate) name: String,
    /// Stored adapter name (`LlmAdapter::as_str`).
    pub(crate) adapter: String,
    /// Provider endpoint, absent when the provider's default is used.
    pub(crate) base_url: Option<String>,
    /// Sealed API key.
    pub(crate) secret_enc: Vec<u8>,
    /// Last 4 characters of the key, or an empty string.
    pub(crate) api_key_hint: String,
    /// Sealed extra headers.
    pub(crate) extra_headers_enc: Option<Vec<u8>>,
    /// Extra header names, lowercase, sorted.
    pub(crate) extra_header_names: Vec<String>,
    /// Model used when a caller names none.
    pub(crate) default_model: Option<String>,
    /// Stored mode name (`StructuredOutputMode::as_str`).
    pub(crate) structured_output: String,
    /// Provider calls the connection runs at once.
    pub(crate) max_concurrency: u32,
    /// Provider calls per minute.
    pub(crate) requests_per_minute: u32,
}

/// Every connection of `org`, ordered by name then id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_connections(
    pool: &SqlitePool,
    org: OrgId,
) -> Result<Vec<ConnectionRow>, MetaError> {
    let records: Vec<ConnectionRecord> = sqlx::query_as(
        "SELECT id, org_id, name, adapter, base_url, secret_enc, api_key_hint, extra_headers_enc, \
         extra_header_names, default_model, structured_output, max_concurrency, \
         requests_per_minute, status, last_error, last_error_at, created_by, created_at, \
         updated_at \
         FROM llm_connections WHERE org_id = ? ORDER BY name, id",
    )
    .bind(org.to_string())
    .fetch_all(pool)
    .await?;
    records.into_iter().map(row_from).collect()
}

/// The connection `id` in `org`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn get_connection(
    pool: &SqlitePool,
    org: OrgId,
    id: Uuid,
) -> Result<Option<ConnectionRow>, MetaError> {
    let record: Option<ConnectionRecord> = sqlx::query_as(
        "SELECT id, org_id, name, adapter, base_url, secret_enc, api_key_hint, extra_headers_enc, \
         extra_header_names, default_model, structured_output, max_concurrency, \
         requests_per_minute, status, last_error, last_error_at, created_by, created_at, \
         updated_at \
         FROM llm_connections WHERE org_id = ? AND id = ?",
    )
    .bind(org.to_string())
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    record.map(row_from).transpose()
}

/// The same read on the writer, inside a transaction.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn get_connection_tx(
    conn: &mut SqliteConnection,
    org: OrgId,
    id: Uuid,
) -> Result<Option<ConnectionRow>, MetaError> {
    let record: Option<ConnectionRecord> = sqlx::query_as(
        "SELECT id, org_id, name, adapter, base_url, secret_enc, api_key_hint, extra_headers_enc, \
         extra_header_names, default_model, structured_output, max_concurrency, \
         requests_per_minute, status, last_error, last_error_at, created_by, created_at, \
         updated_at \
         FROM llm_connections WHERE org_id = ? AND id = ?",
    )
    .bind(org.to_string())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    record.map(row_from).transpose()
}

/// Inserts a connection. `id` is chosen by the caller, because the AAD of the
/// sealed columns needs it before the insert.
///
/// A duplicate name is [`MetaError::Conflict`] `"a connection with this name
/// already exists"`; the org's 101st connection is `"an organization has at
/// most 100 connections"`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails or a stored value does not decode.
pub(crate) async fn insert_connection(
    conn: &mut SqliteConnection,
    id: Uuid,
    org: OrgId,
    fields: ConnectionFields,
    created_by: Option<Uuid>,
    now: i64,
) -> Result<ConnectionRow, MetaError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM llm_connections WHERE org_id = ?")
        .bind(org.to_string())
        .fetch_one(&mut *conn)
        .await?;
    if count >= ORG_CAP {
        return Err(MetaError::Conflict(
            "an organization has at most 100 connections",
        ));
    }
    let names = names_json(&fields.extra_header_names)?;
    sqlx::query(
        "INSERT INTO llm_connections \
         (id, org_id, name, adapter, base_url, secret_enc, api_key_hint, extra_headers_enc, \
         extra_header_names, default_model, structured_output, max_concurrency, \
         requests_per_minute, status, last_error, last_error_at, created_by, created_at, \
         updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'ok', NULL, NULL, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(org.to_string())
    .bind(fields.name)
    .bind(fields.adapter)
    .bind(fields.base_url)
    .bind(fields.secret_enc)
    .bind(fields.api_key_hint)
    .bind(fields.extra_headers_enc)
    .bind(names)
    .bind(fields.default_model)
    .bind(fields.structured_output)
    .bind(i64::from(fields.max_concurrency))
    .bind(i64::from(fields.requests_per_minute))
    .bind(created_by.map(|user| user.to_string()))
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "a connection with this name already exists"))?;
    get_connection_tx(conn, org, id)
        .await?
        .ok_or(MetaError::Corrupt {
            table: "llm_connections",
            column: "id",
        })
}

/// Replaces every editable column and bumps `updated_at`. `None` means the
/// connection is missing or belongs to another org.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails or a stored value does not decode.
pub(crate) async fn update_connection(
    conn: &mut SqliteConnection,
    org: OrgId,
    id: Uuid,
    fields: ConnectionFields,
    now: i64,
) -> Result<Option<ConnectionRow>, MetaError> {
    let names = names_json(&fields.extra_header_names)?;
    let result = sqlx::query(
        "UPDATE llm_connections SET name = ?, adapter = ?, base_url = ?, secret_enc = ?, \
         api_key_hint = ?, extra_headers_enc = ?, extra_header_names = ?, default_model = ?, \
         structured_output = ?, max_concurrency = ?, requests_per_minute = ?, updated_at = ? \
         WHERE org_id = ? AND id = ?",
    )
    .bind(fields.name)
    .bind(fields.adapter)
    .bind(fields.base_url)
    .bind(fields.secret_enc)
    .bind(fields.api_key_hint)
    .bind(fields.extra_headers_enc)
    .bind(names)
    .bind(fields.default_model)
    .bind(fields.structured_output)
    .bind(i64::from(fields.max_concurrency))
    .bind(i64::from(fields.requests_per_minute))
    .bind(now)
    .bind(org.to_string())
    .bind(id.to_string())
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "a connection with this name already exists"))?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_connection_tx(conn, org, id)
        .await?
        .ok_or(MetaError::Corrupt {
            table: "llm_connections",
            column: "id",
        })
        .map(Some)
}

/// Deletes the connection. `false` means it is missing or belongs to another org.
///
/// # Errors
///
/// Returns [`MetaError`] when the delete fails.
pub(crate) async fn delete_connection(
    conn: &mut SqliteConnection,
    org: OrgId,
    id: Uuid,
) -> Result<bool, MetaError> {
    let result = sqlx::query("DELETE FROM llm_connections WHERE org_id = ? AND id = ?")
        .bind(org.to_string())
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Sets `status`, `last_error` and `last_error_at` (`None` clears both),
/// without touching `updated_at`.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails.
pub(crate) async fn set_connection_status(
    conn: &mut SqliteConnection,
    id: Uuid,
    status: &'static str,
    error: Option<&str>,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query(
        "UPDATE llm_connections SET status = ?, last_error = ?, last_error_at = ? WHERE id = ?",
    )
    .bind(status)
    .bind(error)
    .bind(error.is_some().then_some(now))
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct ConnectionRecord {
    id: String,
    org_id: String,
    name: String,
    adapter: String,
    base_url: Option<String>,
    secret_enc: Vec<u8>,
    api_key_hint: String,
    extra_headers_enc: Option<Vec<u8>>,
    extra_header_names: String,
    default_model: Option<String>,
    structured_output: String,
    max_concurrency: i64,
    requests_per_minute: i64,
    status: String,
    last_error: Option<String>,
    last_error_at: Option<i64>,
    created_by: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn row_from(record: ConnectionRecord) -> Result<ConnectionRow, MetaError> {
    Ok(ConnectionRow {
        id: parse_uuid(&record.id, "id")?,
        org_id: OrgId::from_uuid(parse_uuid(&record.org_id, "org_id")?),
        name: record.name,
        adapter: record.adapter,
        base_url: record.base_url,
        secret_enc: record.secret_enc,
        api_key_hint: record.api_key_hint,
        extra_headers_enc: record.extra_headers_enc,
        extra_header_names: serde_json::from_str(&record.extra_header_names).map_err(|_| {
            MetaError::Corrupt {
                table: "llm_connections",
                column: "extra_header_names",
            }
        })?,
        default_model: record.default_model,
        structured_output: record.structured_output,
        max_concurrency: u32::try_from(record.max_concurrency).map_err(|_| MetaError::Corrupt {
            table: "llm_connections",
            column: "max_concurrency",
        })?,
        requests_per_minute: u32::try_from(record.requests_per_minute).map_err(|_| {
            MetaError::Corrupt {
                table: "llm_connections",
                column: "requests_per_minute",
            }
        })?,
        status: record.status,
        last_error: record.last_error,
        last_error_at: record.last_error_at,
        created_by: match record.created_by {
            Some(value) => Some(parse_uuid(&value, "created_by")?),
            None => None,
        },
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

fn parse_uuid(value: &str, column: &'static str) -> Result<Uuid, MetaError> {
    value.parse().map_err(|_| MetaError::Corrupt {
        table: "llm_connections",
        column,
    })
}

fn names_json(names: &[String]) -> Result<String, MetaError> {
    serde_json::to_string(names).map_err(|_| MetaError::Corrupt {
        table: "llm_connections",
        column: "extra_header_names",
    })
}

#[cfg(test)]
mod tests {
    use llmobs_core::ids::OrgId;
    use uuid::Uuid;

    use super::{ConnectionFields, ConnectionRow};
    use crate::meta::MetaDb;
    use crate::meta::MetaError;
    use crate::meta::connections::{
        delete_connection, get_connection, insert_connection, list_connections,
        set_connection_status, update_connection,
    };
    use crate::meta::orgs::insert_org;
    use crate::meta::users::{NewUser, insert_user};

    const NOW: i64 = 1_000;

    fn fields(name: &str) -> ConnectionFields {
        ConnectionFields {
            name: name.to_owned(),
            adapter: "openai".to_owned(),
            base_url: Some("https://api.openai.com/v1".to_owned()),
            secret_enc: vec![1, 2, 3],
            api_key_hint: "WXYZ".to_owned(),
            extra_headers_enc: Some(vec![9, 9, 9]),
            extra_header_names: vec!["x-tenant".to_owned()],
            default_model: Some("gpt-5-mini".to_owned()),
            structured_output: "native".to_owned(),
            max_concurrency: 4,
            requests_per_minute: 300,
        }
    }

    async fn org(db: &MetaDb, slug: &str) -> OrgId {
        let slug = slug.to_owned();
        db.write(move |conn| Box::pin(async move { insert_org(conn, "Acme", &slug, NOW).await }))
            .await
            .unwrap()
            .id
    }

    async fn insert(
        db: &MetaDb,
        org: OrgId,
        name: &str,
        created_by: Option<Uuid>,
    ) -> ConnectionRow {
        let name = name.to_owned();
        db.write(move |conn| {
            Box::pin(async move {
                insert_connection(conn, Uuid::now_v7(), org, fields(&name), created_by, NOW).await
            })
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn insert_list_get_round_trip() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        let other = org(&db, "other").await;
        let author = db
            .write(|conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email: "ada@example.com".to_owned(),
                            name: None,
                            password_hash: None,
                            email_verified_at: None,
                            is_instance_admin: false,
                        },
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap();

        let b = insert(&db, acme, "b", None).await;
        let a = insert(&db, acme, "a", Some(author.id)).await;

        let listed = list_connections(db.reader(), acme).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        for row in [&a, &b] {
            let read = get_connection(db.reader(), acme, row.id)
                .await
                .unwrap()
                .unwrap();
            let expected = ConnectionRow {
                id: row.id,
                org_id: acme,
                name: row.name.clone(),
                adapter: "openai".to_owned(),
                base_url: Some("https://api.openai.com/v1".to_owned()),
                secret_enc: vec![1, 2, 3],
                api_key_hint: "WXYZ".to_owned(),
                extra_headers_enc: Some(vec![9, 9, 9]),
                extra_header_names: vec!["x-tenant".to_owned()],
                default_model: Some("gpt-5-mini".to_owned()),
                structured_output: "native".to_owned(),
                max_concurrency: 4,
                requests_per_minute: 300,
                status: "ok".to_owned(),
                last_error: None,
                last_error_at: None,
                created_by: row.created_by,
                created_at: NOW,
                updated_at: NOW,
            };
            assert_eq!(read, expected);
        }
        assert_eq!(a.created_by, Some(author.id));
        assert_eq!(b.created_by, None);

        assert!(
            get_connection(db.reader(), other, a.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn duplicate_name_is_a_conflict() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        insert(&db, acme, "same", None).await;
        let error = db
            .write(|conn| {
                Box::pin(async move {
                    insert_connection(conn, Uuid::now_v7(), acme, fields("same"), None, NOW).await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("a connection with this name already exists")
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn org_cap_is_100() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        db.write(|conn| {
            Box::pin(async move {
                for index in 0..100 {
                    insert_connection(
                        conn,
                        Uuid::now_v7(),
                        acme,
                        fields(&format!("c-{index:03}")),
                        None,
                        NOW,
                    )
                    .await?;
                }
                Ok(())
            })
        })
        .await
        .unwrap();
        let error = db
            .write(|conn| {
                Box::pin(async move {
                    insert_connection(
                        conn,
                        Uuid::now_v7(),
                        acme,
                        fields("one-too-many"),
                        None,
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("an organization has at most 100 connections")
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn update_replaces_columns_and_bumps_updated_at() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        let row = insert(&db, acme, "before", None).await;

        let later = NOW + 500;
        let updated = db
            .write(move |conn| {
                Box::pin(async move {
                    update_connection(
                        conn,
                        acme,
                        row.id,
                        ConnectionFields {
                            name: "after".to_owned(),
                            adapter: "anthropic".to_owned(),
                            base_url: None,
                            secret_enc: vec![7],
                            api_key_hint: String::new(),
                            extra_headers_enc: None,
                            extra_header_names: Vec::new(),
                            default_model: None,
                            structured_output: "tool".to_owned(),
                            max_concurrency: 8,
                            requests_per_minute: 60,
                        },
                        later,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "after");
        assert_eq!(updated.adapter, "anthropic");
        assert_eq!(updated.base_url, None);
        assert_eq!(updated.secret_enc, vec![7]);
        assert_eq!(updated.api_key_hint, "");
        assert_eq!(updated.extra_headers_enc, None);
        assert_eq!(updated.extra_header_names, Vec::<String>::new());
        assert_eq!(updated.default_model, None);
        assert_eq!(updated.structured_output, "tool");
        assert_eq!(updated.max_concurrency, 8);
        assert_eq!(updated.requests_per_minute, 60);
        assert_eq!(updated.created_at, NOW);
        assert_eq!(updated.updated_at, later);

        let unknown = db
            .write(|conn| {
                Box::pin(async move {
                    update_connection(conn, acme, Uuid::now_v7(), fields("x"), later).await
                })
            })
            .await
            .unwrap();
        assert!(unknown.is_none());
    }

    #[tokio::test]
    async fn delete_reports_whether_a_row_went() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        let row = insert(&db, acme, "gone", None).await;
        let went = db
            .write(move |conn| Box::pin(async move { delete_connection(conn, acme, row.id).await }))
            .await
            .unwrap();
        assert!(went);
        let again = db
            .write(|conn| Box::pin(async move { delete_connection(conn, acme, row.id).await }))
            .await
            .unwrap();
        assert!(!again);
    }

    #[tokio::test]
    async fn set_status_keeps_updated_at() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        let row = insert(&db, acme, "watched", None).await;

        let failed_at = NOW + 50;
        db.write({
            let id = row.id;
            move |conn| {
                Box::pin(async move {
                    set_connection_status(
                        conn,
                        id,
                        "failing",
                        Some("provider refused the key"),
                        failed_at,
                    )
                    .await
                })
            }
        })
        .await
        .unwrap();
        let failing = get_connection(db.reader(), acme, row.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failing.status, "failing");
        assert_eq!(
            failing.last_error.as_deref(),
            Some("provider refused the key")
        );
        assert_eq!(failing.last_error_at, Some(failed_at));
        assert_eq!(failing.updated_at, NOW);

        db.write({
            let id = row.id;
            move |conn| {
                Box::pin(async move { set_connection_status(conn, id, "ok", None, NOW + 60).await })
            }
        })
        .await
        .unwrap();
        let ok = get_connection(db.reader(), acme, row.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ok.status, "ok");
        assert_eq!(ok.last_error, None);
        assert_eq!(ok.last_error_at, None);
        assert_eq!(ok.updated_at, NOW);
    }

    #[tokio::test]
    async fn org_delete_cascades() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        let row = insert(&db, acme, "cascade", None).await;
        db.write({
            let org_id = acme.to_string();
            move |conn| {
                Box::pin(async move {
                    sqlx::query("DELETE FROM orgs WHERE id = ?")
                        .bind(org_id)
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            }
        })
        .await
        .unwrap();
        assert!(
            get_connection(db.reader(), acme, row.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn debug_hides_sealed_bytes() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db, "acme").await;
        let mut row = insert(&db, acme, "opaque", None).await;
        row.secret_enc = vec![200, 201, 202];
        let text = format!("{row:?}");
        assert!(!text.contains("200, 201"), "{text}");
        assert!(text.contains("secret_enc: 3"), "{text}");
    }
}
