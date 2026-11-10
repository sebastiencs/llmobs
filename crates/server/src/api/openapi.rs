//! The OpenAPI document for the 22 read-API operations.
//!
//! Spec 09. Handlers are not implemented yet; this module is the contract.

use std::collections::BTreeMap;

use llmobs_core::enums::{ScoreSource, ScoreTargetKind};
use llmobs_core::score::Score;
use utoipa::openapi::path::{
    HttpMethod, OperationBuilder, Parameter, ParameterBuilder, ParameterIn, PathItem, Paths,
    PathsBuilder,
};
use utoipa::openapi::request_body::RequestBodyBuilder;
use utoipa::openapi::response::{Response, ResponseBuilder, Responses, ResponsesBuilder};
use utoipa::openapi::schema::{
    Components, ComponentsBuilder, KnownFormat, ObjectBuilder, Schema, SchemaFormat,
};
use utoipa::openapi::security::{
    ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme,
};
use utoipa::openapi::tag::{Tag, TagBuilder};
use utoipa::openapi::{Content, InfoBuilder, OpenApi, OpenApiBuilder, Ref, RefOr, Required};
use utoipa::{IntoParams, PartialSchema, ToSchema};

use super::account::{AuthConfig, Me, Project};
use super::common::{Limit, Page, PageParams, Problem, SortOrder, TimeParam};
use super::metrics::{MetricsBucket, MetricsGroupBy, MetricsParams, MetricsResponse};
use super::saved_views::{CreateSavedView, SavedView, SavedViewListParams, UpdateSavedView};
use super::schema::SchemaResponse;
use super::scores::ScoreListParams;
use super::sessions::{SessionDetail, SessionListParams, SessionSort, SessionSummary};
use super::traces::{
    SpanDetail, SpanListParams, SpanSort, SpanSummary, TraceDetail, TraceDetailParams,
    TraceListParams, TraceSort, TraceStreamParams, TraceSummary,
};
use super::users::{EndUserDetail, EndUserSummary, UserListParams, UserSort};

const PROJECT: &str = "/api/v1/projects/{project}";
const COOKIE_NAME: &str = concat!(llmobs_core::llmobs_name!(), "_session");

const AUTH_ERRORS: &[u16] = &[401, 403, 404, 429];
const QUERY_ERRORS: &[u16] = &[400, 401, 403, 404, 429];

/// Schemas collected for the document.
///
/// `clash` is set when two types produce the same component name with different schemas.
/// OpenAPI would keep only one of them.
pub(crate) struct SchemaSet {
    /// Component name to schema. Names are unique.
    pub schemas: BTreeMap<String, RefOr<Schema>>,
    /// The component name that two types disagreed on.
    pub clash: Option<String>,
}

/// Collects every component schema the document registers.
///
/// The same name may be reached from several types. A second insert is kept when the schema
/// matches, and a concrete schema replaces a `$ref` placeholder. A differing schema sets [`SchemaSet::clash`].
pub(crate) fn collect_schemas() -> SchemaSet {
    let mut set = SchemaSet {
        schemas: BTreeMap::new(),
        clash: None,
    };
    set.insert::<Problem>();
    set.insert::<Limit>();
    set.insert::<TimeParam>();
    set.insert::<SortOrder>();
    set.insert::<TraceSort>();
    set.insert::<SpanSort>();
    set.insert::<SessionSort>();
    set.insert::<UserSort>();
    set.insert::<ScoreTargetKind>();
    set.insert::<ScoreSource>();
    set.insert::<MetricsBucket>();
    set.insert::<MetricsGroupBy>();
    set.insert::<AuthConfig>();
    set.insert::<Me>();
    set.insert::<Project>();
    set.insert::<TraceSummary>();
    set.insert::<SpanSummary>();
    set.insert::<TraceDetail>();
    set.insert::<SpanDetail>();
    set.insert::<SessionSummary>();
    set.insert::<SessionDetail>();
    set.insert::<EndUserSummary>();
    set.insert::<EndUserDetail>();
    set.insert::<Score>();
    set.insert::<SchemaResponse>();
    set.insert::<SavedView>();
    set.insert::<CreateSavedView>();
    set.insert::<UpdateSavedView>();
    set.insert::<MetricsResponse>();
    set.insert_page::<TraceSummary>("TracePage");
    set.insert_page::<SpanSummary>("SpanPage");
    set.insert_page::<SessionSummary>("SessionPage");
    set.insert_page::<EndUserSummary>("EndUserPage");
    set.insert_page::<Score>("ScorePage");
    set.insert_page::<Project>("ProjectPage");
    set.insert_page::<SavedView>("SavedViewPage");
    set
}

impl SchemaSet {
    fn insert<T: ToSchema>(&mut self) {
        let mut nested = Vec::new();
        T::schemas(&mut nested);
        let name = T::name().into_owned();
        if !nested.iter().any(|(existing, _)| existing == &name) {
            nested.insert(0, (name, T::schema()));
        }
        for (name, schema) in nested {
            self.add(name, schema);
        }
    }

    fn insert_page<T: PartialSchema + ToSchema + 'static>(&mut self, name: &str) {
        self.add(name.to_owned(), <Page<T> as PartialSchema>::schema());
        self.insert::<T>();
    }

    fn add(&mut self, name: String, schema: RefOr<Schema>) {
        match self.schemas.get(&name) {
            Some(existing) => match (schema_json(existing), schema_json(&schema)) {
                (Some(previous), Some(next)) if previous == next => {}
                (Some(_), Some(_)) if is_ref(existing) && !is_ref(&schema) => {
                    self.schemas.insert(name, schema);
                }
                (Some(_), Some(_)) if !is_ref(existing) && is_ref(&schema) => {}
                _ => {
                    self.clash.get_or_insert(name);
                }
            },
            None => {
                self.schemas.insert(name, schema);
            }
        }
    }
}

fn schema_json(schema: &RefOr<Schema>) -> Option<serde_json::Value> {
    serde_json::to_value(schema).ok()
}

fn is_ref(schema: &RefOr<Schema>) -> bool {
    matches!(schema, RefOr::Ref(_))
}

/// The OpenAPI document. Spec 09.
pub fn openapi() -> OpenApi {
    let schemas = collect_schemas();
    OpenApiBuilder::new()
        .info(
            InfoBuilder::new()
                .title(concat!(llmobs_core::llmobs_name!(), " API"))
                .version(env!("CARGO_PKG_VERSION"))
                .description(Some(
                    "Observability read API. The rules are in docs/spec/09-api.md.",
                ))
                .build(),
        )
        .tags(Some([
            tag("auth", "Login methods and the current user."),
            tag("projects", "Projects the caller can see."),
            tag("traces", "Trace list, detail, and live streams."),
            tag("spans", "Span list and detail."),
            tag("sessions", "Session list and detail."),
            tag("users", "End users."),
            tag("scores", "Scores attached to spans, traces, and sessions."),
            tag("schema", "Filter and SQL catalog."),
            tag("saved_views", "Saved list views and SQL queries."),
            tag("metrics", "Dashboard metrics."),
            tag("media", "Extracted media blobs."),
        ]))
        .components(Some(components(&schemas)))
        .security(Some([
            SecurityRequirement::new("session_cookie", [] as [&str; 0]),
            SecurityRequirement::new("bearer", [] as [&str; 0]),
        ]))
        .paths(paths())
        .build()
}

fn components(schemas: &SchemaSet) -> Components {
    ComponentsBuilder::new()
        .schemas_from_iter(schemas.schemas.clone())
        .security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                COOKIE_NAME,
                "Browser session cookie.",
            ))),
        )
        .security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "A personal access token (`pat-…`) or a secret key (`sk-…`).",
                    ))
                    .build(),
            ),
        )
        .build()
}

fn tag(name: &str, description: &str) -> Tag {
    TagBuilder::new()
        .name(name)
        .description(Some(description))
        .build()
}

fn paths() -> Paths {
    let paths = PathsBuilder::new();
    let paths = auth_paths(paths);
    let paths = project_paths(paths);
    let paths = trace_paths(paths);
    let paths = session_paths(paths);
    let paths = score_paths(paths);
    let paths = saved_view_paths(paths);
    media_paths(paths).build()
}

fn auth_paths(paths: PathsBuilder) -> PathsBuilder {
    let config = operation("get_auth_config", "auth", "Get the auth config")
        .securities(Some(Vec::new()))
        .responses(responses(
            "200",
            json_of::<AuthConfig>("Login methods the UI should offer."),
            &[],
        ))
        .build();
    let me = operation("get_me", "auth", "Get the current user")
        .responses(responses(
            "200",
            json_of::<Me>("The signed-in user and org memberships."),
            &[401, 429],
        ))
        .build();
    paths
        .path("/api/auth/config", PathItem::new(HttpMethod::Get, config))
        .path("/api/auth/me", PathItem::new(HttpMethod::Get, me))
}

fn project_paths(paths: PathsBuilder) -> PathsBuilder {
    let list = operation("list_projects", "projects", "List projects")
        .parameters(Some(with_query::<PageParams>(vec![path_string(
            "org",
            "Org UUID or slug.",
        )])))
        .responses(responses(
            "200",
            json_named("ProjectPage", "One page of projects."),
            QUERY_ERRORS,
        ))
        .build();
    let get = operation("get_project", "projects", "Get a project")
        .parameters(Some(vec![project_param()]))
        .responses(responses(
            "200",
            json_of::<Project>("The project."),
            AUTH_ERRORS,
        ))
        .build();
    paths
        .path(
            "/api/v1/orgs/{org}/projects",
            PathItem::new(HttpMethod::Get, list),
        )
        .path(
            "/api/v1/projects/{project}",
            PathItem::new(HttpMethod::Get, get),
        )
}

fn trace_paths(paths: PathsBuilder) -> PathsBuilder {
    let list = operation("list_traces", "traces", "List traces")
        .description(Some(EXCLUSIONS))
        .parameters(Some(with_query::<TraceListParams>(vec![project_param()])))
        .responses(responses(
            "200",
            json_named("TracePage", "One page of traces."),
            QUERY_ERRORS,
        ))
        .build();
    let stream = operation("stream_traces", "traces", "Stream traces")
        .description(Some(format!(
            "{EXCLUSIONS} Server-sent events: `trace` data is a TraceSummary; `resync` and `ping` data are `{{}}`. `ping` is sent every 15 seconds."
        )))
        .parameters(Some(with_query::<TraceStreamParams>(vec![project_param()])))
        .responses(responses(
            "200",
            sse("Live trace summaries."),
            QUERY_ERRORS,
        ))
        .build();
    let detail = operation("get_trace", "traces", "Get a trace")
        .description(Some(
            "Spans are a flat pre-order list: roots first, children sorted by start time then span id. `depth` is 0 at a root. `detached` is true when the parent is missing or the span sits in a parent cycle. At most 20000 spans; `truncated` is true when more exist.",
        ))
        .parameters(Some(with_query::<TraceDetailParams>(vec![
            project_param(),
            path_ref("trace_id", "TraceId", "Trace id, 16 bytes as hex."),
        ])))
        .responses(responses(
            "200",
            json_of::<TraceDetail>("The trace and its span tree."),
            QUERY_ERRORS,
        ))
        .build();
    let span_stream = operation("stream_trace", "traces", "Stream a trace")
        .description(Some(
            "Server-sent events: `span` data is a SpanSummary; `resync` and `ping` data are `{}`. `ping` is sent every 15 seconds. The stream closes after 10 minutes idle.",
        ))
        .parameters(Some(vec![
            project_param(),
            path_ref("trace_id", "TraceId", "Trace id, 16 bytes as hex."),
        ]))
        .responses(responses(
            "200",
            sse("Live spans of one trace."),
            AUTH_ERRORS,
        ))
        .build();
    let span = operation("get_span", "spans", "Get a span")
        .parameters(Some(vec![
            project_param(),
            path_ref("trace_id", "TraceId", "Trace id, 16 bytes as hex."),
            path_ref("span_id", "SpanId", "Span id, 8 bytes as hex."),
        ]))
        .responses(responses(
            "200",
            json_of::<SpanDetail>("The span, including payloads."),
            AUTH_ERRORS,
        ))
        .build();
    let spans = operation("list_spans", "spans", "List spans")
        .description(Some(EXCLUSIONS))
        .parameters(Some(with_query::<SpanListParams>(vec![project_param()])))
        .responses(responses(
            "200",
            json_named("SpanPage", "One page of spans."),
            QUERY_ERRORS,
        ))
        .build();
    paths
        .path(
            format!("{PROJECT}/traces"),
            PathItem::new(HttpMethod::Get, list),
        )
        .path(
            format!("{PROJECT}/traces/stream"),
            PathItem::new(HttpMethod::Get, stream),
        )
        .path(
            format!("{PROJECT}/traces/{{trace_id}}"),
            PathItem::new(HttpMethod::Get, detail),
        )
        .path(
            format!("{PROJECT}/traces/{{trace_id}}/stream"),
            PathItem::new(HttpMethod::Get, span_stream),
        )
        .path(
            format!("{PROJECT}/traces/{{trace_id}}/spans/{{span_id}}"),
            PathItem::new(HttpMethod::Get, span),
        )
        .path(
            format!("{PROJECT}/spans"),
            PathItem::new(HttpMethod::Get, spans),
        )
}

fn session_paths(paths: PathsBuilder) -> PathsBuilder {
    let sessions = operation("list_sessions", "sessions", "List sessions")
        .parameters(Some(with_query::<SessionListParams>(vec![project_param()])))
        .responses(responses(
            "200",
            json_named("SessionPage", "One page of sessions."),
            QUERY_ERRORS,
        ))
        .build();
    let session = operation("get_session", "sessions", "Get a session")
        .description(Some(
            "Turns are the session's traces by start time, at most 1000. `input` and `output` are the root span's payloads, omitted when the trace has no root.",
        ))
        .parameters(Some(vec![
            project_param(),
            path_string("session_id", "Session id."),
        ]))
        .responses(responses(
            "200",
            json_of::<SessionDetail>("The session and its turns."),
            AUTH_ERRORS,
        ))
        .build();
    let users = operation("list_users", "users", "List users")
        .description(Some(
            "The filter runs on the `traces` view before traces are grouped by user.",
        ))
        .parameters(Some(with_query::<UserListParams>(vec![project_param()])))
        .responses(responses(
            "200",
            json_named("EndUserPage", "One page of end users."),
            QUERY_ERRORS,
        ))
        .build();
    let user = operation("get_user", "users", "Get a user")
        .description(Some("Sessions are the user's 100 most recent."))
        .parameters(Some(vec![
            project_param(),
            path_string("user_id", "End-user id."),
        ]))
        .responses(responses(
            "200",
            json_of::<EndUserDetail>("The user and recent sessions."),
            AUTH_ERRORS,
        ))
        .build();
    paths
        .path(
            format!("{PROJECT}/sessions"),
            PathItem::new(HttpMethod::Get, sessions),
        )
        .path(
            format!("{PROJECT}/sessions/{{session_id}}"),
            PathItem::new(HttpMethod::Get, session),
        )
        .path(
            format!("{PROJECT}/users"),
            PathItem::new(HttpMethod::Get, users),
        )
        .path(
            format!("{PROJECT}/users/{{user_id}}"),
            PathItem::new(HttpMethod::Get, user),
        )
}

fn score_paths(paths: PathsBuilder) -> PathsBuilder {
    let scores = operation("list_scores", "scores", "List scores")
        .parameters(Some(with_query::<ScoreListParams>(vec![project_param()])))
        .responses(responses(
            "200",
            json_named("ScorePage", "One page of scores."),
            QUERY_ERRORS,
        ))
        .build();
    let schema = operation("get_schema", "schema", "Get the schema")
        .parameters(Some(vec![project_param()]))
        .responses(responses(
            "200",
            json_of::<SchemaResponse>(
                "Views and functions the filter builder and SQL console show.",
            ),
            AUTH_ERRORS,
        ))
        .build();
    let metrics = operation("get_metrics", "metrics", "Get metrics")
        .description(Some(
            "Series have one point per bucket. Latencies and TTFT are nanoseconds. Costs are USD. Results are cached for 10 seconds.",
        ))
        .parameters(Some(with_query::<MetricsParams>(vec![project_param()])))
        .responses(responses(
            "200",
            json_of::<MetricsResponse>("Dashboard metrics."),
            QUERY_ERRORS,
        ))
        .build();
    paths
        .path(
            format!("{PROJECT}/scores"),
            PathItem::new(HttpMethod::Get, scores),
        )
        .path(
            format!("{PROJECT}/schema"),
            PathItem::new(HttpMethod::Get, schema),
        )
        .path(
            format!("{PROJECT}/metrics"),
            PathItem::new(HttpMethod::Get, metrics),
        )
}

fn saved_view_paths(paths: PathsBuilder) -> PathsBuilder {
    let list = operation("list_saved_views", "saved_views", "List saved views")
        .parameters(Some(with_query::<SavedViewListParams>(vec![
            project_param(),
        ])))
        .responses(responses(
            "200",
            json_named("SavedViewPage", "One page of saved views."),
            QUERY_ERRORS,
        ))
        .build();
    let create = operation("create_saved_view", "saved_views", "Create a saved view")
        .parameters(Some(vec![project_param()]))
        .request_body(Some(json_body("CreateSavedView")))
        .responses(responses(
            "201",
            json_of::<SavedView>("The created saved view."),
            QUERY_ERRORS,
        ))
        .build();
    let update = operation("update_saved_view", "saved_views", "Update a saved view")
        .parameters(Some(vec![
            project_param(),
            path_uuid("id", "Saved view id."),
        ]))
        .request_body(Some(json_body("UpdateSavedView")))
        .responses(responses(
            "200",
            json_of::<SavedView>("The updated saved view."),
            QUERY_ERRORS,
        ))
        .build();
    let delete = operation("delete_saved_view", "saved_views", "Delete a saved view")
        .parameters(Some(vec![
            project_param(),
            path_uuid("id", "Saved view id."),
        ]))
        .responses(responses(
            "204",
            ResponseBuilder::new().description("Deleted.").build(),
            AUTH_ERRORS,
        ))
        .build();
    paths
        .path(
            format!("{PROJECT}/saved-views"),
            PathItem::new(HttpMethod::Get, list),
        )
        .path(
            format!("{PROJECT}/saved-views"),
            PathItem::new(HttpMethod::Post, create),
        )
        .path(
            format!("{PROJECT}/saved-views/{{id}}"),
            PathItem::new(HttpMethod::Patch, update),
        )
        .path(
            format!("{PROJECT}/saved-views/{{id}}"),
            PathItem::new(HttpMethod::Delete, delete),
        )
}

fn media_paths(paths: PathsBuilder) -> PathsBuilder {
    let media = operation("get_media", "media", "Get a media blob")
        .description(Some(
            "Supports `Range`. A satisfiable range returns 206; an unsatisfiable range returns 416. `Cache-Control` is `private, immutable`. The response `Content-Type` is the blob's MIME type.",
        ))
        .parameters(Some(vec![
            project_param(),
            path_sha256(),
            range_header(),
        ]))
        .responses(responses_with(
            [
                ("200", binary("The whole blob.")),
                ("206", binary("The requested byte range.")),
            ],
            &[401, 403, 404, 416, 429],
        ))
        .build();
    paths.path(
        format!("{PROJECT}/media/{{sha256}}"),
        PathItem::new(HttpMethod::Get, media),
    )
}

const EXCLUSIONS: &str = concat!(
    "Evaluation traces (`environment = ",
    llmobs_core::llmobs_name!("-eval"),
    "`) and experiment traces are excluded unless `include_eval` or `include_experiments` is set."
);

fn operation(id: &str, tag_name: &str, summary: &str) -> OperationBuilder {
    OperationBuilder::new()
        .operation_id(Some(id))
        .tag(tag_name)
        .summary(Some(summary))
}

fn project_param() -> Parameter {
    path_string(
        "project",
        "Project UUID, URL-encoded org-slug/project-slug, or `current`.",
    )
}

fn path_string(name: &str, description: &str) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .description(Some(description))
        .schema(Some(string_schema()))
        .build()
}

fn path_ref(name: &str, schema_name: &str, description: &str) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .description(Some(description))
        .schema(Some(Ref::from_schema_name(schema_name)))
        .build()
}

fn path_uuid(name: &str, description: &str) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .description(Some(description))
        .schema(Some(
            ObjectBuilder::new()
                .schema_type(utoipa::openapi::schema::Type::String)
                .format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid))),
        ))
        .build()
}

fn path_sha256() -> Parameter {
    ParameterBuilder::new()
        .name("sha256")
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .description(Some("SHA-256 of the media blob, lowercase hex."))
        .schema(Some(
            ObjectBuilder::new()
                .schema_type(utoipa::openapi::schema::Type::String)
                .pattern(Some("^[0-9a-f]{64}$")),
        ))
        .build()
}

fn range_header() -> Parameter {
    ParameterBuilder::new()
        .name("Range")
        .parameter_in(ParameterIn::Header)
        .required(Required::False)
        .description(Some(
            "Optional byte range. A satisfiable range returns 206; an unsatisfiable range returns 416.",
        ))
        .schema(Some(string_schema()))
        .build()
}

fn string_schema() -> ObjectBuilder {
    ObjectBuilder::new().schema_type(utoipa::openapi::schema::Type::String)
}

fn with_query<T: IntoParams>(mut path_params: Vec<Parameter>) -> Vec<Parameter> {
    path_params.extend(T::into_params(|| Some(ParameterIn::Query)));
    path_params
}

fn json_of<T: ToSchema>(description: &str) -> Response {
    json_named(&T::name(), description)
}

fn json_named(name: &str, description: &str) -> Response {
    ResponseBuilder::new()
        .description(description)
        .content(
            "application/json",
            Content::new(Some(Ref::from_schema_name(name))),
        )
        .build()
}

fn json_body(name: &str) -> utoipa::openapi::request_body::RequestBody {
    RequestBodyBuilder::new()
        .required(Some(Required::True))
        .content(
            "application/json",
            Content::new(Some(Ref::from_schema_name(name))),
        )
        .build()
}

fn sse(description: &str) -> Response {
    ResponseBuilder::new()
        .description(description)
        .content("text/event-stream", Content::new(Some(string_schema())))
        .build()
}

fn binary(description: &str) -> Response {
    ResponseBuilder::new()
        .description(description)
        .content(
            "application/octet-stream",
            Content::new(Some(
                ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::String)
                    .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
                    .description(Some(
                        "Bytes of the blob. The response Content-Type is the blob's MIME type.",
                    )),
            )),
        )
        .build()
}

fn responses(status: &str, success: Response, errors: &[u16]) -> Responses {
    responses_with([(status, success)], errors)
}

fn responses_with<const N: usize>(success: [(&str, Response); N], errors: &[u16]) -> Responses {
    let mut builder = ResponsesBuilder::new();
    for (status, response) in success {
        builder = builder.response(status, response);
    }
    for code in errors {
        builder = builder.response(code.to_string(), problem(*code));
    }
    builder.build()
}

fn problem(code: u16) -> Response {
    let description = match code {
        400 => "The request is malformed or a filter is invalid.",
        401 => "No credentials, or the credentials are invalid.",
        403 => "The credentials do not allow this operation.",
        404 => "The resource does not exist.",
        416 => "The Range header does not overlap the blob.",
        429 => "Too many requests.",
        _ => "Request failed.",
    };
    ResponseBuilder::new()
        .description(description)
        .content(
            "application/problem+json",
            Content::new(Some(Ref::from_schema_name(Problem::name()))),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::collect_schemas;

    #[test]
    fn component_names_are_unique_and_roles_differ() {
        let set = collect_schemas();
        assert!(set.clash.is_none(), "schema clash: {:?}", set.clash);
        let message = serde_json::to_value(set.schemas.get("MessageRole").unwrap()).unwrap();
        let org = serde_json::to_value(set.schemas.get("OrgRole").unwrap()).unwrap();
        assert_ne!(message, org);
        assert!(message.to_string().contains("MessageRole") || message["type"] == "string");
        assert!(org["enum"].as_array().is_some());
    }
}
