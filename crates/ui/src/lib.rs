//! The web UI, embedded at build time (D-G-18, D-G-35).

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{
    ACCEPT_ENCODING, ALLOW, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, ETAG,
    IF_NONE_MATCH, VARY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::Response;

/// One embedded file.
#[derive(Debug)]
struct Asset {
    path: &'static str,
    content_type: &'static str,
    etag: &'static str,
    body: &'static [u8],
    gzip: Option<&'static [u8]>,
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

/// Whether `web/dist` was present at build time.
pub fn embedded() -> bool {
    BUILT
}

/// Answers every `GET` and `HEAD`: an embedded file, `index.html` for an extension-less path
/// that matches no file (the SPA fallback), or `503` when no UI was embedded.
pub fn router() -> Router {
    router_with(BUILT, ASSETS)
}

fn router_with(built: bool, assets: &'static [Asset]) -> Router {
    Router::new().fallback(move |request: Request| async move {
        with_security_headers(serve(built, assets, &request))
    })
}

fn serve(built: bool, assets: &'static [Asset], request: &Request) -> Response {
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        let mut headers = HeaderMap::new();
        headers.insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return reply(StatusCode::METHOD_NOT_ALLOWED, headers, Body::empty());
    }
    if !built {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return reply(
            StatusCode::SERVICE_UNAVAILABLE,
            headers,
            Body::from("web UI not built: run `just build-release`\n"),
        );
    }

    let path = request.uri().path();
    let wanted = if path == "/" { "/index.html" } else { path };
    let asset = match assets.iter().find(|asset| asset.path == wanted) {
        Some(asset) => asset,
        None => {
            let last_segment = path.rsplit('/').next().unwrap_or_default();
            let looks_like_file = path.starts_with("/assets/") || last_segment.contains('.');
            match assets.iter().find(|asset| asset.path == "/index.html") {
                Some(index) if !looks_like_file => index,
                _ => return reply(StatusCode::NOT_FOUND, HeaderMap::new(), Body::empty()),
            }
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(ETAG, HeaderValue::from_static(asset.etag));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if asset.path.starts_with("/assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    if asset.gzip.is_some() {
        headers.insert(VARY, HeaderValue::from_static("accept-encoding"));
    }

    let if_none_match = request
        .headers()
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    if if_none_match == Some(asset.etag) {
        return reply(StatusCode::NOT_MODIFIED, headers, Body::empty());
    }

    let (body, gzipped) = match asset.gzip {
        Some(gzip) if accepts_gzip(request.headers()) => (gzip, true),
        _ => (asset.body, false),
    };
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
    if gzipped {
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    headers.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
    let payload = if head {
        Body::empty()
    } else {
        Body::from(body)
    };
    reply(StatusCode::OK, headers, payload)
}

/// Whether `Accept-Encoding` lists `gzip` with a non-zero quality.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get_all(ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|item| {
            let mut params = item.split(';');
            let coding = params.next().unwrap_or_default().trim();
            coding.eq_ignore_ascii_case("gzip")
                && params.all(|param| match param.trim().split_once('=') {
                    Some((name, q)) if name.trim().eq_ignore_ascii_case("q") => {
                        q.trim().parse::<f32>().is_ok_and(|q| q > 0.0)
                    }
                    _ => true,
                })
        })
}

fn reply(status: StatusCode, headers: HeaderMap, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn with_security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(CONTENT_SECURITY_POLICY_VALUE),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use axum::body::Body;
    use axum::http::{HeaderMap, Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    static TEST_ASSETS: &[Asset] = &[
        Asset {
            path: "/index.html",
            content_type: "text/html; charset=utf-8",
            etag: "\"aaaa\"",
            body: b"<html>app</html>",
            gzip: None,
        },
        Asset {
            path: "/assets/app.js",
            content_type: "text/javascript; charset=utf-8",
            etag: "\"bbbb\"",
            body: b"console.log(1)",
            gzip: Some(b"GZ"),
        },
        Asset {
            path: "/favicon.svg",
            content_type: "image/svg+xml",
            etag: "\"cccc\"",
            body: b"<svg/>",
            gzip: None,
        },
    ];

    async fn get(
        app: &Router,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, body.to_vec())
    }

    fn test_app() -> Router {
        router_with(true, TEST_ASSETS)
    }

    fn assert_security_headers(headers: &HeaderMap) {
        assert_eq!(
            headers.get("content-security-policy").unwrap(),
            CONTENT_SECURITY_POLICY_VALUE
        );
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("referrer-policy").unwrap(), "same-origin");
    }

    #[tokio::test]
    async fn serves_hashed_assets_with_gzip_and_long_cache() {
        let app = test_app();
        let (status, headers, body) = get(&app, Method::GET, "/assets/app.js", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "text/javascript; charset=utf-8");
        assert_eq!(
            headers[CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(headers[VARY], "accept-encoding");
        assert_eq!(body, b"console.log(1)");
        assert_security_headers(&headers);

        let (_, headers, body) = get(
            &app,
            Method::GET,
            "/assets/app.js",
            &[("accept-encoding", "br, gzip")],
        )
        .await;
        assert_eq!(headers[CONTENT_ENCODING], "gzip");
        assert_eq!(body, b"GZ");

        let (_, headers, body) = get(
            &app,
            Method::GET,
            "/assets/app.js",
            &[("accept-encoding", "gzip;q=0")],
        )
        .await;
        assert!(headers.get(CONTENT_ENCODING).is_none());
        assert_eq!(body, b"console.log(1)");

        let (status, headers, body) = get(
            &app,
            Method::GET,
            "/assets/app.js",
            &[("accept-encoding", "gzip"), ("if-none-match", "\"bbbb\"")],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert_eq!(headers[ETAG], "\"bbbb\"");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn serves_index_for_root_and_deep_links_without_caching() {
        let app = test_app();
        for uri in ["/", "/some/deep/link"] {
            let (status, headers, body) = get(&app, Method::GET, uri, &[]).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(headers[CACHE_CONTROL], "no-cache", "{uri}");
            assert_eq!(body, b"<html>app</html>", "{uri}");
            assert_security_headers(&headers);
        }
    }

    #[tokio::test]
    async fn serves_top_level_files_and_404s_missing_ones() {
        let app = test_app();
        let (status, headers, body) = get(&app, Method::GET, "/favicon.svg", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CACHE_CONTROL], "no-cache");
        assert_eq!(body, b"<svg/>");

        for uri in ["/assets/missing.js", "/missing.png"] {
            let (status, _, body) = get(&app, Method::GET, uri, &[]).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert!(body.is_empty(), "{uri}");
        }
    }

    #[tokio::test]
    async fn answers_not_modified_for_the_index_etag() {
        let app = test_app();
        let (status, headers, body) =
            get(&app, Method::GET, "/", &[("if-none-match", "\"aaaa\"")]).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert_eq!(headers[ETAG], "\"aaaa\"");
        assert!(body.is_empty());
        assert_security_headers(&headers);
    }

    #[tokio::test]
    async fn head_reports_the_full_length_without_a_body() {
        let app = test_app();
        let (status, headers, body) = get(&app, Method::HEAD, "/assets/app.js", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_LENGTH], "14");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn rejects_other_methods_with_405() {
        let app = test_app();
        let (status, headers, _) = get(&app, Method::POST, "/", &[]).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(headers[ALLOW], "GET, HEAD");
        assert_security_headers(&headers);
    }

    #[tokio::test]
    async fn answers_503_when_no_ui_was_embedded() {
        let app = router_with(false, &[]);
        let (status, headers, body) = get(&app, Method::GET, "/", &[]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(headers[CONTENT_TYPE], "text/plain; charset=utf-8");
        assert_eq!(body, b"web UI not built: run `just build-release`\n");
        assert_security_headers(&headers);
    }

    #[test]
    fn embedded_matches_the_web_build_on_disk() {
        let index = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist/index.html");
        assert_eq!(embedded(), index.exists());
    }
}
