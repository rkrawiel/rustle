//! The settings page: theme, reading preferences, and (indirectly) change password.

mod support;

use axum::http::StatusCode;
use sqlx::PgPool;
use support::{
    app_with_state, assert_redirects_to, body_string, csrf_token, get_as, post_form_as, sign_up,
};

#[sqlx::test]
async fn the_settings_page_shows_the_current_preferences(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;

    let body = body_string(get_as(router, "/settings", &cookie).await).await;

    assert!(body.contains("reader"));
    assert!(body.contains(r#"value="50""#)); // the default entries_per_page
    assert!(body.contains(r#"value="0""#)); // the default retention_days
    assert!(body.contains(r#"value="system" checked"#) || body.contains(r#"checked"#));
}

#[sqlx::test]
async fn changing_the_theme_persists_it(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;

    let response = post_form_as(
        router.clone(),
        "/settings/theme",
        &cookie,
        &format!("csrf_token={token}&theme=dark"),
    )
    .await;
    assert_redirects_to(&response, "/settings");

    let theme: String = sqlx::query_scalar("select theme from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(theme, "dark");

    let body = body_string(get_as(router, "/unread", &cookie).await).await;
    assert!(body.contains(r#"data-theme="dark""#));
}

#[sqlx::test]
async fn changing_reading_preferences_persists_them(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;

    let response = post_form_as(
        router,
        "/settings",
        &cookie,
        &format!("csrf_token={token}&entries_per_page=25&retention_days=14"),
    )
    .await;
    assert_redirects_to(&response, "/settings");

    let (entries_per_page, retention_days): (i32, i32) =
        sqlx::query_as("select entries_per_page, retention_days from users")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(entries_per_page, 25);
    assert_eq!(retention_days, 14);
}

#[sqlx::test]
async fn entries_per_page_outside_the_allowed_range_is_refused(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;

    let too_low = post_form_as(
        router.clone(),
        "/settings",
        &cookie,
        &format!("csrf_token={token}&entries_per_page=1&retention_days=0"),
    )
    .await;
    assert_eq!(too_low.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let too_high = post_form_as(
        router,
        "/settings",
        &cookie,
        &format!("csrf_token={token}&entries_per_page=9000&retention_days=0"),
    )
    .await;
    assert_eq!(too_high.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let entries_per_page: i32 = sqlx::query_scalar("select entries_per_page from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        entries_per_page, 50,
        "the invalid values must not have been saved"
    );
}

#[sqlx::test]
async fn negative_retention_is_refused(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;

    let response = post_form_as(
        router,
        "/settings",
        &cookie,
        &format!("csrf_token={token}&entries_per_page=50&retention_days=-1"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn settings_changes_require_a_csrf_token(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;

    let theme = post_form_as(router.clone(), "/settings/theme", &cookie, "theme=dark").await;
    assert_eq!(theme.status(), StatusCode::FORBIDDEN);

    let preferences = post_form_as(
        router,
        "/settings",
        &cookie,
        "entries_per_page=50&retention_days=0",
    )
    .await;
    assert_eq!(preferences.status(), StatusCode::FORBIDDEN);
}
