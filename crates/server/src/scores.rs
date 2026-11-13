//! Score write core: validation, ids, times and append (spec 06 § Scores; D-G-26 to D-G-28).
//!
//! Every write path ends in [`write`] or [`delete`]. The score routes call them (G3.4), and
//! public feedback calls [`write`] (G3.5).

use std::collections::HashMap;

use llmobs_core::enums::ScoreSource;
use llmobs_core::ids::ProjectId;
use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::api::common::FieldError;
use crate::api::convert;
use crate::api::scores::ScoreCreate;
use crate::http::AppState;
use crate::http::error::ApiError;
use crate::ingest::IngestError;
use crate::meta::projects::ProjectRow;
use crate::routes::read::{READ_TIMEOUT, open_query};

/// Most scores one request writes.
const MAX_SCORES: usize = 1000;
/// Longest name, session id and categorical label, in bytes.
const TEXT_MAX: usize = 255;
const HOUR_NS: i64 = 3_600_000_000_000;
const DAY_NS: i64 = 86_400_000_000_000;
/// Prefix of the derived-id input (spec 06).
const DERIVE_TAG: &[u8] = b"llmobs.score.v1";

/// Who is writing, which decides `source` and the author (D-G-27).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Writer {
    /// A web session on the project route.
    Human { user_id: Uuid },
    /// A PAT (`token_id`) or `sk` key (`key_id`) on the project route.
    Api { author: Uuid },
    /// An `sk` key on `POST /api/v1/scores`.
    Sdk { key_id: Uuid },
    /// A `pk` key on `POST /api/public/feedback`.
    EndUser { key_id: Uuid },
}

impl Writer {
    /// The stored `source` for a body `source`, or `None` when the body may not set it here.
    fn source(self, requested: Option<ScoreSource>) -> Option<ScoreSource> {
        match (self, requested) {
            (Self::Human { .. }, None | Some(ScoreSource::Human)) => Some(ScoreSource::Human),
            (Self::Api { .. }, None | Some(ScoreSource::Api)) => Some(ScoreSource::Api),
            (Self::Api { .. }, Some(ScoreSource::Sdk)) => Some(ScoreSource::Sdk),
            (Self::Sdk { .. }, None | Some(ScoreSource::Sdk)) => Some(ScoreSource::Sdk),
            (Self::EndUser { .. }, None | Some(ScoreSource::EndUser)) => Some(ScoreSource::EndUser),
            _ => None,
        }
    }

    /// The id a derived `score_id` uses as its author (D-G-27).
    fn author(self) -> Uuid {
        match self {
            Self::Human { user_id } => user_id,
            Self::Api { author } => author,
            Self::Sdk { key_id } | Self::EndUser { key_id } => key_id,
        }
    }

    /// `author_user_id` of the stored row. Only a person has one.
    fn author_user(self) -> Option<Uuid> {
        match self {
            Self::Human { user_id } => Some(user_id),
            Self::Api { .. } | Self::Sdk { .. } | Self::EndUser { .. } => None,
        }
    }
}

/// Validates `items`, resolves ids and times, appends the rows, and returns one id per item
/// in input order (spec 06 § Score write rules).
pub(crate) async fn write(
    state: &AppState,
    project: &ProjectRow,
    writer: Writer,
    items: Vec<ScoreCreate>,
) -> Result<Vec<Uuid>, ApiError> {
    let now = state.clock.now_ns();
    let mut errors = Vec::new();
    if !(1..=MAX_SCORES).contains(&items.len()) {
        errors.push(FieldError {
            field: "scores".to_owned(),
            message: "between 1 and 1000 scores per request".to_owned(),
            position: None,
        });
    }
    let checked: Vec<Option<ScoreSource>> = items
        .iter()
        .enumerate()
        .map(|(index, item)| check_item(index, item, writer, project, now, &mut errors))
        .collect();
    if !errors.is_empty() {
        return Err(invalid(errors));
    }
    // With no errors every item has a source, so nothing is dropped here.
    let sources: Vec<ScoreSource> = checked.into_iter().flatten().collect();

    let author = writer.author();
    let pending: Vec<Pending> = items
        .into_iter()
        .zip(sources)
        .map(|(item, source)| {
            let id = item
                .score_id
                .unwrap_or_else(|| derive_id(project.id, &item.target, &item.name, source, author));
            Pending { id, source, item }
        })
        .collect();

    // Answer: one id per input item, in input order.
    let ids: Vec<Uuid> = pending.iter().map(|pending| pending.id).collect();
    // Duplicates keep the last item (D-G-28).
    let last: HashMap<Uuid, usize> = pending
        .iter()
        .enumerate()
        .map(|(index, pending)| (pending.id, index))
        .collect();
    let kept: Vec<Pending> = pending
        .into_iter()
        .enumerate()
        .filter_map(|(index, pending)| (last.get(&pending.id) == Some(&index)).then_some(pending))
        .collect();

    let lookup: Vec<String> = kept
        .iter()
        .filter(|pending| !is_v7(pending.id))
        .map(|pending| pending.id.to_string())
        .collect();
    let live = live_scores(state, project.id, &lookup).await?;

    let author_user = writer.author_user();
    let rows: Vec<Score> = kept
        .into_iter()
        .map(|pending| {
            let (created_at, updated_at) = times(pending.id, now, &live);
            let Pending { id, source, item } = pending;
            let ScoreCreate {
                target,
                session_id,
                user_id,
                name,
                value,
                reason,
                metadata,
                ..
            } = item;
            let session_id = match target {
                ScoreTarget::Session { .. } => None,
                ScoreTarget::Span { .. } | ScoreTarget::Trace { .. } => session_id,
            };
            Score {
                project_id: project.id,
                score_id: id,
                target,
                session_id,
                user_id,
                name,
                value,
                reason,
                source,
                evaluator_id: None,
                rule_id: None,
                experiment_id: None,
                dataset_item_id: None,
                author_user_id: author_user,
                metadata,
                created_at,
                updated_at,
                deleted: false,
                ingest_time: now,
            }
        })
        .collect();

    append(state, project.id, &rows).await?;
    state.usage.add(
        project.org_id,
        project.id,
        now,
        0,
        0,
        u64::try_from(rows.len()).unwrap_or(u64::MAX),
    );
    Ok(ids)
}

/// Appends a deleted copy of the live score `score_id` (spec 03 §5). `NotFound` when none.
pub(crate) async fn delete(
    state: &AppState,
    project: &ProjectRow,
    score_id: Uuid,
) -> Result<(), ApiError> {
    let now = state.clock.now_ns();
    let mut live = live_scores(state, project.id, &[score_id.to_string()]).await?;
    let Some(mut tombstone) = live.remove(&score_id) else {
        return Err(ApiError::NotFound);
    };
    tombstone.deleted = true;
    tombstone.updated_at = now.max(tombstone.updated_at.saturating_add(1));
    tombstone.ingest_time = now;
    append(state, project.id, std::slice::from_ref(&tombstone)).await
}

/// An item that passed the checks, with its final id.
struct Pending {
    id: Uuid,
    source: ScoreSource,
    item: ScoreCreate,
}

fn invalid(errors: Vec<FieldError>) -> ApiError {
    ApiError::BadRequest {
        detail: Some("invalid scores".to_owned()),
        errors,
    }
}

fn field(index: usize, name: &str, message: &str) -> FieldError {
    FieldError {
        field: format!("scores[{index}].{name}"),
        message: message.to_owned(),
        position: None,
    }
}

fn text_ok(text: &str) -> bool {
    (1..=TEXT_MAX).contains(&text.len())
}

/// Pushes every field error of one item. Returns its stored source when it has none.
fn check_item(
    index: usize,
    item: &ScoreCreate,
    writer: Writer,
    project: &ProjectRow,
    now: i64,
    errors: &mut Vec<FieldError>,
) -> Option<ScoreSource> {
    let before = errors.len();
    let source = writer.source(item.source);
    if let Some(requested) = item.source.filter(|_| source.is_none()) {
        errors.push(field(
            index,
            "source",
            &format!("source {} is not allowed here", requested.as_str()),
        ));
    }
    if !text_ok(&item.name) {
        errors.push(field(index, "name", "name must be 1 to 255 bytes"));
    }
    if let ScoreTarget::Session { session_id } = &item.target
        && !text_ok(session_id)
    {
        errors.push(FieldError {
            field: format!("scores[{index}].target.session_id"),
            message: "session_id must be 1 to 255 bytes".to_owned(),
            position: None,
        });
    }
    if let ScoreValue::Categorical { label, .. } = &item.value
        && !text_ok(label)
    {
        errors.push(field(index, "value", "label must be 1 to 255 bytes"));
    }
    if let Some(id) = item.score_id.filter(|id| is_v7(*id)) {
        match v7_created_at(id) {
            None => errors.push(field(index, "score_id", "score_id time is in the future")),
            Some(created) if created > now.saturating_add(HOUR_NS) => {
                errors.push(field(index, "score_id", "score_id time is in the future"));
            }
            Some(created) => {
                if let Some(days) = project.retention_days {
                    let cutoff = now.saturating_sub(i64::from(days).saturating_mul(DAY_NS));
                    if created < cutoff {
                        errors.push(field(
                            index,
                            "score_id",
                            "score_id time is older than the project's retention",
                        ));
                    }
                }
            }
        }
    }
    source.filter(|_| errors.len() == before)
}

fn is_v7(id: Uuid) -> bool {
    id.get_version() == Some(uuid::Version::SortRand)
}

/// `created_at` of a UUIDv7: its Unix time in nanoseconds. `None` when the time does not fit.
fn v7_created_at(id: Uuid) -> Option<i64> {
    let (secs, nanos) = id.get_timestamp()?.to_unix();
    i64::try_from(secs)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(i64::from(nanos))
}

/// `(created_at, updated_at)` of a row with final id `id` (spec 06 § Times).
///
/// A UUIDv7 gives `created_at` from its time and needs no lookup. Any other id takes the
/// live version's `created_at`, or `now` for a new score.
fn times(id: Uuid, now: i64, live: &HashMap<Uuid, Score>) -> (i64, i64) {
    if let Some(created) = v7_created_at(id).filter(|_| is_v7(id)) {
        return (created, now.max(created));
    }
    match live.get(&id) {
        Some(found) => (
            found.created_at,
            now.max(found.updated_at.saturating_add(1)),
        ),
        None => (now, now),
    }
}

/// The derived `score_id` of an item without one (spec 06, D-G-28): UUIDv8 over SHA-256.
///
/// The seven fields are length-prefixed, so no two different field lists share an input.
fn derive_id(
    project: ProjectId,
    target: &ScoreTarget,
    name: &str,
    source: ScoreSource,
    author: Uuid,
) -> Uuid {
    let (trace, locator) = match target {
        ScoreTarget::Span { trace_id, span_id } => (trace_id.to_string(), span_id.to_string()),
        ScoreTarget::Trace { trace_id } => (trace_id.to_string(), String::new()),
        ScoreTarget::Session { session_id } => (String::new(), session_id.clone()),
    };
    let fields = [
        project.to_string(),
        target.kind().as_str().to_owned(),
        trace,
        locator,
        name.to_owned(),
        source.as_str().to_owned(),
        author.to_string(),
    ];
    let mut hasher = Sha256::new();
    hasher.update(DERIVE_TAG);
    for field in &fields {
        // Names and ids are at most 255 bytes here, so the length always fits in `u32`.
        let length = u32::try_from(field.len()).unwrap_or(u32::MAX);
        hasher.update(length.to_be_bytes());
        hasher.update(field.as_bytes());
    }
    let digest = hasher.finalize();
    // SHA-256 has 32 bytes, so the first 16 always exist.
    let bytes: [u8; 16] = std::array::from_fn(|index| digest[index]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// The live versions of `ids` (UUID strings), keyed by score id. No ids means no lookup.
async fn live_scores(
    state: &AppState,
    project: ProjectId,
    ids: &[String],
) -> Result<HashMap<Uuid, Score>, ApiError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ctx = open_query(state, project, None).await?;
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let batches = llmobs_storage::scores_by_id(&ctx, &refs, READ_TIMEOUT)
        .await
        .map_err(|error| {
            tracing::warn!(kind = error.kind(), "score lookup failed");
            ApiError::Unavailable {
                detail: "score lookup is unavailable, retry later",
            }
        })?;
    let scores = convert::scores(&batches, project)?;
    Ok(scores
        .into_iter()
        .map(|score| (score.score_id, score))
        .collect())
}

/// Appends `rows` as one batch (spec 06 § Score write rules, step 7).
async fn append(state: &AppState, project: ProjectId, rows: &[Score]) -> Result<(), ApiError> {
    let batch = llmobs_core::batch::scores_to_record_batch(rows)
        .map_err(|error| ApiError::Internal(Box::new(error)))?;
    state
        .storage
        .append_scores(project, batch)
        .await
        .map_err(|error| ApiError::from(IngestError::Append(error)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::test_support::TestApp;
    use llmobs_storage::ScoreListRequest;

    /// The fixed project of the derived-id tests.
    const PROJECT: &str = "018f6b4e-7c1a-7c1a-8c1a-000000000001";
    const TRACE: &str = "a1000000000000000000000000000001";
    const SPAN: &str = "b100000000000012";
    const AUTHOR: &str = "01900000-0000-7000-8000-000000000402";
    /// Computed from the spec's byte layout with SHA-256 outside this code, then pinned.
    const DERIVED: &str = "119b8f11-8f63-8b55-a76d-e39b7f513b86";
    /// UUIDv7 with Unix time 1_717_986_918_400 ms (2024-06-10).
    const V7_FIXED: &str = "01900000-0000-7000-8000-000000000401";
    const V7_FIXED_MS: i64 = 1_717_986_918_400;

    fn span_target() -> ScoreTarget {
        ScoreTarget::Span {
            trace_id: TRACE.parse().unwrap(),
            span_id: SPAN.parse().unwrap(),
        }
    }

    fn author() -> Uuid {
        AUTHOR.parse().unwrap()
    }

    fn api_writer() -> Writer {
        Writer::Api { author: author() }
    }

    fn item(name: &str, value: f64) -> ScoreCreate {
        ScoreCreate {
            score_id: None,
            target: span_target(),
            session_id: None,
            user_id: None,
            name: name.to_owned(),
            value: ScoreValue::Numeric(value),
            reason: None,
            source: None,
            metadata: BTreeMap::new(),
        }
    }

    /// A UUIDv4 with the given first byte.
    fn v4(byte: u8) -> Uuid {
        uuid::Builder::from_random_bytes([byte; 16]).into_uuid()
    }

    /// A UUIDv7 whose time is `ms` milliseconds after the Unix epoch.
    fn v7_at(ms: u64) -> Uuid {
        uuid::Builder::from_unix_timestamp_millis(ms, &[0; 10]).into_uuid()
    }

    /// Every stored live score of `project`, read through storage.
    async fn stored(app: &TestApp, project: ProjectId) -> Vec<Score> {
        let ctx = open_query(&app.state, project, None).await.unwrap();
        let page = llmobs_storage::score_list(
            &ctx,
            ScoreListRequest {
                target: None,
                trace_id: None,
                span_id: None,
                session_id: None,
                name: None,
                source: None,
                window: None,
                limit: 100,
                cursor: None,
                timeout: READ_TIMEOUT,
            },
        )
        .await
        .unwrap();
        convert::scores(&page.batches, project).unwrap()
    }

    /// The `(field, message)` pairs of a refused write.
    fn refused(result: Result<Vec<Uuid>, ApiError>) -> Vec<(String, String)> {
        match result {
            Err(ApiError::BadRequest { errors, .. }) => errors
                .into_iter()
                .map(|error| (error.field, error.message))
                .collect(),
            other => panic!("expected a bad request, got {other:?}"),
        }
    }

    fn project_id() -> ProjectId {
        PROJECT.parse().unwrap()
    }

    #[test]
    fn derive_id_matches_the_pinned_value() {
        let id = derive_id(
            project_id(),
            &span_target(),
            "accuracy",
            ScoreSource::Sdk,
            author(),
        );
        assert_eq!(id.to_string(), DERIVED);
        assert_eq!(id.get_version_num(), 8);
        assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
    }

    #[test]
    fn derive_id_changes_with_each_of_the_seven_fields() {
        let base = derive_id(
            project_id(),
            &span_target(),
            "accuracy",
            ScoreSource::Sdk,
            author(),
        );
        let other_project: ProjectId = "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap();
        let trace = ScoreTarget::Trace {
            trace_id: TRACE.parse().unwrap(),
        };
        let other_trace = ScoreTarget::Span {
            trace_id: "a1000000000000000000000000000002".parse().unwrap(),
            span_id: SPAN.parse().unwrap(),
        };
        let other_span = ScoreTarget::Span {
            trace_id: TRACE.parse().unwrap(),
            span_id: "b100000000000013".parse().unwrap(),
        };
        let session = ScoreTarget::Session {
            session_id: "chat-42".to_owned(),
        };
        let other_author: Uuid = "01900000-0000-7000-8000-000000000403".parse().unwrap();
        let changed = [
            derive_id(
                other_project,
                &span_target(),
                "accuracy",
                ScoreSource::Sdk,
                author(),
            ),
            derive_id(project_id(), &trace, "accuracy", ScoreSource::Sdk, author()),
            derive_id(
                project_id(),
                &other_trace,
                "accuracy",
                ScoreSource::Sdk,
                author(),
            ),
            derive_id(
                project_id(),
                &other_span,
                "accuracy",
                ScoreSource::Sdk,
                author(),
            ),
            derive_id(
                project_id(),
                &session,
                "accuracy",
                ScoreSource::Sdk,
                author(),
            ),
            derive_id(
                project_id(),
                &span_target(),
                "relevance",
                ScoreSource::Sdk,
                author(),
            ),
            derive_id(
                project_id(),
                &span_target(),
                "accuracy",
                ScoreSource::Api,
                author(),
            ),
            derive_id(
                project_id(),
                &span_target(),
                "accuracy",
                ScoreSource::Sdk,
                other_author,
            ),
        ];
        for other in changed {
            assert_ne!(other, base);
        }
    }

    #[test]
    fn v7_time_is_the_created_at_of_the_id() {
        let id: Uuid = V7_FIXED.parse().unwrap();
        assert_eq!(v7_created_at(id), Some(V7_FIXED_MS * 1_000_000));
        assert!(is_v7(id));
        assert!(!is_v7(v4(1)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn v7_id_far_in_the_future_is_refused_and_one_hour_is_accepted() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let now_ms = u64::try_from(now / 1_000_000).unwrap();
        let hour_ms = 3_600_000;

        let accepted = v7_at(now_ms + hour_ms);
        let written = write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![ScoreCreate {
                score_id: Some(accepted),
                ..item("accuracy", 0.5)
            }],
        )
        .await
        .unwrap();
        assert_eq!(written, vec![accepted]);
        let rows = stored(&app, seeded.project.id).await;
        assert_eq!(rows[0].created_at, now + 3_600_000_000_000);

        let refused_id = v7_at(now_ms + hour_ms + 1);
        let errors = refused(
            write(
                &app.state,
                &seeded.project,
                api_writer(),
                vec![ScoreCreate {
                    score_id: Some(refused_id),
                    ..item("accuracy", 0.5)
                }],
            )
            .await,
        );
        assert_eq!(
            errors,
            vec![(
                "scores[0].score_id".to_owned(),
                "score_id time is in the future".to_owned()
            )]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn v7_time_becomes_created_at() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let id: Uuid = V7_FIXED.parse().unwrap();
        write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![ScoreCreate {
                score_id: Some(id),
                ..item("accuracy", 0.5)
            }],
        )
        .await
        .unwrap();
        let rows = stored(&app, seeded.project.id).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].score_id, id);
        assert_eq!(rows[0].created_at, V7_FIXED_MS * 1_000_000);
        assert_eq!(rows[0].updated_at, now);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_score_takes_now_for_both_times() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let id = v4(1);
        write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![ScoreCreate {
                score_id: Some(id),
                ..item("accuracy", 0.8)
            }],
        )
        .await
        .unwrap();
        let rows = stored(&app, seeded.project.id).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.score_id, id);
        assert_eq!(row.created_at, now);
        assert_eq!(row.updated_at, now);
        assert_eq!(row.source, ScoreSource::Api);
        assert_eq!(row.author_user_id, None);
        assert_eq!(row.value, ScoreValue::Numeric(0.8));
        assert_eq!(row.target, span_target());
        assert!(!row.deleted);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_keeps_created_at() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let id = v4(2);
        let write_value = |value: f64| {
            write(
                &app.state,
                &seeded.project,
                api_writer(),
                vec![ScoreCreate {
                    score_id: Some(id),
                    ..item("accuracy", value)
                }],
            )
        };
        write_value(0.8).await.unwrap();
        app.state.clock.advance(std::time::Duration::from_secs(60));
        write_value(0.2).await.unwrap();

        let rows = stored(&app, seeded.project.id).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, ScoreValue::Numeric(0.2));
        assert_eq!(rows[0].created_at, now);
        assert_eq!(rows[0].updated_at, now + 60_000_000_000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_hides_the_score_and_a_second_delete_is_not_found() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let id = v4(3);
        write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![ScoreCreate {
                score_id: Some(id),
                ..item("accuracy", 0.8)
            }],
        )
        .await
        .unwrap();

        delete(&app.state, &seeded.project, id).await.unwrap();
        assert!(stored(&app, seeded.project.id).await.is_empty());
        match delete(&app.state, &seeded.project, id).await {
            Err(ApiError::NotFound) => {}
            other => panic!("expected not found, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sources_follow_the_caller() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let user = Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0500);
        let human = Writer::Human { user_id: user };
        let sdk = Writer::Sdk { key_id: author() };

        // A person may not send `source: api`.
        let errors = refused(
            write(
                &app.state,
                &seeded.project,
                human,
                vec![ScoreCreate {
                    source: Some(ScoreSource::Api),
                    ..item("accuracy", 0.5)
                }],
            )
            .await,
        );
        assert_eq!(
            errors,
            vec![(
                "scores[0].source".to_owned(),
                "source api is not allowed here".to_owned()
            )]
        );
        // An `sk` key may not send `source: human`.
        let errors = refused(
            write(
                &app.state,
                &seeded.project,
                sdk,
                vec![ScoreCreate {
                    source: Some(ScoreSource::Human),
                    ..item("accuracy", 0.5)
                }],
            )
            .await,
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "scores[0].source");
        // A PAT or key on the project route may send `sdk`.
        write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![ScoreCreate {
                source: Some(ScoreSource::Sdk),
                ..item("relevance", 0.5)
            }],
        )
        .await
        .unwrap();
        // A person's score stores the person as its author.
        write(&app.state, &seeded.project, human, vec![item("tone", 1.0)])
            .await
            .unwrap();

        // Feedback is stored as `end_user`; any other source is refused.
        let end_user = Writer::EndUser { key_id: author() };
        let errors = refused(
            write(
                &app.state,
                &seeded.project,
                end_user,
                vec![ScoreCreate {
                    source: Some(ScoreSource::Sdk),
                    ..item("helpful", 1.0)
                }],
            )
            .await,
        );
        assert_eq!(errors.len(), 1);
        write(
            &app.state,
            &seeded.project,
            end_user,
            vec![item("helpful", 1.0)],
        )
        .await
        .unwrap();

        let rows = stored(&app, seeded.project.id).await;
        let relevance = rows.iter().find(|row| row.name == "relevance").unwrap();
        assert_eq!(relevance.source, ScoreSource::Sdk);
        assert_eq!(relevance.author_user_id, None);
        let tone = rows.iter().find(|row| row.name == "tone").unwrap();
        assert_eq!(tone.source, ScoreSource::Human);
        assert_eq!(tone.author_user_id, Some(user));
        let helpful = rows.iter().find(|row| row.name == "helpful").unwrap();
        assert_eq!(helpful.source, ScoreSource::EndUser);
        assert_eq!(helpful.author_user_id, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_items_report_every_field_and_write_nothing() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let long_name = "n".repeat(256);
        let items = vec![
            item("", 0.5),
            item(&long_name, 0.5),
            ScoreCreate {
                target: ScoreTarget::Session {
                    session_id: String::new(),
                },
                ..item("accuracy", 0.5)
            },
            ScoreCreate {
                value: ScoreValue::Categorical {
                    label: String::new(),
                    value: None,
                },
                ..item("accuracy", 0.5)
            },
        ];
        let errors = refused(write(&app.state, &seeded.project, api_writer(), items).await);
        assert_eq!(
            errors,
            vec![
                (
                    "scores[0].name".to_owned(),
                    "name must be 1 to 255 bytes".to_owned()
                ),
                (
                    "scores[1].name".to_owned(),
                    "name must be 1 to 255 bytes".to_owned()
                ),
                (
                    "scores[2].target.session_id".to_owned(),
                    "session_id must be 1 to 255 bytes".to_owned()
                ),
                (
                    "scores[3].value".to_owned(),
                    "label must be 1 to 255 bytes".to_owned()
                ),
            ]
        );
        assert!(stored(&app, seeded.project.id).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batch_size_is_between_one_and_a_thousand() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let expected = vec![(
            "scores".to_owned(),
            "between 1 and 1000 scores per request".to_owned(),
        )];
        let empty = refused(write(&app.state, &seeded.project, api_writer(), Vec::new()).await);
        assert_eq!(empty, expected);
        let too_many: Vec<ScoreCreate> = (0..=MAX_SCORES).map(|_| item("accuracy", 0.5)).collect();
        let too_many = refused(write(&app.state, &seeded.project, api_writer(), too_many).await);
        assert_eq!(too_many, expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_ids_keep_the_last_item_and_answer_each_position() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let id = v4(4);
        let items = vec![
            ScoreCreate {
                score_id: Some(id),
                ..item("accuracy", 0.1)
            },
            ScoreCreate {
                score_id: Some(id),
                ..item("accuracy", 0.9)
            },
        ];
        let ids = write(&app.state, &seeded.project, api_writer(), items)
            .await
            .unwrap();
        assert_eq!(ids, vec![id, id]);
        let rows = stored(&app, seeded.project.id).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, ScoreValue::Numeric(0.9));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn derived_id_is_overwritten_by_the_same_writer_and_target() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let first = write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![item("accuracy", 0.1)],
        )
        .await
        .unwrap();
        let second = write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![item("accuracy", 0.9)],
        )
        .await
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].get_version_num(), 8);

        let rows = stored(&app, seeded.project.id).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, ScoreValue::Numeric(0.9));
        assert_eq!(rows[0].created_at, now);
        assert_eq!(rows[0].updated_at, now + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn v7_time_must_be_inside_the_project_retention() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (short, _) = app.add_project(&seeded, "short", Some(1)).await;
        let now = app.state.clock.now_ns();
        let cutoff = now - DAY_NS;
        // The last whole millisecond before the cutoff, and the first one at or after it.
        let outside = v7_at(u64::try_from(cutoff / 1_000_000 - 1).unwrap());
        let inside = v7_at(u64::try_from((cutoff + 999_999) / 1_000_000).unwrap());
        let with_id = |id: Uuid| ScoreCreate {
            score_id: Some(id),
            ..item("accuracy", 0.5)
        };

        let errors = refused(write(&app.state, &short, api_writer(), vec![with_id(outside)]).await);
        assert_eq!(
            errors,
            vec![(
                "scores[0].score_id".to_owned(),
                "score_id time is older than the project's retention".to_owned()
            )]
        );
        let written = write(&app.state, &short, api_writer(), vec![with_id(inside)])
            .await
            .unwrap();
        assert_eq!(written, vec![inside]);
        // A project without retention keeps the old id.
        let written = write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![with_id(outside)],
        )
        .await
        .unwrap();
        assert_eq!(written, vec![outside]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn v7_time_beyond_the_timestamp_range_is_in_the_future() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let largest = v7_at(u64::MAX >> 16);
        let errors = refused(
            write(
                &app.state,
                &seeded.project,
                api_writer(),
                vec![ScoreCreate {
                    score_id: Some(largest),
                    ..item("accuracy", 0.5)
                }],
            )
            .await,
        );
        assert_eq!(
            errors,
            vec![(
                "scores[0].score_id".to_owned(),
                "score_id time is in the future".to_owned()
            )]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_thousand_scores_are_accepted_in_one_request() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let items: Vec<ScoreCreate> = (0..MAX_SCORES)
            .map(|index| item(&format!("score-{index}"), 0.5))
            .collect();
        let ids = write(&app.state, &seeded.project, api_writer(), items)
            .await
            .unwrap();
        assert_eq!(ids.len(), MAX_SCORES);
        let distinct: std::collections::BTreeSet<Uuid> = ids.into_iter().collect();
        assert_eq!(distinct.len(), MAX_SCORES);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sdk_writer_without_a_body_source_stores_sdk() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let id = v4(5);
        write(
            &app.state,
            &seeded.project,
            Writer::Sdk { key_id: author() },
            vec![ScoreCreate {
                score_id: Some(id),
                ..item("accuracy", 0.5)
            }],
        )
        .await
        .unwrap();
        let rows = stored(&app, seeded.project.id).await;
        let row = rows.iter().find(|row| row.score_id == id).unwrap();
        assert_eq!(row.source, ScoreSource::Sdk);
        assert_eq!(row.author_user_id, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_target_drops_the_body_session_id() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let on_session = v4(6);
        let on_span = v4(7);
        write(
            &app.state,
            &seeded.project,
            api_writer(),
            vec![
                ScoreCreate {
                    score_id: Some(on_session),
                    target: ScoreTarget::Session {
                        session_id: "chat-42".to_owned(),
                    },
                    session_id: Some("chat-42".to_owned()),
                    ..item("session-score", 0.5)
                },
                ScoreCreate {
                    score_id: Some(on_span),
                    session_id: Some("chat-42".to_owned()),
                    ..item("span-score", 0.5)
                },
            ],
        )
        .await
        .unwrap();
        let rows = stored(&app, seeded.project.id).await;
        let session = rows.iter().find(|row| row.score_id == on_session).unwrap();
        assert_eq!(session.session_id, None);
        let span = rows.iter().find(|row| row.score_id == on_span).unwrap();
        assert_eq!(span.session_id.as_deref(), Some("chat-42"));
    }
}
