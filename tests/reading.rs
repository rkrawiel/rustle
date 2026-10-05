//! The list views, the entry page, and the read/star toggles.

mod support;

use axum::http::StatusCode;
use rustle::AppState;
use rustle::db::feed;
use rustle::feed::ingest;
use sqlx::PgPool;
use support::{
    app_with_mock, assert_redirects_to, body_string, csrf_token, get_as, post_form_as, rss_feed,
    sign_up,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A signed-in reader, plus a mock publisher to subscribe to.
struct Reader {
    router: axum::Router,
    state: AppState,
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
            state,
            cookie,
            token,
            server,
        }
    }

    /// Serves a feed at `at`, subscribes to it, and runs the fetch the background poller
    /// would otherwise perform (which is not running in this test harness), so the
    /// entries exist before the view under test is requested.
    async fn subscribe(&self, pool: &PgPool, at: &str, items: &[(&str, &str, &str)]) -> i64 {
        Mock::given(method("GET"))
            .and(path(at.to_owned()))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(rss_feed(&self.server.uri(), items)),
            )
            .mount(&self.server)
            .await;
        let url = format!("{}{at}", self.server.uri());

        let response = self.post("/feeds", &format!("url={}", encode(&url))).await;
        assert_redirects_to(&response, "/feeds");

        let id: i64 = sqlx::query_scalar("select id from feeds where feed_url = $1")
            .bind(&url)
            .fetch_one(pool)
            .await
            .expect("the feed we just subscribed to should exist");

        let row = feed::find(pool, 1, id)
            .await
            .unwrap()
            .expect("the feed we just looked up should exist");
        ingest::refresh_feed(pool, self.state.fetcher(), &row)
            .await
            .expect("the mock feed should ingest cleanly");

        id
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

    /// A POST with no CSRF field at all, as a forged cross-site form would send.
    async fn post_without_csrf(
        &self,
        uri: &str,
        body: &str,
    ) -> axum::http::Response<axum::body::Body> {
        post_form_as(self.router.clone(), uri, &self.cookie, body).await
    }

    async fn get(&self, uri: &str) -> axum::http::Response<axum::body::Body> {
        get_as(self.router.clone(), uri, &self.cookie).await
    }
}

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

async fn entry_ids_newest_first(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("select id from entries order by published_at desc, id desc")
        .fetch_all(pool)
        .await
        .unwrap()
}

fn count_entries(body: &str) -> usize {
    body.matches("item-title").count()
}

// ---------------------------------------------------------------- list views

#[sqlx::test]
async fn the_unread_view_explains_itself_when_empty(pool: PgPool) {
    let reader = Reader::new(&pool).await;

    let body = body_string(reader.get("/unread").await).await;

    assert!(body.contains("Nothing unread"));
}

#[sqlx::test]
async fn unread_view_lists_only_unread_entries(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(
            &pool,
            "/feed.xml",
            &[("a", "First", "Body."), ("b", "Second", "Body.")],
        )
        .await;
    let ids = entry_ids_newest_first(&pool).await;

    reader
        .post(&format!("/entries/{}/read", ids[0]), "view=unread")
        .await;

    let unread_body = body_string(reader.get("/unread").await).await;
    assert_eq!(count_entries(&unread_body), 1);

    let all_body = body_string(reader.get("/all").await).await;
    assert_eq!(count_entries(&all_body), 2);
}

#[sqlx::test]
async fn by_feed_view_shows_only_that_feeds_entries(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let feed_a = reader
        .subscribe(&pool, "/a.xml", &[("1", "From A", "Body.")])
        .await;
    reader
        .subscribe(&pool, "/b.xml", &[("2", "From B", "Body.")])
        .await;

    let body = body_string(reader.get(&format!("/feeds/{feed_a}")).await).await;

    assert!(body.contains("From A"));
    assert!(!body.contains("From B"));
}

#[sqlx::test]
async fn by_category_view_shows_entries_from_every_feed_in_it(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .post("/categories", &format!("title={}", encode("Blogs")))
        .await;
    let category_id: i64 = sqlx::query_scalar("select id from categories where title = 'Blogs'")
        .fetch_one(&pool)
        .await
        .unwrap();

    let feed_a = reader
        .subscribe(&pool, "/a.xml", &[("1", "From A", "Body.")])
        .await;
    reader
        .subscribe(&pool, "/b.xml", &[("2", "From B", "Body.")])
        .await;

    // Move only feed A into the new category; B stays in the default one.
    let (feed_url, title): (String, String) =
        sqlx::query_as("select feed_url, title from feeds where id = $1")
            .bind(feed_a)
            .fetch_one(&pool)
            .await
            .unwrap();
    reader
        .post(
            &format!("/feeds/{feed_a}"),
            &format!(
                "url={}&title={}&category_id={category_id}",
                encode(&feed_url),
                encode(&title)
            ),
        )
        .await;

    let body = body_string(reader.get(&format!("/categories/{category_id}")).await).await;

    assert!(body.contains("From A"));
    assert!(!body.contains("From B"));
}

// ---------------------------------------------------------------- the entry page

#[sqlx::test]
async fn opening_an_entry_marks_it_read(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(&pool, "/feed.xml", &[("a", "First", "Body.")])
        .await;
    let id = entry_ids_newest_first(&pool).await[0];

    let nav_before = body_string(reader.get("/unread").await).await;
    assert!(nav_before.contains("Unread") && nav_before.contains("(1)"));

    reader.get(&format!("/entries/{id}?view=unread")).await;

    let read_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select read_at from entries where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(read_at.is_some());

    let nav_after = body_string(reader.get("/unread").await).await;
    assert!(!nav_after.contains("(1)"));
}

#[sqlx::test]
async fn entry_navigation_moves_to_the_neighbouring_entry_within_the_view(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(
            &pool,
            "/feed.xml",
            &[
                ("a", "Oldest", "Body."),
                ("b", "Middle", "Body."),
                ("c", "Newest", "Body."),
            ],
        )
        .await;
    let ids = entry_ids_newest_first(&pool).await;
    assert_eq!(ids.len(), 3);

    let body = body_string(reader.get(&format!("/entries/{}?view=all", ids[1])).await).await;

    assert!(body.contains(&format!("/entries/{}?view=all", ids[0])));
    assert!(body.contains(&format!("/entries/{}?view=all", ids[2])));
}

// ---------------------------------------------------------------- toggles

#[sqlx::test]
async fn toggling_read_flips_state_without_opening(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(&pool, "/feed.xml", &[("a", "First", "Body.")])
        .await;
    let id = entry_ids_newest_first(&pool).await[0];

    let response = reader
        .post(&format!("/entries/{id}/read"), "view=unread")
        .await;
    assert_redirects_to(&response, "/unread");

    let read_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select read_at from entries where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(read_at.is_some());

    reader
        .post(&format!("/entries/{id}/read"), "view=unread")
        .await;
    let read_at_again: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select read_at from entries where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(read_at_again.is_none());
}

#[sqlx::test]
async fn toggling_star_adds_and_removes_from_the_starred_view(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(&pool, "/feed.xml", &[("a", "First", "Body.")])
        .await;
    let id = entry_ids_newest_first(&pool).await[0];

    reader
        .post(&format!("/entries/{id}/star"), "view=unread")
        .await;
    let starred = body_string(reader.get("/starred").await).await;
    assert_eq!(count_entries(&starred), 1);

    reader
        .post(&format!("/entries/{id}/star"), "view=unread")
        .await;
    let unstarred = body_string(reader.get("/starred").await).await;
    assert!(unstarred.contains("Nothing starred"));
}

#[sqlx::test]
async fn mark_all_read_is_scoped_to_the_view_it_was_invoked_from(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    let feed_a = reader
        .subscribe(&pool, "/a.xml", &[("1", "From A", "Body.")])
        .await;
    reader
        .subscribe(&pool, "/b.xml", &[("2", "From B", "Body.")])
        .await;

    reader
        .post("/entries/read-all", &format!("view=feed:{feed_a}"))
        .await;

    let unread_titles: Vec<String> =
        sqlx::query_scalar("select title from entries where read_at is null order by id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(unread_titles, vec!["From B".to_owned()]);
}

// ---------------------------------------------------------------- pagination

#[sqlx::test]
async fn pagination_offers_an_older_page_once_the_limit_is_exceeded(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    sqlx::query("update users set entries_per_page = 10")
        .execute(&pool)
        .await
        .unwrap();
    let items: Vec<(String, String, String)> = (0..11)
        .map(|i| {
            (
                format!("item-{i}"),
                format!("Entry {i}"),
                "Body.".to_owned(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    reader.subscribe(&pool, "/feed.xml", &borrowed).await;

    let first_page = body_string(reader.get("/all").await).await;
    assert_eq!(count_entries(&first_page), 10);
    assert!(first_page.contains(">Older<"));

    // The href is HTML-escaped (`&` becomes `&#38;`), which is correct markup but needs
    // undoing before the raw query string can be replayed as a request.
    let query = first_page
        .split("/all?after=")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .map(|q| q.replace("&#38;", "&").replace("&amp;", "&"))
        .expect("an older link should be present");
    let second_page = body_string(reader.get(&format!("/all?after={query}")).await).await;
    assert_eq!(count_entries(&second_page), 1);
    assert!(!second_page.contains(">Older<"));
}

// ---------------------------------------------------------------- CSRF

#[sqlx::test]
async fn state_changing_entry_actions_require_a_csrf_token(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(&pool, "/feed.xml", &[("a", "First", "Body.")])
        .await;
    let id = entry_ids_newest_first(&pool).await[0];

    assert_eq!(
        reader
            .post_without_csrf(&format!("/entries/{id}/read"), "view=unread")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reader
            .post_without_csrf(&format!("/entries/{id}/star"), "view=unread")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reader
            .post_without_csrf("/entries/read-all", "view=unread")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}
