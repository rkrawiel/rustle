//! Fetching and ingesting feeds, against a real HTTP server.
//!
//! Everything here runs against `wiremock` rather than a stubbed client, so the
//! conditional-GET headers, the status handling and the redirect policy are exercised as
//! the network actually delivers them.

mod support;

use rustle::db::feed::Feed;
use rustle::feed::fetch::{self, FetchError, Fetched, Fetcher};
use rustle::feed::ingest::{self, Ingested};
use sqlx::PgPool;
use support::rss_feed;
use url::Url;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fetcher() -> Fetcher {
    Fetcher::allowing_loopback("Rustle/test").expect("test client builds")
}

/// A user, a category and a feed pointing at `feed_url`, with the poll due now.
async fn subscribed(pool: &PgPool, feed_url: &str) -> Feed {
    sqlx::query("insert into users (id, username, password_hash) values (1, 'reader', 'x')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("insert into categories (id, user_id, title) values (1, 1, 'News')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into feeds (id, user_id, category_id, title, feed_url,
                            check_interval_seconds, next_check_at)
         values (1, 1, 1, 'Placeholder', $1, 3600, now())",
    )
    .bind(feed_url)
    .execute(pool)
    .await
    .unwrap();

    rustle::db::feed::find(pool, 1, 1)
        .await
        .unwrap()
        .expect("the feed was just inserted")
}

async fn reload(pool: &PgPool) -> Feed {
    rustle::db::feed::find(pool, 1, 1)
        .await
        .unwrap()
        .expect("the feed still exists")
}

// ---------------------------------------------------------------- the happy path

#[sqlx::test]
async fn a_successful_poll_stores_entries_and_the_feeds_own_title(pool: PgPool) {
    let server = MockServer::start().await;
    let body = rss_feed(
        &server.uri(),
        &[
            ("a", "First post", "Body of the first."),
            ("b", "Second post", "And the second."),
        ],
    );

    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/rss+xml")
                .insert_header("etag", "W/\"v1\"")
                .insert_header("last-modified", "Wed, 30 Sep 2026 08:00:00 GMT"),
        )
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    let counts = ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    assert_eq!(
        counts,
        Ingested {
            added: 2,
            updated: 0,
            unchanged: 0
        }
    );

    let after = reload(&pool).await;
    assert_eq!(
        after.title, "Mock Feed",
        "the feed's own title replaces the placeholder"
    );
    assert_eq!(after.etag.as_deref(), Some("W/\"v1\""));
    assert_eq!(
        after.last_modified.as_deref(),
        Some("Wed, 30 Sep 2026 08:00:00 GMT")
    );
    assert_eq!(after.failure_count, 0);
    assert!(after.last_error.is_none());
    assert!(after.last_success_at.is_some());
    assert!(after.next_check_at > chrono::Utc::now());

    let titles: Vec<String> = sqlx::query_scalar("select title from entries order by title")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(titles, ["First post", "Second post"]);

    // Content is sanitized and the plain text is populated for the search vector.
    let (content, text): (String, String) =
        sqlx::query_as("select content, content_text from entries where title = 'First post'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(text.contains("Body of the first"));
    assert!(!content.contains("<script"));
}

#[sqlx::test]
async fn the_second_poll_sends_the_stored_validators(pool: PgPool) {
    let server = MockServer::start().await;

    // The mock matches *only* when both validators are present, so a 304 is proof they
    // were sent; anything else would 404 and be recorded as a failure instead.
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .and(header_exists("if-none-match"))
        .and(header_exists("if-modified-since"))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;

    subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    sqlx::query(
        "update feeds set etag = 'W/\"v1\"',
                          last_modified = 'Wed, 30 Sep 2026 08:00:00 GMT' where id = 1",
    )
    .execute(&pool)
    .await
    .unwrap();

    let feed = reload(&pool).await;
    let counts = ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    assert_eq!(counts, Ingested::default());
    assert_eq!(
        reload(&pool).await.failure_count,
        0,
        "a 304 was reached, so the validators went out"
    );
}

#[sqlx::test]
async fn a_304_is_a_success_that_transfers_nothing(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    sqlx::query("update feeds set failure_count = 3, last_error = 'old' where id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let feed = Feed {
        failure_count: 3,
        ..feed
    };

    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    let after = reload(&pool).await;
    assert_eq!(after.failure_count, 0, "304 means the feed is healthy");
    assert!(after.last_error.is_none());
    assert!(after.last_success_at.is_some());
    assert!(after.next_check_at > chrono::Utc::now());
}

#[sqlx::test]
async fn re_polling_unchanged_content_writes_nothing(pool: PgPool) {
    let server = MockServer::start().await;
    let body = rss_feed(&server.uri(), &[("a", "First post", "Body.")]);

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;

    let first = ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();
    assert_eq!(first.added, 1);

    // Mark it read, then poll again. An unchanged entry must not come back unread.
    sqlx::query("update entries set read_at = now()")
        .execute(&pool)
        .await
        .unwrap();

    let second = ingest::refresh_feed(&pool, &fetcher(), &reload(&pool).await)
        .await
        .unwrap();
    assert_eq!(
        second,
        Ingested {
            added: 0,
            updated: 0,
            unchanged: 1
        }
    );

    let still_read: bool = sqlx::query_scalar("select read_at is not null from entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(still_read, "an unchanged entry must stay read");

    let count: i64 = sqlx::query_scalar("select count(*) from entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "a re-poll must not duplicate the entry");
}

#[sqlx::test]
async fn an_edited_entry_is_updated_in_place(pool: PgPool) {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(rss_feed(&server.uri(), &[("a", "First post", "Original.")])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(rss_feed(
            &server.uri(),
            &[("a", "First post", "Corrected.")],
        )))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/v1", server.uri())).await;
    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    // Same guid, different content.
    sqlx::query("update feeds set feed_url = $1 where id = 1")
        .bind(format!("{}/v2", server.uri()))
        .execute(&pool)
        .await
        .unwrap();

    let counts = ingest::refresh_feed(&pool, &fetcher(), &reload(&pool).await)
        .await
        .unwrap();
    assert_eq!(counts.updated, 1);
    assert_eq!(counts.added, 0);

    let text: String = sqlx::query_scalar("select content_text from entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(text.contains("Corrected"), "{text}");
}

// ---------------------------------------------------------------- rate limiting

#[sqlx::test]
async fn a_429_with_retry_after_seconds_is_not_counted_as_a_failure(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    sqlx::query("update feeds set failure_count = 2 where id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let feed = Feed {
        failure_count: 2,
        ..feed
    };

    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    let after = reload(&pool).await;
    // The whole point: a throttled feed must not accumulate failures and get disabled.
    assert_eq!(after.failure_count, 2);
    assert!(after.rate_limited_until.is_some());
    assert!(after.disabled_at.is_none());

    // Roughly an hour out, allowing for the jitter.
    let delay = (after.next_check_at - chrono::Utc::now()).num_seconds();
    assert!((3100..=4000).contains(&delay), "{delay}s");
}

#[sqlx::test]
async fn a_429_with_an_http_date_retry_after_is_honoured(pool: PgPool) {
    let server = MockServer::start().await;
    let when = chrono::Utc::now() + chrono::Duration::hours(2);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header(
            "retry-after",
            when.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
        ))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    let after = reload(&pool).await;
    let delay = (after.next_check_at - chrono::Utc::now()).num_seconds();
    assert!(
        (6400..=7900).contains(&delay),
        "{delay}s should be about two hours"
    );
}

#[sqlx::test]
async fn a_503_with_retry_after_is_treated_as_throttling_not_failure(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "1800"))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    let after = reload(&pool).await;
    assert_eq!(after.failure_count, 0);
    assert!(after.rate_limited_until.is_some());
}

#[sqlx::test]
async fn a_503_without_retry_after_is_an_ordinary_failure(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    let after = reload(&pool).await;
    assert_eq!(after.failure_count, 1);
    assert!(after.rate_limited_until.is_none());
    assert!(after.last_error.as_deref().unwrap().contains("503"));
}

// ---------------------------------------------------------------- failures

#[sqlx::test]
async fn a_404_is_recorded_on_the_feed_rather_than_returned_as_an_error(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;

    // Not an Err: a publisher's broken feed is not our failure, and the panel is where
    // the reader learns about it.
    let counts = ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();
    assert_eq!(counts, Ingested::default());

    let after = reload(&pool).await;
    assert_eq!(after.failure_count, 1);
    assert!(
        after.last_error.as_deref().unwrap().contains("404"),
        "{after:?}"
    );
    assert!(after.last_success_at.is_none());
}

#[sqlx::test]
async fn repeated_failures_eventually_disable_the_feed(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    sqlx::query("update feeds set failure_count = 11 where id = 1")
        .execute(&pool)
        .await
        .unwrap();

    ingest::refresh_feed(
        &pool,
        &fetcher(),
        &Feed {
            failure_count: 11,
            ..feed
        },
    )
    .await
    .unwrap();

    let after = reload(&pool).await;
    assert_eq!(after.failure_count, 12);
    assert!(
        after.disabled_at.is_some(),
        "a feed that has failed twelve times running is gone, not slow"
    );
}

#[sqlx::test]
async fn a_page_that_is_not_a_feed_is_a_parse_failure(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><body>Not a feed</body></html>"),
        )
        .mount(&server)
        .await;

    let feed = subscribed(&pool, &format!("{}/feed.xml", server.uri())).await;
    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    let after = reload(&pool).await;
    assert_eq!(after.failure_count, 1);
    assert!(after.last_error.is_some());
}

#[sqlx::test]
async fn a_feed_at_an_unparseable_address_fails_without_a_request(pool: PgPool) {
    let feed = subscribed(&pool, "not a url at all").await;

    ingest::refresh_feed(&pool, &fetcher(), &feed)
        .await
        .unwrap();

    let after = reload(&pool).await;
    assert_eq!(after.failure_count, 1);
    assert!(after.last_error.as_deref().unwrap().contains("not valid"));
}

// ---------------------------------------------------------------- redirects and limits

#[tokio::test]
async fn redirects_are_followed_and_the_final_url_is_reported() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/new", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/new"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
        .mount(&server)
        .await;

    let url = Url::parse(&format!("{}/old", server.uri())).unwrap();
    let fetched = fetch::fetch(&fetcher(), &url, None, None).await.unwrap();

    match fetched {
        Fetched::Body { final_url, .. } => {
            assert!(final_url.path().ends_with("/new"), "{final_url}");
        }
        other => panic!("expected a body, got {other:?}"),
    }
}

#[tokio::test]
async fn a_redirect_loop_is_abandoned_rather_than_followed_forever() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/loop", server.uri())),
        )
        .mount(&server)
        .await;

    let url = Url::parse(&format!("{}/loop", server.uri())).unwrap();
    let err = fetch::fetch(&fetcher(), &url, None, None)
        .await
        .expect_err("a loop should not be followed");
    assert!(matches!(err, FetchError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn a_body_past_the_cap_is_abandoned() {
    let server = MockServer::start().await;
    // Larger than the cap, and served without a Content-Length that would let us bail
    // early, so the streaming check is what stops it.
    let huge = "x".repeat(fetch::MAX_BODY_BYTES + 1024);

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(huge))
        .mount(&server)
        .await;

    let url = Url::parse(&format!("{}/huge", server.uri())).unwrap();
    let err = fetch::fetch(&fetcher(), &url, None, None)
        .await
        .expect_err("an oversized body should be refused");
    assert!(matches!(err, FetchError::TooLarge), "{err:?}");
}

#[tokio::test]
async fn the_user_agent_identifies_rustle_to_the_publisher() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("user-agent", "Rustle/test"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let url = Url::parse(&format!("{}/feed", server.uri())).unwrap();
    // The mock only matches on our User-Agent, so a 200 proves it was sent.
    assert!(matches!(
        fetch::fetch(&fetcher(), &url, None, None).await,
        Ok(Fetched::Body { .. })
    ));
}

#[tokio::test]
async fn a_private_address_is_refused_before_any_request() {
    let production = Fetcher::new(&support::test_config()).unwrap();

    for address in [
        "http://127.0.0.1/feed",
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.1/feed",
    ] {
        let url = Url::parse(address).unwrap();
        let err = fetch::fetch(&production, &url, None, None)
            .await
            .expect_err("{address} should be blocked");
        assert!(matches!(err, FetchError::Blocked(_)), "{address}: {err:?}");
    }
}
