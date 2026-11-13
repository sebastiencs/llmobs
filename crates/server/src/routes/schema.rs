//! Filter and SQL schema (spec 05 § Logical views, spec 09 § Observability data).

use axum::http::Method;
use axum::routing::get;

use crate::api::convert;
use crate::api::schema::SchemaResponse;
use crate::auth::roles::ProjectPerm;
use crate::http::access::{Access, RouteDef};
use crate::http::error;

/// Schema route. Documented and needs project view. It opens no query.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::GET,
        path: "/api/v1/projects/{project}/schema",
        access: Access::Project {
            perm: ProjectPerm::View,
            session_only: false,
        },
        documented: true,
        handler: get(get_schema),
    }]
}

async fn get_schema() -> error::Json<SchemaResponse> {
    error::Json(convert::schema_response())
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};

    use crate::api::schema::SchemaResponse;
    use crate::test_support::TestApp;

    #[tokio::test(flavor = "multi_thread")]
    async fn schema_is_the_static_fixture() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/schema", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let expected: SchemaResponse =
            serde_json::from_str(include_str!("../../../../fixtures/api/static/schema.json"))
                .unwrap();
        let body: SchemaResponse = response.json();
        assert_eq!(body, expected);
    }
}
