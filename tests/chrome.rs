//! The page shell: assets, caching, security headers and the error page.

mod support;

use axum::http::{StatusCode, header};
use http_body_util::BodyExt;
use sqlx::PgPool;
use support::{app, get};

#[sqlx::test]
async fn assets_are_served_immutable_under_a_hashed_path(pool: PgPool) {
    let path = rustle::assets::get("app.css")
        .expect("app.css is embedded")
        .path();

    let response = get(app(pool), &path).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/css; charset=utf-8"
    );
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable",
        "the hash in the path is what makes a year-long immutable cache safe"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!body.is_empty());
}

#[sqlx::test]
async fn unknown_assets_are_not_found(pool: PgPool) {
    let response = get(app(pool), "/assets/deadbeefdeadbeef/nope.css").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn html_is_never_cached_because_it_carries_theme_and_read_state(pool: PgPool) {
    let response = get(app(pool), "/does-not-exist").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "private, no-store"
    );
}

#[sqlx::test]
async fn every_response_carries_the_security_headers(pool: PgPool) {
    let response = get(app(pool), "/health").await;

    let headers = response.headers();
    assert_eq!(
        headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
        "nosniff"
    );
    assert_eq!(headers.get(header::REFERRER_POLICY).unwrap(), "no-referrer");

    let csp = headers
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("frame-ancestors 'none'"));
    // Entry content embeds publisher-hosted images, and a media proxy is out of scope.
    assert!(csp.contains("img-src * data:"));
}

#[sqlx::test]
async fn the_error_page_renders_the_shell_with_landmarks_and_a_skip_link(pool: PgPool) {
    let response = get(app(pool), "/does-not-exist").await;
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();

    assert!(html.contains("<title>Not found · Rustle</title>"));
    // Pre-authentication pages follow the OS, since there is no user row to read.
    assert!(html.contains(r#"data-theme="system""#));
    assert!(html.contains(r##"<a class="skip" href="#main">"##));
    assert!(html.contains(r#"<main id="main""#));
    assert!(html.contains("<header"));
    assert!(html.contains("<footer"));
    assert!(html.contains("That page does not exist."));
    // Tokens first, so components can override them.
    assert!(html.contains("/tokens.css"));
    assert!(html.contains("/app.css"));
}
