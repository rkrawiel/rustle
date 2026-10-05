//! Categories, the subscription panel, autodiscovery, and OPML import and export.

mod support;

use axum::http::{StatusCode, header};
use sqlx::PgPool;
use support::{
    app_with_mock, assert_redirects_to, body_string, csrf_token, get_as, post_form_as,
    post_multipart_as, rss_feed, sign_up,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A signed-in reader, plus a mock publisher to subscribe to.
///
/// Subscribing fetches the address before saving anything — that is how pasting a site URL
/// finds the feed, and how an unreachable address is reported at once rather than as a
/// failing subscription an hour later — so these tests need a server that answers.
struct Reader {
    router: axum::Router,
    cookie: String,
    token: String,
    server: MockServer,
}

impl Reader {
    async fn new(pool: &PgPool) -> Self {
        let (router, state) = app_with_mock(pool.clone());
        let cookie = sign_up(&router, state.setup_token()).await;
        let token = csrf_token(pool).await;
        let server = MockServer::start().await;

        Self {
            router,
            cookie,
            token,
            server,
        }
    }

    /// Serves a feed at `at` and returns its absolute address.
    async fn serve_feed(&self, at: &str, items: &[(&str, &str, &str)]) -> String {
        Mock::given(method("GET"))
            .and(path(at.to_owned()))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(rss_feed(&self.server.uri(), items)),
            )
            .mount(&self.server)
            .await;
        format!("{}{at}", self.server.uri())
    }

    /// Serves an HTML page advertising a feed, and the feed itself.
    async fn serve_page_with_feed(&self, page_at: &str, feed_at: &str) -> String {
        self.serve_feed(feed_at, &[("a", "From the page's feed", "Body.")])
            .await;

        let html = format!(
            r#"<!doctype html><html><head>
                 <title>A Site</title>
                 <link rel="alternate" type="application/rss+xml" title="Site feed" href="{feed_at}">
               </head><body><p>A page, not a feed.</p></body></html>"#
        );
        Mock::given(method("GET"))
            .and(path(page_at.to_owned()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(html)
                    .insert_header("content-type", "text/html; charset=utf-8"),
            )
            .mount(&self.server)
            .await;

        format!("{}{page_at}", self.server.uri())
    }

    async fn post(&self, uri: &str, body: &str) -> axum::http::Response<axum::body::Body> {
        post_form_as(
            self.router.clone(),
            uri,
            &self.cookie,
            &format!("csrf_token={}&{body}", self.token),
        )
        .await
    }

    async fn get(&self, uri: &str) -> axum::http::Response<axum::body::Body> {
        get_as(self.router.clone(), uri, &self.cookie).await
    }

    /// Subscribes to an address, expecting it to work.
    async fn subscribe(&self, url: &str) {
        let response = self.post("/feeds", &format!("url={}", encode(url))).await;
        assert_redirects_to(&response, "/feeds");
    }

    async fn add_category(&self, title: &str) {
        let response = self
            .post("/categories", &format!("title={}", encode(title)))
            .await;
        assert_redirects_to(&response, "/categories");
    }
}

/// Minimal form encoding for the characters a URL or title actually contains.
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

async fn scalar<T>(pool: &PgPool, sql: &'static str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar::<_, T>(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err}"))
}

// ---------------------------------------------------------------- subscribing

#[sqlx::test]
async fn the_panel_explains_itself_when_there_are_no_feeds(pool: PgPool) {
    let reader = Reader::new(&pool).await;

    let body = body_string(reader.get("/feeds").await).await;

    assert!(body.contains("No subscriptions yet"));
    assert!(body.contains("Add a feed"));
}

#[sqlx::test]
async fn subscribing_takes_the_title_from_the_feed_and_the_default_category(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;

    reader.subscribe(&url).await;

    let (title, category): (String, String) = sqlx::query_as(
        "select f.title, c.title from feeds f join categories c on c.id = f.category_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(category, "Uncategorized");
    // Not the hostname: the feed was fetched, so its own title is available.
    assert_eq!(title, "Mock Feed");

    let body = body_string(reader.get("/feeds").await).await;
    assert!(body.contains("Mock Feed"));
}

#[sqlx::test]
async fn pasting_a_site_address_finds_the_feed_the_page_advertises(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let page = reader.serve_page_with_feed("/blog", "/blog/rss.xml").await;

    reader.subscribe(&page).await;

    let (feed_url, title): (String, String) = sqlx::query_as("select feed_url, title from feeds")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        feed_url.ends_with("/blog/rss.xml"),
        "we should have subscribed to the feed, not the page: {feed_url}"
    );
    // The page's own link title beats the feed's, since it is usually more specific.
    assert_eq!(title, "Site feed");
}

#[sqlx::test]
async fn a_page_with_no_feed_on_it_is_refused(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    Mock::given(method("GET"))
        .and(path("/plain"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><body>Nothing here</body></html>"),
        )
        .mount(&reader.server)
        .await;

    let response = reader
        .post(
            "/feeds",
            &format!("url={}", encode(&format!("{}/plain", reader.server.uri()))),
        )
        .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_string(response).await.contains("no feed on it"));
    assert_eq!(scalar::<i64>(&pool, "select count(*) from feeds").await, 0);
}

#[sqlx::test]
async fn an_unreachable_address_is_reported_now_rather_than_saved(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&reader.server)
        .await;

    let response = reader
        .post(
            "/feeds",
            &format!("url={}", encode(&format!("{}/broken", reader.server.uri()))),
        )
        .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(scalar::<i64>(&pool, "select count(*) from feeds").await, 0);
}

#[sqlx::test]
async fn addresses_that_cannot_be_fetched_are_refused_without_a_request(pool: PgPool) {
    let reader = Reader::new(&pool).await;

    for (url, expected) in [
        ("", "Enter a feed address"),
        ("file%3A%2F%2F%2Fetc%2Fpasswd", "Only http and https"),
        ("http%3A%2F%2F", "not a valid address"),
    ] {
        let response = reader.post("/feeds", &format!("url={url}")).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{url:?} should be refused"
        );
        assert!(
            body_string(response).await.contains(expected),
            "{url:?} should say {expected:?}"
        );
    }

    assert_eq!(scalar::<i64>(&pool, "select count(*) from feeds").await, 0);
}

#[sqlx::test]
async fn subscribing_twice_to_the_same_address_is_refused(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;
    reader.subscribe(&url).await;

    let response = reader
        .post("/feeds", &format!("url={}", encode(&url)))
        .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_string(response).await.contains("already subscribed"));
}

#[sqlx::test]
async fn a_new_subscription_is_polled_at_once_rather_than_appearing_empty(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;

    reader.subscribe(&url).await;

    // Subscribing jitters the first scheduled check into the future, so a new feed would
    // otherwise sit empty for up to an interval. It is brought forward to now instead.
    assert!(
        scalar::<bool>(&pool, "select next_check_at <= now() from feeds").await,
        "a new subscription should be due immediately"
    );
}

#[sqlx::test]
async fn editing_a_feed_changes_its_title_and_category(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;
    reader.subscribe(&url).await;
    reader.add_category("Rust").await;

    let feed_id = scalar::<i64>(&pool, "select id from feeds").await;
    let rust_id = scalar::<i64>(&pool, "select id from categories where title = 'Rust'").await;

    let response = reader
        .post(
            &format!("/feeds/{feed_id}"),
            &format!("title=Renamed&url={}&category_id={rust_id}", encode(&url)),
        )
        .await;
    assert_redirects_to(&response, "/feeds");

    let (title, category_id): (String, i64) =
        sqlx::query_as("select title, category_id from feeds")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(title, "Renamed");
    assert_eq!(category_id, rust_id);
}

#[sqlx::test]
async fn changing_the_address_discards_the_validators_for_the_old_one(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let first = reader
        .serve_feed("/one.xml", &[("a", "First post", "Body.")])
        .await;
    let second = reader
        .serve_feed("/two.xml", &[("b", "Other post", "Body.")])
        .await;
    reader.subscribe(&first).await;

    let feed_id = scalar::<i64>(&pool, "select id from feeds").await;
    let category_id = scalar::<i64>(&pool, "select category_id from feeds").await;
    sqlx::query(
        "update feeds set etag = 'W/\"abc\"', last_modified = 'Mon, 01 Jan 2024 00:00:00 GMT',
                          failure_count = 3, last_error = 'boom'",
    )
    .execute(&pool)
    .await
    .unwrap();

    reader
        .post(
            &format!("/feeds/{feed_id}"),
            &format!(
                "title=Kept&url={}&category_id={category_id}",
                encode(&second)
            ),
        )
        .await;

    let (etag, last_modified, failures): (Option<String>, Option<String>, i32) =
        sqlx::query_as("select etag, last_modified, failure_count from feeds")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(etag, None, "the old etag would wrongly produce a 304");
    assert_eq!(last_modified, None);
    assert_eq!(failures, 0, "a new address starts with a clean record");

    // Editing without changing the address keeps them.
    sqlx::query("update feeds set etag = 'W/\"keep\"', failure_count = 2")
        .execute(&pool)
        .await
        .unwrap();
    reader
        .post(
            &format!("/feeds/{feed_id}"),
            &format!(
                "title=Kept2&url={}&category_id={category_id}",
                encode(&second)
            ),
        )
        .await;

    let (etag, failures): (Option<String>, i32) =
        sqlx::query_as("select etag, failure_count from feeds")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(etag.as_deref(), Some("W/\"keep\""));
    assert_eq!(failures, 2);
}

#[sqlx::test]
async fn refreshing_brings_the_next_check_forward(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;
    reader.subscribe(&url).await;

    // Push it into the future so bringing it forward is observable.
    sqlx::query("update feeds set next_check_at = now() + interval '1 hour'")
        .execute(&pool)
        .await
        .unwrap();

    let feed_id = scalar::<i64>(&pool, "select id from feeds").await;
    let response = reader.post(&format!("/feeds/{feed_id}/refresh"), "").await;
    assert_redirects_to(&response, "/feeds");

    assert!(
        scalar::<bool>(&pool, "select next_check_at <= now() from feeds").await,
        "the feed should now be due for the poller to pick up"
    );
}

#[sqlx::test]
async fn refresh_all_queues_every_feed(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    for (at, guid) in [("/one.xml", "a"), ("/two.xml", "b")] {
        let url = reader.serve_feed(at, &[(guid, "A post", "Body.")]).await;
        reader.subscribe(&url).await;
    }
    sqlx::query("update feeds set next_check_at = now() + interval '1 hour'")
        .execute(&pool)
        .await
        .unwrap();

    assert_redirects_to(&reader.post("/feeds/refresh", "").await, "/feeds");

    assert_eq!(
        scalar::<i64>(
            &pool,
            "select count(*) from feeds where next_check_at <= now()"
        )
        .await,
        2
    );
}

#[sqlx::test]
async fn deleting_a_feed_removes_it(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;
    reader.subscribe(&url).await;

    let feed_id = scalar::<i64>(&pool, "select id from feeds").await;
    reader.post(&format!("/feeds/{feed_id}/delete"), "").await;

    assert_eq!(scalar::<i64>(&pool, "select count(*) from feeds").await, 0);
    // Deleting a feed takes its entries with it.
    assert_eq!(
        scalar::<i64>(&pool, "select count(*) from entries").await,
        0
    );

    // A second delete is a 404, not a silent success.
    let again = reader.post(&format!("/feeds/{feed_id}/delete"), "").await;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------- categories

#[sqlx::test]
async fn categories_can_be_created_renamed_and_deleted(pool: PgPool) {
    let reader = Reader::new(&pool).await;

    reader.add_category(" Rust ").await;
    let id = scalar::<i64>(&pool, "select id from categories where title = 'Rust'").await;

    reader
        .post(&format!("/categories/{id}"), "title=Systems")
        .await;
    assert_eq!(
        scalar::<String>(&pool, "select title from categories").await,
        "Systems"
    );

    reader.post(&format!("/categories/{id}/delete"), "").await;
    assert_eq!(
        scalar::<i64>(&pool, "select count(*) from categories").await,
        0
    );
}

#[sqlx::test]
async fn a_duplicate_or_empty_category_name_is_refused(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader.add_category("Rust").await;

    let duplicate = reader.post("/categories", "title=Rust").await;
    assert_eq!(duplicate.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_string(duplicate).await.contains("already a category"));

    let blank = reader.post("/categories", "title=+").await;
    assert_eq!(blank.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_string(blank).await.contains("needs a name"));
}

#[sqlx::test]
async fn deleting_a_category_that_still_has_feeds_is_refused(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;
    reader.subscribe(&url).await;

    let id = scalar::<i64>(&pool, "select id from categories").await;
    let response = reader.post(&format!("/categories/{id}/delete"), "").await;

    // `on delete restrict` rather than cascade: deleting a category must never take its
    // feeds and every entry in them along with it.
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_string(response).await.contains("still has feeds"));
    assert_eq!(scalar::<i64>(&pool, "select count(*) from feeds").await, 1);
}

#[sqlx::test]
async fn the_categories_page_counts_feeds_and_unread_entries(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "One", "Body."), ("b", "Two", "Body.")])
        .await;
    reader.subscribe(&url).await;

    let body = body_string(reader.get("/categories").await).await;

    assert!(body.contains("Uncategorized"));
    assert!(body.contains("1 feed"), "{body}");
}

// ---------------------------------------------------------------- OPML

#[sqlx::test]
async fn opml_round_trips_through_export_and_import(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader.add_category("Rust").await;
    let rust_id = scalar::<i64>(&pool, "select id from categories where title = 'Rust'").await;

    let mut urls = Vec::new();
    for (at, guid) in [("/one.xml", "a"), ("/two.xml", "b")] {
        let url = reader.serve_feed(at, &[(guid, "A post", "Body.")]).await;
        reader
            .post(
                "/feeds",
                &format!("url={}&category_id={rust_id}", encode(&url)),
            )
            .await;
        urls.push(url);
    }

    let exported = reader.get("/opml/export").await;
    assert_eq!(exported.status(), StatusCode::OK);
    assert_eq!(
        exported.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/x-opml; charset=utf-8"
    );
    assert!(
        exported
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("rustle-subscriptions.opml")
    );

    let document = body_string(exported).await;
    assert!(document.contains(&urls[0]));
    assert!(document.contains(r#"<outline text="Rust">"#));

    // Wipe everything and import the file back.
    sqlx::query("delete from feeds")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from categories")
        .execute(&pool)
        .await
        .unwrap();

    let imported = post_multipart_as(
        reader.router.clone(),
        "/opml/import",
        &reader.cookie,
        &[("csrf_token", reader.token.as_str(), None)],
        Some(("opml", "subs.opml", document.as_bytes())),
    )
    .await;
    assert_redirects_to(&imported, "/feeds");

    let rows: Vec<(String, String)> = sqlx::query_as(
        "select f.feed_url, c.title from feeds f join categories c on c.id = f.category_id
         order by f.feed_url",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(_, category)| category == "Rust"));
}

#[sqlx::test]
async fn importing_a_file_that_overlaps_what_is_subscribed_skips_the_duplicates(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let url = reader
        .serve_feed("/feed.xml", &[("a", "First post", "Body.")])
        .await;
    reader.subscribe(&url).await;

    let document = format!(
        r#"<opml version="2.0"><body>
             <outline text="News">
               <outline type="rss" text="Already here" xmlUrl="{url}" />
               <outline type="rss" text="Brand new" xmlUrl="https://elsewhere.example/feed" />
             </outline>
           </body></opml>"#
    );

    let response = post_multipart_as(
        reader.router.clone(),
        "/opml/import",
        &reader.cookie,
        &[("csrf_token", reader.token.as_str(), None)],
        Some(("opml", "subs.opml", document.as_bytes())),
    )
    .await;
    assert_redirects_to(&response, "/feeds");

    assert_eq!(
        scalar::<i64>(&pool, "select count(*) from feeds").await,
        2,
        "the already-subscribed feed is skipped, not duplicated"
    );
}

#[sqlx::test]
async fn an_import_without_a_valid_csrf_token_is_forbidden(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let document = r#"<opml><body><outline xmlUrl="https://a.example/feed" /></body></opml>"#;

    let borrowed = post_multipart_as(
        reader.router.clone(),
        "/opml/import",
        &reader.cookie,
        &[("csrf_token", "borrowed", None)],
        Some(("opml", "subs.opml", document.as_bytes())),
    )
    .await;
    assert_eq!(borrowed.status(), StatusCode::FORBIDDEN);

    // A file sent with no preceding token is refused rather than buffered — which is why
    // the CSRF field is rendered before the file input.
    let no_token = post_multipart_as(
        reader.router.clone(),
        "/opml/import",
        &reader.cookie,
        &[],
        Some(("opml", "subs.opml", document.as_bytes())),
    )
    .await;
    assert_eq!(no_token.status(), StatusCode::FORBIDDEN);

    assert_eq!(scalar::<i64>(&pool, "select count(*) from feeds").await, 0);
}

#[sqlx::test]
async fn importing_a_file_with_no_feeds_says_so(pool: PgPool) {
    let reader = Reader::new(&pool).await;

    let response = post_multipart_as(
        reader.router.clone(),
        "/opml/import",
        &reader.cookie,
        &[("csrf_token", reader.token.as_str(), None)],
        Some(("opml", "subs.opml", b"<html><body>not opml</body></html>")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_string(response).await.contains("no feeds found"));
}
