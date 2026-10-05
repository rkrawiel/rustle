//! The liveness route.
//!
//! `#[sqlx::test]` creates a scratch database per test, runs `migrations/` against it, and
//! drops it afterwards, so these also prove the baseline migration applies cleanly.

mod support;

use axum::http::StatusCode;
use http_body_util::BodyExt;
use sqlx::PgPool;
use support::{app, get};

#[sqlx::test]
async fn health_reports_ok_when_the_database_answers(pool: PgPool) {
    let response = get(app(pool), "/health").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[sqlx::test]
async fn health_reports_unavailable_when_the_database_is_gone(pool: PgPool) {
    let router = app(pool.clone());
    pool.close().await;

    let response = get(router, "/health").await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
