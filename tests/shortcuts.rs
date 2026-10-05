//! The markup the keyboard shortcuts in `assets/app.js` act on. The JS itself has no
//! runtime here to execute it against, so these pin the ids, attributes and conditional
//! rendering it depends on, rather than the script.

mod support;

use sqlx::PgPool;
use support::{
    app_with_mock, assert_redirects_to, body_string, csrf_token, get, get_as, post_form_as,
    rss_feed, sign_up,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn encode(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            ':' => "%3A".to_owned(),
            '/' => "%2F".to_owned(),
            '?' => "%3F".to_owned(),
            '=' => "%3D".to_owned(),
            '&' => "%26".to_owned(),
            ' ' => "+".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

#[sqlx::test]
async fn the_script_is_linked_only_once_signed_in(pool: PgPool) {
    let (router, state) = app_with_mock(pool.clone());

    let anonymous = body_string(get(router.clone(), "/login").await).await;
    assert!(
        !anonymous.contains("app.js"),
        "the shortcuts script has nothing to act on before sign-in"
    );

    let cookie = sign_up(&router, state.setup_token()).await;
    let signed_in = body_string(get_as(router, "/unread", &cookie).await).await;
    assert!(signed_in.contains("app.js"));
    assert!(signed_in.contains("defer"));
}

#[sqlx::test]
async fn the_help_overlay_is_present_but_hidden(pool: PgPool) {
    let (router, state) = app_with_mock(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;

    let body = body_string(get_as(router, "/unread", &cookie).await).await;

    assert!(body.contains(r#"id="shortcut-help""#));
    assert!(body.contains("hidden"));
    assert!(body.contains(r#"id="shortcut-help-open""#));
    assert!(body.contains(r#"id="shortcut-help-close""#));
}

#[sqlx::test]
async fn mark_all_read_form_has_a_stable_id_for_the_a_shortcut(pool: PgPool) {
    let (router, state) = app_with_mock(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(rss_feed(&server.uri(), &[("a", "First post", "Body.")])),
        )
        .mount(&server)
        .await;
    let url = format!("{}/feed.xml", server.uri());
    post_form_as(
        router.clone(),
        "/feeds",
        &cookie,
        &format!("csrf_token={token}&url={}", encode(&url)),
    )
    .await;

    let feed_id: i64 = sqlx::query_scalar("select id from feeds where feed_url = $1")
        .bind(&url)
        .fetch_one(&pool)
        .await
        .unwrap();
    let row = rustle::db::feed::find(&pool, 1, feed_id)
        .await
        .unwrap()
        .unwrap();
    rustle::feed::ingest::refresh_feed(&pool, state.fetcher(), &row)
        .await
        .unwrap();

    let body = body_string(get_as(router, "/unread", &cookie).await).await;
    assert!(body.contains(r#"id="mark-all-read-form""#));
}

#[sqlx::test]
async fn refresh_feed_form_appears_only_when_viewing_a_single_feed(pool: PgPool) {
    let (router, state) = app_with_mock(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(rss_feed(&server.uri(), &[])))
        .mount(&server)
        .await;
    let url = format!("{}/feed.xml", server.uri());
    let response = post_form_as(
        router.clone(),
        "/feeds",
        &cookie,
        &format!("csrf_token={token}&url={}", encode(&url)),
    )
    .await;
    assert_redirects_to(&response, "/feeds");

    let feed_id: i64 = sqlx::query_scalar("select id from feeds where feed_url = $1")
        .bind(&url)
        .fetch_one(&pool)
        .await
        .unwrap();

    let feed_page =
        body_string(get_as(router.clone(), &format!("/feeds/{feed_id}"), &cookie).await).await;
    assert!(feed_page.contains(r#"id="refresh-feed-form""#));
    assert!(feed_page.contains(&format!("/feeds/{feed_id}/refresh")));

    let unread_page = body_string(get_as(router, "/unread", &cookie).await).await;
    assert!(!unread_page.contains(r#"id="refresh-feed-form""#));
}

#[sqlx::test]
async fn the_entry_page_has_stable_ids_for_its_read_and_star_forms(pool: PgPool) {
    let (router, state) = app_with_mock(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(rss_feed(&server.uri(), &[("a", "First post", "Body.")])),
        )
        .mount(&server)
        .await;
    let url = format!("{}/feed.xml", server.uri());
    post_form_as(
        router.clone(),
        "/feeds",
        &cookie,
        &format!("csrf_token={token}&url={}", encode(&url)),
    )
    .await;

    let feed_id: i64 = sqlx::query_scalar("select id from feeds where feed_url = $1")
        .bind(&url)
        .fetch_one(&pool)
        .await
        .unwrap();
    let row = rustle::db::feed::find(&pool, 1, feed_id)
        .await
        .unwrap()
        .unwrap();
    rustle::feed::ingest::refresh_feed(&pool, state.fetcher(), &row)
        .await
        .unwrap();

    let entry_id: i64 = sqlx::query_scalar("select id from entries limit 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    let body = body_string(get_as(router, &format!("/entries/{entry_id}"), &cookie).await).await;

    assert!(body.contains(r#"id="entry-read-form""#));
    assert!(body.contains(r#"id="entry-star-form""#));
}
