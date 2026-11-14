# F1 — UI fallback for dotted deep links, `If-None-Match`, CSP (M-1, m-8)

Read [README.md](README.md) first. Independent of F2–F4. F5 depends on it.

**Goal:**
- A browser page load of any app path gets `index.html`, even when the last path segment contains a dot: `/acme/proj/users/alice%40example.com` and `/acme/proj/sessions/chat.2026-01` are user and session ids, not files. Today the fallback answers a bare `404` for any missing path whose last segment contains `.`.
- `If-None-Match` follows RFC 9110: a list of entity tags, weak tags (`W/"…"`) and `*` all match.
- The CSP adds `base-uri 'none'` and `form-action 'self'`, which `default-src` does not cover.

**Edit only:**
- `crates/ui/src/lib.rs`;
- `docs/spec/12-operations.md`;
- `docs/spec/11-ui.md`;
- `docs/mvp-decisions.md` (row D-G-35 only).

**Commit:** `Serve the app for dotted deep links and tighten the UI headers`

## Background

In `crates/ui/src/lib.rs`:
- `fn serve(built, assets, request) -> Response` looks the path up in `assets`. When nothing matches it computes `last_segment` and `looks_like_file = path.starts_with("/assets/") || last_segment.contains('.')`, and serves `/index.html` only when `!looks_like_file`; otherwise a bare `404`.
- The `304` check is `if if_none_match == Some(asset.etag)`, with `if_none_match` the raw header string. `asset.etag` is a strong, quoted tag such as `"\"aaaa\""`.
- `const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";`
- Tests live in `mod tests` at the bottom, with the helper `get(app, method, uri, headers) -> (StatusCode, HeaderMap, Vec<u8>)`, `test_app()` over `TEST_ASSETS` (`/index.html` with etag `"aaaa"`, `/assets/app.js` with etag `"bbbb"` and a gzip copy, `/favicon.svg` with etag `"cccc"`), and `assert_security_headers`.

The request path is the raw, percent-encoded path: `alice@example.com` arrives as `alice%40example.com`.

## Changes

### 1. Which missing paths are files

Add, above `fn serve`:

```rust
/// Extensions of the files a web build emits. A missing path whose last segment ends in one
/// of these is a missing file (`404`), unless the request is a page load.
const FILE_EXTENSIONS: &[&str] = &[
    "css", "gif", "html", "ico", "jpeg", "jpg", "js", "json", "map", "mjs", "png", "svg", "txt",
    "wasm", "webmanifest", "webp", "woff", "woff2",
];

/// Whether a path that matches no embedded file names a missing file (`404`) rather than an
/// app route (`index.html`, D-G-35).
fn looks_like_file(headers: &HeaderMap, path: &str) -> bool {
    if path.starts_with("/assets/") {
        return true;
    }
    if is_page_load(headers) {
        return false;
    }
    let last_segment = path.rsplit('/').next().unwrap_or_default();
    last_segment.rsplit_once('.').is_some_and(|(_, extension)| {
        FILE_EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// A browser page load: `Sec-Fetch-Mode: navigate`, or an `Accept` header that lists
/// `text/html`.
fn is_page_load(headers: &HeaderMap) -> bool {
    let navigate = headers
        .get("sec-fetch-mode")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"navigate"));
    navigate
        || headers
            .get_all(ACCEPT)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|item| {
                item.split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("text/html")
            })
}
```

Add `ACCEPT` to the `axum::http::header::{…}` import.

In `serve`, replace the `None => { … }` arm of the asset lookup with:

```rust
        None => match assets.iter().find(|asset| asset.path == "/index.html") {
            Some(index) if !looks_like_file(request.headers(), path) => index,
            _ => return reply(StatusCode::NOT_FOUND, HeaderMap::new(), Body::empty()),
        },
```

### 2. `If-None-Match`

Add below `accepts_gzip`:

```rust
/// Whether `If-None-Match` matches `etag`: `*`, or any listed tag equal to it under the weak
/// comparison of RFC 9110 § 13.1.2 (a `W/` prefix is ignored).
fn none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag)
}
```

In `serve`, delete the `let if_none_match = …;` statement and replace `if if_none_match == Some(asset.etag) {` with `if none_match(request.headers(), asset.etag) {`.

### 3. CSP

Replace the constant with:

```rust
const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";
```

`assert_security_headers` compares against the constant, so it needs no change.

## Tests (in `mod tests`)

1. Rename `serves_top_level_files_and_404s_missing_ones` to `serves_top_level_files_and_404s_missing_ones_outside_page_loads`. Keep its body, and change its `for uri in [...]` list to `["/assets/missing.js", "/missing.png", "/a/b/report.json"]`.
2. New `serves_index_for_dotted_deep_links`: with no extra headers, for each of `/acme/proj/users/alice%40example.com`, `/acme/proj/sessions/chat.2026-01` and `/acme/proj/users/v1.2`, `GET` answers `200` with body `b"<html>app</html>"` and `Cache-Control: no-cache`.
3. New `a_page_load_gets_index_even_for_a_file_extension`: `GET /acme/proj/users/report.json` with `("sec-fetch-mode", "navigate")` answers `200` and the index body; the same path with `("accept", "text/html,application/xhtml+xml;q=0.9")` answers `200` and the index body; `GET /assets/missing.js` with `("sec-fetch-mode", "navigate")` still answers `404`.
4. Extend `answers_not_modified_for_the_index_etag`: after the existing assertions, each of these `If-None-Match` values on `GET /` answers `304`: `W/"aaaa"`, `"zzzz", "aaaa"`, `*`. The value `"zzzz"` answers `200`.
5. New `the_csp_restricts_base_and_form_targets`: `GET /` has a `content-security-policy` header containing `base-uri 'none'` and `form-action 'self'`.

## Docs

### `docs/spec/12-operations.md`

In the line that starts `- Content-Security-Policy on the UI:`, replace the quoted policy with:

```
`default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'`
```

### `docs/spec/11-ui.md`

In the first paragraph, replace the sentence `Built output embedded in the binary with `memory-serve` (brotli/gzip precompressed, ETags, SPA fallback).` with:

```
Built output embedded in the binary by the `llmobs-ui` crate (gzip precompressed, ETags, SPA fallback; D-G-35). Every path that is not a file serves the app, including ids with a dot such as an email address.
```

### `docs/mvp-decisions.md`, row D-G-35

Replace its sentence `Missing `/assets/*` files and paths with an extension are `404`; other paths fall back to `index.html` with `no-cache`.` with:

```
Missing `/assets/*` files are `404`. Another missing path is `404` only when its last segment ends in a web-build file extension (`.js`, `.css`, `.png`, `.json`, …) and the request is not a page load (`Sec-Fetch-Mode: navigate` or `Accept: text/html`); every other path falls back to `index.html` with `no-cache`, so ids with a dot (emails) deep-link (G review M-1).
```

## Done when

- `cargo test -p llmobs-ui` passes, with the five tests above.
- `just check` passes.
