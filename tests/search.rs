//! Full-text search over titles and content.

mod support;

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
    /// would otherwise perform (not running in this test harness), so the entries exist
    /// — and are indexed — before a search is made.
    async fn subscribe(&self, pool: &PgPool, at: &str, items: &[(&str, &str, &str)]) {
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

#[sqlx::test]
async fn visiting_search_with_no_term_prompts_rather_than_searching(pool: PgPool) {
    let reader = Reader::new(&pool).await;

    let body = body_string(reader.get("/search").await).await;

    assert!(body.contains("Search your entries"));
}

#[sqlx::test]
async fn a_search_finds_matches_in_the_title(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(
            &pool,
            "/feed.xml",
            &[
                ("a", "Rust is great for systems work", "Body."),
                ("b", "Baking bread at home", "Body."),
            ],
        )
        .await;

    let body = body_string(reader.get("/search?q=rust").await).await;

    assert!(body.contains("Rust is great for systems work"));
    assert!(!body.contains("Baking bread at home"));
}

#[sqlx::test]
async fn a_search_finds_matches_in_the_content_and_not_only_the_title(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(
            &pool,
            "/feed.xml",
            &[("a", "An update", "Our new pricing takes effect in January.")],
        )
        .await;

    let body = body_string(reader.get("/search?q=pricing").await).await;

    assert!(body.contains("An update"));
}

#[sqlx::test]
async fn a_search_with_no_matches_says_so(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(
            &pool,
            "/feed.xml",
            &[("a", "Baking bread at home", "Body.")],
        )
        .await;

    let body = body_string(reader.get("/search?q=astrophysics").await).await;

    assert!(body.contains("No matches"));
}

#[sqlx::test]
async fn search_is_scoped_to_the_signed_in_reader_like_every_other_view(pool: PgPool) {
    let reader = Reader::new(&pool).await;
    reader
        .subscribe(&pool, "/feed.xml", &[("a", "Searchable headline", "Body.")])
        .await;

    // Blank and whitespace-only terms are "nothing searched yet", not "match nothing".
    let blank = body_string(reader.get("/search?q=").await).await;
    assert!(blank.contains("Search your entries"));

    let whitespace = body_string(reader.get("/search?q=%20%20").await).await;
    assert!(whitespace.contains("Search your entries"));
}

#[sqlx::test]
async fn the_search_box_echoes_back_the_term_that_was_searched(pool: PgPool) {
    let reader = Reader::new(&pool).await;

    let body = body_string(reader.get("/search?q=astrophysics").await).await;

    assert!(body.contains(r#"value="astrophysics""#));
}
