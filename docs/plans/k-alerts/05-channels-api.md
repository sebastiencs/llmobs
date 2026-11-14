# K5 — Channel routes, OpenAPI and fixtures

**Commit subject:** `server: add notification channel routes`

Adds `GET/POST /api/v1/projects/{project}/channels` and `PATCH/DELETE /api/v1/projects/{project}/channels/{id}` (spec 09), their OpenAPI operations, and the list fixture. "Send test" (`POST …/channels/{id}/test`) needs the message builders and comes in K8.

## Read first

- README § Design, D-K-5, D-K-6, D-K-9, § Conventions.
- `crates/server/src/routes/api_keys.rs` (the model: `routes()`, handlers, `field_error`, audit logs), `crates/server/src/http/access.rs` (`RouteDef`, `Access`, `ProjectAccess`, `routes()` list, test `route_table_matches_parameters_and_openapi_security`), `crates/server/src/http/access_suite.rs` (`concrete_path`, `expected`, `is_list`).
- `crates/server/src/auth/principal.rs`: `Principal`, `session_user`.
- `crates/server/src/http/error.rs`: `ApiError`, `error::Json`, `error::Path`; `crates/server/src/api/common.rs`: `Page`, `FieldError`, `ProblemCode`.
- `crates/server/src/api/openapi.rs`: `collect_schemas`, the `tags(...)` list, `paths()`, `api_key_paths`, `saved_view_paths`, helpers `operation`, `project_param`, `path_uuid`, `json_named`, `json_of`, `json_body`, `responses`, `problem`, constants `PROJECT`, `AUTH_ERRORS`.
- `crates/server/tests/openapi.rs`: `OPERATIONS`, `TAGS`, the `expected_errors` chain in `document_lint`, and the two `pages` lists.
- `crates/server/src/api_examples.rs` (`generate()`, the static `example(...)` entries), `crates/server/tests/api_examples.rs` (`static_files_match_their_types`), `fixtures/api/README.md`, `fixtures/api/static/api-keys.json`.
- K3 (`api::alerts`, `alerts::channel`) and K4 (`meta::alerts`, `seal_config`, `open_config`).

## Files

| File | Change |
|---|---|
| `crates/server/src/api/alerts.rs` | channel request and response types |
| `crates/server/src/routes/channels.rs` | new |
| `crates/server/src/routes/mod.rs` | `pub(crate) mod channels;` (alphabetical: after `auth`, before `feedback`) |
| `crates/server/src/http/access.rs` | `routes.extend(crate::routes::channels::routes());` in `routes()`, after the `api_keys` line |
| `crates/server/src/api/openapi.rs` | schemas, tag `alerts`, `channel_paths` |
| `crates/server/tests/openapi.rs` | operations, tag, errors, pages |
| `crates/server/openapi.json`, `web/src/api/schema.gen.ts` | regenerated (`just openapi`) |
| `fixtures/api/static/channels.json` | new |
| `crates/server/src/api_examples.rs` | one static example |
| `fixtures/api/manifest.json` | regenerated (`just api-examples`) |
| `crates/server/tests/api_examples.rs` | one round-trip |

## 1. Types in `api/alerts.rs`

Add (imports: `std::collections::BTreeMap`, `llmobs_core::time_serde`, `uuid::Uuid`):

```rust
/// A delivery's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// Waiting for its first or next attempt.
    Pending,
    /// The channel accepted it.
    Sent,
    /// Gave up: a permanent error, or 8 attempts.
    Failed,
}

impl DeliveryStatus {
    /// The stored spelling: `pending`, `sent`, `failed`.
    #[must_use]
    pub fn as_str(self) -> &'static str { /* match */ }

    /// Parses the stored spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> { /* match; anything else is None */ }
}

/// The newest finished delivery of a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelDelivery {
    /// `sent` or `failed`.
    pub status: DeliveryStatus,
    /// When it finished.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub at: i64,
    /// Why it failed. Never contains a URL, a secret or an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A notification channel. The URL, the secret and header values are never returned (D-K-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotificationChannel {
    /// Channel id.
    pub id: Uuid,
    /// Display name, unique in the project.
    pub name: String,
    /// `slack`, `webhook` or `email`.
    pub kind: ChannelKind,
    /// Where it sends, without secrets: `hooks.slack.com/…/abcd`, a host, or the first address and `+N`.
    pub target_hint: String,
    /// False for an email channel while SMTP is not configured.
    pub available: bool,
    /// Email recipients. Empty for other kinds.
    pub recipients: Vec<String>,
    /// Names of the extra webhook headers. Empty for other kinds.
    pub header_names: Vec<String>,
    /// The newest finished delivery. Absent when none finished yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<ChannelDelivery>,
    /// Creation time.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// Last change.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
}

/// Body of `POST /channels`. Send the fields of `kind` only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateChannelRequest {
    /// Display name, 1–100 characters after trimming.
    pub name: String,
    /// The kind. Cannot change later.
    pub kind: ChannelKind,
    /// `slack` and `webhook`: the URL.
    #[serde(default)]
    pub url: Option<String>,
    /// `webhook`: the signing secret, 16–256 characters.
    #[serde(default)]
    pub secret: Option<String>,
    /// `webhook`: extra headers, at most 10.
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// `email`: 1–20 addresses.
    #[serde(default)]
    pub recipients: Option<Vec<String>>,
}

/// Body of `PATCH /channels/{id}`. An absent field keeps its value; `headers` replaces the whole map.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateChannelRequest {
    /// New display name.
    #[serde(default)]
    pub name: Option<String>,
    /// `slack` and `webhook`: a new URL.
    #[serde(default)]
    pub url: Option<String>,
    /// `webhook`: a new signing secret.
    #[serde(default)]
    pub secret: Option<String>,
    /// `webhook`: the new header map.
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// `email`: the new recipient list.
    #[serde(default)]
    pub recipients: Option<Vec<String>>,
}
```

`Debug` is derived on the request types; the server never logs them (the convention for `CreateApiKeyRequest` too). Do not add a `Serialize` derive to the request types.

## 2. `routes/channels.rs`

Module doc: `//! Notification channels (spec 07 § Channels, D-K-5, D-K-6).`

Imports (rustfmt orders them; run `just fmt`): `axum::extract::{Extension, State}`, `axum::http::{Method, StatusCode}`, `axum::routing::{delete, get, patch, post}`, `serde::Deserialize`, `uuid::Uuid`, `crate::alerts::channel::{ChannelConfig, open_config, seal_config}`, `crate::api::alerts::{ChannelDelivery, ChannelKind, CreateChannelRequest, DeliveryStatus, NotificationChannel, UpdateChannelRequest}`, `crate::api::common::{FieldError, Page}`, `crate::auth::principal::{self, Principal}`, `crate::auth::roles::ProjectPerm`, `crate::http::AppState`, `crate::http::access::{Access, ProjectAccess, RouteDef}`, `crate::http::error::{self, ApiError}`, `crate::meta::alerts::{self, ChannelPatch, ChannelRow, LastDelivery, NewChannel}`.

The handlers share names with the store functions: always call the store through the module path (`alerts::list_channels`, `alerts::insert_channel`, `alerts::channel_by_id`, `alerts::update_channel`, `alerts::delete_channel`, `alerts::last_deliveries`), as `api_keys.rs` calls `api_keys::list_api_keys`.

### Route table

```rust
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef { method: Method::GET,    path: "/api/v1/projects/{project}/channels",      access: Access::Project { perm: ProjectPerm::View,         session_only: false }, documented: true, handler: get(list_channels) },
        RouteDef { method: Method::POST,   path: "/api/v1/projects/{project}/channels",      access: Access::Project { perm: ProjectPerm::ManageAlerts, session_only: false }, documented: true, handler: post(create_channel) },
        RouteDef { method: Method::PATCH,  path: "/api/v1/projects/{project}/channels/{id}", access: Access::Project { perm: ProjectPerm::ManageAlerts, session_only: false }, documented: true, handler: patch(update_channel) },
        RouteDef { method: Method::DELETE, path: "/api/v1/projects/{project}/channels/{id}", access: Access::Project { perm: ProjectPerm::ManageAlerts, session_only: false }, documented: true, handler: delete(delete_channel) },
    ]
}
```

(Format with rustfmt; one field per line as in `api_keys.rs`.)

### Handlers

```rust
#[derive(Debug, Deserialize)]
struct ChannelPath { id: Uuid }

async fn list_channels(State(state): State<AppState>, Extension(access): Extension<ProjectAccess>)
    -> Result<error::Json<Page<NotificationChannel>>, ApiError>;
async fn create_channel(State(state): State<AppState>, Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>, error::Json(body): error::Json<CreateChannelRequest>)
    -> Result<(StatusCode, error::Json<NotificationChannel>), ApiError>;
async fn update_channel(State(state): State<AppState>, Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>, error::Path(ChannelPath { id }): error::Path<ChannelPath>,
    error::Json(body): error::Json<UpdateChannelRequest>) -> Result<error::Json<NotificationChannel>, ApiError>;
async fn delete_channel(State(state): State<AppState>, Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>, error::Path(ChannelPath { id }): error::Path<ChannelPath>)
    -> Result<StatusCode, ApiError>;
```

**`list_channels`**: `alerts::list_channels(state.meta.reader(), project)` and `alerts::last_deliveries(…)`; map each row with `channel_json` (below); answer `Page { data, next_cursor: None }`. It takes no query parameters and has no cursor: a project has at most 20 channels (D-K-5), so the list is never paged. Do not add `PageParams`.

**`create_channel`**, in this order (tests rely on it):
1. `let user = principal::session_user(&principal)?;` (an API key never reaches here: `ManageAlerts` has no key scope).
2. `let name = checked_name(&body.name)?;` — trim; empty → `field_error("name", "is required")`; more than 100 characters → `field_error("name", "at most 100 characters")`.
3. `kind == Email && state.mailer.is_none()` → `field_error("kind", "email channels need SMTP (smtp_url is not set)")`.
4. Build the config with `config_from_create(&body)`:
   - `Slack`: `url` required (`field_error("url", "is required")`); `secret`, `headers`, `recipients` must be absent (`field_error("<field>", "does not apply to slack")`).
   - `Webhook`: `url` and `secret` required; `headers` defaults to an empty map; `recipients` must be absent (`"does not apply to webhook"`).
   - `Email`: `recipients` required, each entry trimmed; `url`, `secret`, `headers` must be absent (`"does not apply to email"`).
   - Collect every error of this step into one `ApiError::BadRequest { detail: None, errors }`, in field order `url`, `secret`, `headers`, `recipients`.
5. `config.validate()`; errors → `ApiError::BadRequest { detail: None, errors }`.
6. `let id = Uuid::now_v7(); let sealed = seal_config(&state.secrets, &id, &config).map_err(|error| ApiError::Internal(Box::new(error)))?;`
7. `state.meta.write(…alerts::insert_channel(conn, NewChannel { id, project_id, kind, name, config_enc: sealed, target_hint: config.target_hint() }, now)…)`. A `MetaError::Conflict` becomes 409 through `From<MetaError>`.
8. Audit log: `tracing::info!(target: "audit", channel_id = %row.id, project_id = %project_id, user_id = %user.id, kind = row.kind.as_str(), "alert channel created");`
9. `Ok((StatusCode::CREATED, error::Json(channel_json(&state, &row, None)?)))`.

**`update_channel`**:
1. `session_user`.
2. `alerts::channel_by_id(reader, project, id)`; `None` → `ApiError::NotFound`. (A PATCH of an email channel while SMTP is unset is allowed; only create checks the mailer.)
3. `name`: when `Some`, `checked_name`.
4. When any of `url`, `secret`, `headers`, `recipients` is `Some`: open the current config (`open_config(&state.secrets, &row.id, &row.config_enc)`; an error → `ApiError::Internal`), then apply the fields that belong to its kind; a field of another kind → `field_error("<field>", "does not apply to <kind>")` (all such errors together, field order as above). Trim recipients. Then `validate()`; then seal again with the same id. Without any of these four fields the config is untouched.
5. `alerts::update_channel(conn, project, id, ChannelPatch { name, config: Some((sealed, hint)) or None }, now)`; `None` → 404.
6. Audit log `"alert channel updated"` with the same fields.
7. Answer `channel_json(&state, &row, last)` where `last` is this channel's entry of `last_deliveries`.

**`delete_channel`**: `session_user`; `alerts::delete_channel(conn, project, id)`; `false` → `NotFound`; audit `"alert channel deleted"` (no `kind` field needed); `StatusCode::NO_CONTENT`.

### `channel_json`

```rust
fn channel_json(state: &AppState, row: &ChannelRow, last: Option<&LastDelivery>) -> Result<NotificationChannel, ApiError>
```

- `available = row.kind != ChannelKind::Email || state.mailer.is_some()`.
- `recipients` and `header_names`: open the config (`open_config`). For `Email`, `recipients` is the stored list; for `Webhook`, `header_names` is the header map's keys in map order. Other kinds: empty vectors. If the config does not open, log `tracing::warn!(channel_id = %row.id, "channel config does not open")` and return empty vectors (the list must still answer).
- `last_delivery`: `last.map(…)` with `DeliveryStatus::parse(&last.status)`; an unparsable status → treat as absent.

### `field_error`

Copy the private helper of `routes/api_keys.rs`:

```rust
fn field_error(field: &str, message: &str) -> ApiError {
    ApiError::BadRequest { detail: None, errors: vec![FieldError { field: field.to_owned(), message: message.to_owned(), position: None }] }
}
```

## 3. OpenAPI

In `api/openapi.rs`:
- `collect_schemas`: `set.insert::<ChannelKind>(); set.insert::<DeliveryStatus>(); set.insert::<ChannelDelivery>(); set.insert::<NotificationChannel>(); set.insert::<CreateChannelRequest>(); set.insert::<UpdateChannelRequest>(); set.insert_page::<NotificationChannel>("ChannelPage");` (append before `set`; the page next to the other `insert_page` lines).
- Tags: append `tag("alerts", "Alert rules, alert events and notification channels."),` after `tag("media", …)`.
- `paths()`: `let paths = channel_paths(paths);` before `media_paths(paths).build()`.
- New function:

| operationId | Method, path | Summary | Success | Errors |
|---|---|---|---|---|
| `list_channels` | GET `{PROJECT}/channels` | `List notification channels` | `200` `json_named("ChannelPage", "Every channel of the project, by name.")` | `AUTH_ERRORS` |
| `create_channel` | POST `{PROJECT}/channels` | `Create a notification channel` | `201` `json_of::<NotificationChannel>("The created channel.")` | `&[400, 401, 403, 404, 409, 429]` |
| `update_channel` | PATCH `{PROJECT}/channels/{id}` | `Update a notification channel` | `200` `json_of::<NotificationChannel>("The updated channel.")` | `&[400, 401, 403, 404, 409, 429]` |
| `delete_channel` | DELETE `{PROJECT}/channels/{id}` | `Delete a notification channel` | `204` `ResponseBuilder::new().description("Deleted. Rules that used it no longer do.").build()` | `&[400, 401, 403, 404, 429]` |

Every operation has tag `"alerts"`, `.parameters(Some(vec![project_param()]))` (plus `path_uuid("id", "Channel id.")` for `{id}`), and no `.securities(...)` (these routes are not session-only). `create_channel` has `.request_body(Some(json_body("CreateChannelRequest")))`, `update_channel` `json_body("UpdateChannelRequest")`. `create_channel` gets `.description(Some("Needs role member. Email channels need SMTP. At most 20 channels per project (409)."))`. Register the paths like `saved_view_paths` does (one `.path(...)` per method; the builder merges equal paths).

In `crates/server/tests/openapi.rs`:
- `OPERATIONS`: add `("get", "/api/v1/projects/{project}/channels", "list_channels")`, `("post", "/api/v1/projects/{project}/channels", "create_channel")`, `("patch", "/api/v1/projects/{project}/channels/{id}", "update_channel")`, `("delete", "/api/v1/projects/{project}/channels/{id}", "delete_channel")` (keep the list's order style).
- `TAGS`: add `"alerts"` after `"media"`.
- In the `expected_errors` chain, before the `READ_OPERATIONS` branch: `} else if *id == "create_channel" || *id == "update_channel" { expected_errors.extend(["400", "401", "403", "404", "409", "429"]);` and add `|| *id == "delete_channel"` to the branch whose set is `["400", "401", "403", "404", "429"]` (the `put_project_member` … branch). `list_channels` falls to the default `401, 403, 404, 429`.
- Both `pages` lists: add `("ChannelPage", "NotificationChannel")` to the first and `"ChannelPage"` to the second.

Run `just openapi`; commit `crates/server/openapi.json` and `web/src/api/schema.gen.ts`.

## 4. Fixture

Static fixtures must be canonical (`tests/api_examples.rs` compares the file with `serde_json::to_string_pretty` of its value plus a newline): two-space indent, one array element per line, `[]` for an empty array. Write the content below, then canonicalize it with

```bash
python3 -c 'import json,sys; p=sys.argv[1]; d=json.load(open(p)); open(p,"w").write(json.dumps(d,indent=2,ensure_ascii=False)+"\n")' fixtures/api/static/channels.json
```

(the key order of the objects below already matches the Rust field order).

The content of `fixtures/api/static/channels.json`:

```json
{
  "data": [
    {
      "id": "01900000-0000-7000-8000-000000000502",
      "name": "On-call email",
      "kind": "email",
      "target_hint": "oncall@example.com +1",
      "available": true,
      "recipients": ["oncall@example.com", "ada@example.com"],
      "header_names": [],
      "created_at": "2026-01-01T09:00:00.000000000Z",
      "updated_at": "2026-01-01T09:00:00.000000000Z"
    },
    {
      "id": "01900000-0000-7000-8000-000000000501",
      "name": "Pager webhook",
      "kind": "webhook",
      "target_hint": "hooks.example.com",
      "available": true,
      "recipients": [],
      "header_names": ["X-Team"],
      "last_delivery": {
        "status": "failed",
        "at": "2026-01-01T10:05:00.000000000Z",
        "error": "the endpoint answered 500"
      },
      "created_at": "2026-01-01T09:00:00.000000000Z",
      "updated_at": "2026-01-01T09:00:00.000000000Z"
    },
    {
      "id": "01900000-0000-7000-8000-000000000500",
      "name": "Team Slack",
      "kind": "slack",
      "target_hint": "hooks.slack.com/…/abcd",
      "available": true,
      "recipients": [],
      "header_names": [],
      "last_delivery": {
        "status": "sent",
        "at": "2026-01-01T10:05:00.000000000Z"
      },
      "created_at": "2026-01-01T09:00:00.000000000Z",
      "updated_at": "2026-01-01T09:00:00.000000000Z"
    }
  ],
  "next_cursor": null
}
```

(Ordered by name.)

- `api_examples.rs` `generate()`: add, in the static block next to the `list_api_keys` entry, `example("list_channels", BTreeMap::new(), BTreeMap::new(), "static/channels.json", "application/json"),`.
- Run `just api-examples` (it regenerates `fixtures/api/manifest.json`); commit the manifest.
- `tests/api_examples.rs` `static_files_match_their_types`: `roundtrip::<Page<NotificationChannel>>("static/channels.json");` and import `NotificationChannel` from `llmobs_server::api::alerts` (rustfmt places the `use` line).
- `tests/api_examples.rs` `generated_files_roundtrip`: its `match` panics on an unknown operation id; add `| "list_channels"` to the arm that lists the static operations (with `"list_saved_views"`).

## 5. Tests (`#[cfg(test)] mod tests` in `routes/channels.rs`)

Use `TestApp::with_mailer().await` (so email channels are available) and `app.seed().await`. Requests carry `seeded.cookie` (Ada, owner). Viewer and member users: copy the private test helpers `signed_in`, `add_member` and `cookie_pair` of `routes/members.rs` (around lines 421–470) into this test module with the imports they need, setting `is_instance_admin: false`; add the user to the org with `OrgRole::Viewer` or `OrgRole::Member`.

Reading stored rows: `sqlx::query_scalar::<_, Vec<u8>>("SELECT config_enc FROM notification_channels WHERE id = ?").bind(id.to_string()).fetch_one(app.state.meta.reader())`, and `open_config(&app.state.secrets, &row.id, &row.config_enc)` with `row` from `alerts::channel_by_id`. Writing directly: `app.state.meta.write(move |conn| Box::pin(async move { alerts::insert_channel(conn, NewChannel { … }, now).await })).await`.

A rule for test 8:

```rust
let condition = AlertCondition { metric: AlertMetric::ErrorRate, window: AlertWindow::M5, filter: None,
    comparator: Comparator::Gt, threshold: 0.5, min_count: Some(20), target: None,
    score_name: None, aggregation: None };
let rule = NewRule { project_id: seeded.project.id, name: "r".to_owned(), condition,
    channel_ids: vec![channel_id], enabled: true, cooldown_s: 3600, notify_while_firing: false, created_by: None };
```

Base path: `format!("/api/v1/projects/{}/channels", seeded.project.id)`.

1. `create_and_list_each_kind`: POST a Slack (`{"name":"Team Slack","kind":"slack","url":"https://hooks.slack.com/services/T0/B0/XXXXabcd"}`), a webhook (`{"name":"Pager","kind":"webhook","url":"https://hooks.example.com/x","secret":"0123456789abcdef","headers":{"X-Team":"core"}}`) and an email channel (`{"name":"Mail","kind":"email","recipients":["oncall@example.com"," ada@example.com "]}`) → each `201`. GET the list → three channels by name; Slack `target_hint` `hooks.slack.com/…/abcd`; webhook `header_names ["X-Team"]`; email `recipients ["oncall@example.com","ada@example.com"]` (trimmed). Assert on the raw response text: `!body.contains("XXXXabcd")`, `!body.contains("services/T0")`, `!body.contains("0123456789abcdef")`, `!body.contains("\"core\"")`.
2. `config_is_sealed_at_rest`: after creating the webhook, `SELECT config_enc FROM notification_channels` bytes do not contain `0123456789abcdef` as a substring (`windows`).
3. `fields_of_another_kind_are_refused`: Slack with `"secret":"x"` → `400`, field `secret`, message `does not apply to slack`; email without `recipients` → field `recipients` `is required`; webhook with a 15-character secret → field `secret`.
4. `email_needs_smtp`: with `TestApp::new()` and `seed()` (no mailer), POST an email channel → `400` field `kind`. A channel row inserted directly (`seal_config(&app.state.secrets, &id, &ChannelConfig::Email { recipients: vec!["a@x.io".to_owned()] })` and `alerts::insert_channel`) lists with `available: false`.
5. `names_are_unique`: the same name twice → `409`.
6. `update_keeps_absent_fields`: PATCH the webhook with `{"name":"Pager 2"}` → name changed, `header_names` unchanged; PATCH `{"headers":{}}` → `header_names []`; PATCH `{"recipients":["a@x.io"]}` → `400` `does not apply to webhook`; PATCH `{"secret":""}` → `400` field `secret`. Open the stored config after the name change and check the secret is still `0123456789abcdef`.
7. `update_and_delete_unknown_ids_are_404`: PATCH and DELETE a random UUIDv7 → `404`.
8. `delete_unlinks_rules`: create a channel, insert a rule using it with `meta::alerts::insert_rule`, DELETE the channel → `204`; `rule_by_id` shows empty `channel_ids`.
9. `roles`: a viewer may GET the list (`200`) but POST, PATCH and DELETE are `403`; a member may POST (`201`); an API key (`seeded.read_key` as `Authorization: Bearer …`) may GET (`200`) and POST is `403`.
10. `logs_hold_no_secrets`: wrap a create and an update of a webhook, and a create of an email channel to `oncall@example.com`, in `crate::test_support::assert_logs_clean(&["0123456789abcdef", "hooks.example.com/x", "oncall@example.com"], …)` (read its signature in `test_support/mod.rs` and use it as other tests do).

The generated authorization suite (`access_suite.rs`) covers the new routes automatically: `{id}` becomes a fixed UUID, and the body `{}` fails validation after access is granted. Run it; if it fails for these routes, read `expected()` and fix the route's `Access`, not the suite.

## Commands

```bash
just fmt
```

```bash
cargo test -p llmobs-server channels
```

```bash
just openapi
```

```bash
just api-examples
```

```bash
just check
```

## Done when

- The four routes answer as above, the OpenAPI document and the schema types are regenerated, the fixture and manifest entry exist.
- The 10 tests, the OpenAPI tests, the API-example tests and the access suite pass; `just check` is green.
