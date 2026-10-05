//! The background poller's due-feed selection, retention cleanup, and the end-to-end
//! wiring that makes a woken poller actually fetch a newly subscribed feed.

mod support;

use std::time::Duration;

use rustle::db::{category, entry, feed};
use rustle::feed::scheduler;
use sqlx::PgPool;
use support::{app_with_mock, assert_redirects_to, csrf_token, post_form_as, rss_feed, sign_up};
use tokio_util::sync::CancellationToken;
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

/// Inserts a feed directly, bypassing subscribe, so its scheduling state is exact rather
/// than whatever `feed::create`'s jitter happens to land on.
async fn insert_feed(
    pool: &PgPool,
    user_id: i64,
    category_id: i64,
    slug: &str,
    next_check_in: &str,
    disabled: bool,
) -> i64 {
    let sql = format!(
        "insert into feeds (user_id, category_id, title, feed_url, check_interval_seconds,
                             next_check_at, disabled_at)
         values ($1, $2, $3, $4, 3600, now() + {next_check_in}::interval,
                 case when $5 then now() else null end)
         returning id"
    );
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .bind(category_id)
        .bind(format!("Feed {slug}"))
        .bind(format!("https://example.invalid/{slug}"))
        .bind(disabled)
        .fetch_one(pool)
        .await
        .expect("the feed should insert")
}

async fn insert_entry(
    pool: &PgPool,
    user_id: i64,
    feed_id: i64,
    guid: &str,
    published_days_ago: i64,
    read: bool,
    starred: bool,
) -> i64 {
    sqlx::query_scalar(
        "insert into entries (user_id, feed_id, guid_hash, title, content, content_text,
                               content_hash, published_at, read_at, starred_at)
         values ($1, $2, $3, $4, '<p>x</p>', 'x', $3,
                 now() - make_interval(days => $5::int),
                 case when $6 then now() else null end,
                 case when $7 then now() else null end)
         returning id",
    )
    .bind(user_id)
    .bind(feed_id)
    .bind(guid.as_bytes())
    .bind(format!("Entry {guid}"))
    .bind(published_days_ago)
    .bind(read)
    .bind(starred)
    .fetch_one(pool)
    .await
    .expect("the entry should insert")
}

/// A user and their default category, with no HTTP involved.
async fn user_and_category(pool: &PgPool) -> (i64, i64) {
    let (router, state) = app_with_mock(pool.clone());
    sign_up(&router, state.setup_token()).await;
    let user_id: i64 = sqlx::query_scalar("select id from users limit 1")
        .fetch_one(pool)
        .await
        .unwrap();
    let category_id = category::find_or_create(pool, user_id, "Uncategorized")
        .await
        .unwrap()
        .id;
    (user_id, category_id)
}

// ---------------------------------------------------------------- due feeds

#[sqlx::test]
async fn due_feeds_exclude_ones_not_due_yet_and_disabled_ones(pool: PgPool) {
    let (user_id, category_id) = user_and_category(&pool).await;

    let due_id = insert_feed(&pool, user_id, category_id, "due", "-'1 hour'", false).await;
    let future_id = insert_feed(&pool, user_id, category_id, "future", "'1 hour'", false).await;
    let disabled_id = insert_feed(&pool, user_id, category_id, "disabled", "-'1 hour'", true).await;

    let due = feed::due(&pool, 500).await.unwrap();
    let ids: Vec<i64> = due.iter().map(|f| f.id).collect();

    assert!(ids.contains(&due_id));
    assert!(!ids.contains(&future_id));
    assert!(!ids.contains(&disabled_id));
}

#[sqlx::test]
async fn due_feeds_are_oldest_first_and_respect_the_limit(pool: PgPool) {
    let (user_id, category_id) = user_and_category(&pool).await;

    let oldest = insert_feed(&pool, user_id, category_id, "oldest", "-'3 hour'", false).await;
    let middle = insert_feed(&pool, user_id, category_id, "middle", "-'2 hour'", false).await;
    insert_feed(&pool, user_id, category_id, "newest", "-'1 hour'", false).await;

    let due = feed::due(&pool, 2).await.unwrap();
    let ids: Vec<i64> = due.iter().map(|f| f.id).collect();

    assert_eq!(ids, vec![oldest, middle]);
}

// ---------------------------------------------------------------- retention cleanup

#[sqlx::test]
async fn retention_cleanup_removes_old_read_unstarred_entries(pool: PgPool) {
    let (user_id, category_id) = user_and_category(&pool).await;
    sqlx::query("update users set retention_days = 30")
        .execute(&pool)
        .await
        .unwrap();
    let feed_id = insert_feed(&pool, user_id, category_id, "f", "-'1 hour'", false).await;
    let old_id = insert_entry(&pool, user_id, feed_id, "old", 60, true, false).await;

    let removed = entry::delete_expired(&pool).await.unwrap();

    assert_eq!(removed, 1);
    let remaining: i64 = sqlx::query_scalar("select count(*) from entries where id = $1")
        .bind(old_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[sqlx::test]
async fn retention_cleanup_never_removes_starred_entries(pool: PgPool) {
    let (user_id, category_id) = user_and_category(&pool).await;
    sqlx::query("update users set retention_days = 30")
        .execute(&pool)
        .await
        .unwrap();
    let feed_id = insert_feed(&pool, user_id, category_id, "f", "-'1 hour'", false).await;
    let starred_id = insert_entry(&pool, user_id, feed_id, "starred", 60, true, true).await;

    entry::delete_expired(&pool).await.unwrap();

    let remaining: i64 = sqlx::query_scalar("select count(*) from entries where id = $1")
        .bind(starred_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

#[sqlx::test]
async fn retention_cleanup_leaves_unread_entries_alone(pool: PgPool) {
    let (user_id, category_id) = user_and_category(&pool).await;
    sqlx::query("update users set retention_days = 30")
        .execute(&pool)
        .await
        .unwrap();
    let feed_id = insert_feed(&pool, user_id, category_id, "f", "-'1 hour'", false).await;
    let unread_id = insert_entry(&pool, user_id, feed_id, "unread", 60, false, false).await;

    entry::delete_expired(&pool).await.unwrap();

    let remaining: i64 = sqlx::query_scalar("select count(*) from entries where id = $1")
        .bind(unread_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

#[sqlx::test]
async fn retention_days_zero_means_keep_forever(pool: PgPool) {
    let (user_id, category_id) = user_and_category(&pool).await;
    // The default from setup, since RUSTLE_RETENTION_DAYS is unset in the test config.
    let feed_id = insert_feed(&pool, user_id, category_id, "f", "-'1 hour'", false).await;
    let old_id = insert_entry(&pool, user_id, feed_id, "ancient", 36500, true, false).await;

    entry::delete_expired(&pool).await.unwrap();

    let remaining: i64 = sqlx::query_scalar("select count(*) from entries where id = $1")
        .bind(old_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

// ---------------------------------------------------------------- end to end

#[sqlx::test]
async fn a_woken_poller_fetches_a_newly_subscribed_feed(pool: PgPool) {
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

    let shutdown = CancellationToken::new();
    let poller = tokio::spawn(scheduler::run(state.clone(), shutdown.clone()));

    let url = format!("{}/feed.xml", server.uri());
    let response = post_form_as(
        router,
        "/feeds",
        &cookie,
        &format!("csrf_token={token}&url={}", encode(&url)),
    )
    .await;
    assert_redirects_to(&response, "/feeds");

    // Subscribing already brings `next_check_at` forward and wakes the poller (Phase 4);
    // this just makes sure that wake-up is real, end to end, now that something is
    // actually listening on the other end of it.
    state.wake_poller();

    let mut entries: i64 = 0;
    for _ in 0..100 {
        entries = sqlx::query_scalar("select count(*) from entries")
            .fetch_one(&pool)
            .await
            .unwrap();
        if entries > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(entries, 1, "the poller should have fetched the new feed");

    shutdown.cancel();
    let _ = poller.await;
}
