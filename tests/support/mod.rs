//! Shared helpers for the integration suites.
//!
//! Tests deliberately use unchecked `sqlx::query` for arranging and asserting rather than
//! the `query!` macros, so the `.sqlx` cache stays small and the tests exercise the real
//! repository functions instead of a parallel copy of their SQL.

#![allow(dead_code, reason = "each integration binary uses a different subset")]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use rustle::{AppState, Config, build_router};
use sqlx::PgPool;
use tower::ServiceExt;

/// A router backed by a `#[sqlx::test]` scratch database and the documented defaults.
pub fn app(pool: PgPool) -> Router {
    build_router(state(pool))
}

pub fn state(pool: PgPool) -> AppState {
    AppState::new(test_config(), pool)
}

/// Everything at its default except `DATABASE_URL`, which `Config` requires. The pool is
/// supplied by the test harness, so the value itself is never dialled.
pub fn test_config() -> Config {
    Config::from_lookup(&|name| match name {
        "DATABASE_URL" => Some("postgres://rustle@localhost/rustle".to_owned()),
        _ => None,
    })
    .expect("test configuration should be valid")
}

pub async fn get(router: Router, uri: &str) -> Response<Body> {
    send(router, Request::builder().uri(uri), "").await
}

/// A GET carrying a session cookie.
pub async fn get_as(router: Router, uri: &str, cookie: &str) -> Response<Body> {
    send(
        router,
        Request::builder().uri(uri).header(header::COOKIE, cookie),
        "",
    )
    .await
}

/// A same-origin form POST, which is what a browser on this instance would send.
pub async fn post_form(router: Router, uri: &str, body: &str) -> Response<Body> {
    send(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("sec-fetch-site", "same-origin"),
        body,
    )
    .await
}

/// A same-origin form POST carrying a session cookie.
pub async fn post_form_as(router: Router, uri: &str, cookie: &str, body: &str) -> Response<Body> {
    send(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("sec-fetch-site", "same-origin")
            .header(header::COOKIE, cookie),
        body,
    )
    .await
}

async fn send(router: Router, builder: axum::http::request::Builder, body: &str) -> Response<Body> {
    let request = if body.is_empty() {
        builder.body(Body::empty())
    } else {
        builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
    };

    router
        .oneshot(request.expect("the test request should be well formed"))
        .await
        .expect("router should not fail")
}

pub async fn body_string(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).expect("responses are utf-8")
}

/// The `rustle_session=...` pair from a `Set-Cookie`, ready to send back.
pub fn session_cookie(response: &Response<Body>) -> Option<String> {
    let value = response.headers().get(header::SET_COOKIE)?.to_str().ok()?;
    let pair = value.split(';').next()?;
    // A cleared cookie has an empty value; that is not a usable session.
    pair.strip_prefix("rustle_session=")
        .filter(|token| !token.is_empty())
        .map(|_| pair.to_owned())
}

pub fn location(response: &Response<Body>) -> Option<String> {
    response
        .headers()
        .get(header::LOCATION)?
        .to_str()
        .ok()
        .map(str::to_owned)
}

pub fn assert_redirects_to(response: &Response<Body>, expected: &str) {
    assert!(
        response.status() == StatusCode::SEE_OTHER
            || response.status() == StatusCode::TEMPORARY_REDIRECT
            || response.status() == StatusCode::FOUND,
        "expected a redirect, got {}",
        response.status()
    );
    assert_eq!(location(response).as_deref(), Some(expected));
}

/// A router plus the state behind it, for tests that need the generated setup token.
pub fn app_with_state(pool: PgPool) -> (Router, AppState) {
    let state = state(pool);
    (build_router(state.clone()), state)
}

/// The CSRF token of the one live session. Read straight from the table rather than
/// scraped out of a page, so a CSRF test does not depend on template markup.
pub async fn csrf_token(pool: &PgPool) -> String {
    sqlx::query_scalar::<_, String>("select csrf_token from sessions limit 1")
        .fetch_one(pool)
        .await
        .expect("a session should exist")
}

/// Creates the user and returns a usable session cookie.
pub async fn sign_up(router: &Router, setup_token: &str) -> String {
    let response = post_form(
        router.clone(),
        "/setup",
        &format!("setup_token={setup_token}&username=reader&password=correct+horse+battery"),
    )
    .await;
    session_cookie(&response).expect("setup should sign the new user in")
}

/// A POST that a browser on another site would send: fetch metadata says cross-site.
pub async fn post_cross_site(router: Router, uri: &str, body: &str) -> Response<Body> {
    send(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("sec-fetch-site", "cross-site"),
        body,
    )
    .await
}

pub async fn post_cross_site_as(
    router: Router,
    uri: &str,
    cookie: &str,
    body: &str,
) -> Response<Body> {
    send(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("sec-fetch-site", "cross-site")
            .header(header::COOKIE, cookie),
        body,
    )
    .await
}

/// A `multipart/form-data` POST, for the OPML import route.
///
/// Fields are written in the order given, which matters: the import handler verifies the
/// CSRF token from the first field before it reads the upload, so a test can prove that
/// ordering by leaving the token out or putting it second.
pub async fn post_multipart_as(
    router: Router,
    uri: &str,
    cookie: &str,
    fields: &[(&str, &str, Option<&str>)],
    file: Option<(&str, &str, &[u8])>,
) -> Response<Body> {
    const BOUNDARY: &str = "rustle-test-boundary";
    let mut body: Vec<u8> = Vec::new();

    for (name, value, _) in fields {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    if let Some((name, filename, contents)) = file {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n\
                 Content-Type: text/xml\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(contents);
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("sec-fetch-site", "same-origin")
        .header(header::COOKIE, cookie)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .expect("the test request should be well formed");

    router
        .oneshot(request)
        .await
        .expect("router should not fail")
}

/// A router whose fetcher will talk to a local `wiremock` server.
///
/// `Fetcher::allowing_loopback` is the test-only half of `feed::fetch::AddressPolicy`:
/// the production guard refuses loopback, which is exactly where a mock server lives.
pub fn app_with_mock(pool: PgPool) -> (Router, AppState) {
    let config = test_config();
    let fetcher = rustle::feed::fetch::Fetcher::allowing_loopback("Rustle/test")
        .expect("the test HTTP client configuration is valid");
    let state = AppState::with_fetcher(config, pool, fetcher);
    (build_router(state.clone()), state)
}

/// A minimal valid RSS feed with the given items, for mounting on a mock server.
pub fn rss_feed(site: &str, items: &[(&str, &str, &str)]) -> String {
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <rss version=\"2.0\"><channel>\n\
           <title>Mock Feed</title><link>{site}</link>\
           <description>A feed served by the test suite.</description>\n"
    );
    for (guid, title, body) in items {
        out.push_str(&format!(
            "<item><title>{title}</title><link>{site}/{guid}</link>\
             <guid isPermaLink=\"false\">{guid}</guid>\
             <pubDate>Wed, 30 Sep 2026 08:00:00 GMT</pubDate>\
             <description>{body}</description></item>\n"
        ));
    }
    out.push_str("</channel></rss>\n");
    out
}
