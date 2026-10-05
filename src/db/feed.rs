//! Feeds: subscriptions, their conditional-GET state, and their health.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone)]
pub struct Feed {
    pub id: i64,
    pub user_id: i64,
    pub category_id: i64,
    pub title: String,
    pub feed_url: String,
    pub site_url: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub next_check_at: DateTime<Utc>,
    pub check_interval_seconds: i32,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub failure_count: i32,
    pub rate_limited_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub disabled_at: Option<DateTime<Utc>>,
}

/// A feed as the subscription panel shows it: with its category name and unread count.
#[derive(Debug, Clone)]
pub struct FeedSummary {
    pub id: i64,
    pub title: String,
    pub feed_url: String,
    pub site_url: Option<String>,
    pub category_id: i64,
    pub category_title: String,
    pub unread_count: i64,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub failure_count: i32,
    pub rate_limited_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl FeedSummary {
    /// What the panel says about this feed's health. Returned as a word plus a severity so
    /// the template never conveys the state by colour alone.
    pub fn status(&self) -> (&'static str, &'static str) {
        if self.disabled_at.is_some() {
            ("warning", "Disabled")
        } else if self
            .rate_limited_until
            .is_some_and(|until| until > Utc::now())
        {
            ("warning", "Throttled")
        } else if self.failure_count > 0 {
            ("error", "Failing")
        } else if self.last_success_at.is_some() {
            ("success", "OK")
        } else {
            ("", "Not checked yet")
        }
    }
}

pub async fn list_summaries(pool: &PgPool, user_id: i64) -> Result<Vec<FeedSummary>, sqlx::Error> {
    sqlx::query!(
        "select f.id,
                f.title,
                f.feed_url,
                f.site_url,
                f.category_id,
                c.title as category_title,
                f.last_checked_at,
                f.last_success_at,
                f.next_check_at,
                f.failure_count,
                f.rate_limited_until,
                f.last_error,
                f.disabled_at,
                count(e.id) as unread_count
         from feeds f
         join categories c on c.id = f.category_id
         left join entries e on e.feed_id = f.id and e.read_at is null
         where f.user_id = $1
         group by f.id, c.title
         order by lower(c.title), lower(f.title)",
        user_id,
    )
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| FeedSummary {
                id: row.id,
                title: row.title,
                feed_url: row.feed_url,
                site_url: row.site_url,
                category_id: row.category_id,
                category_title: row.category_title,
                unread_count: row.unread_count.unwrap_or(0),
                last_checked_at: row.last_checked_at,
                last_success_at: row.last_success_at,
                next_check_at: row.next_check_at,
                failure_count: row.failure_count,
                rate_limited_until: row.rate_limited_until,
                last_error: row.last_error,
                disabled_at: row.disabled_at,
            })
            .collect()
    })
}

pub async fn find(pool: &PgPool, user_id: i64, id: i64) -> Result<Option<Feed>, sqlx::Error> {
    sqlx::query_as!(
        Feed,
        "select id, user_id, category_id, title, feed_url, site_url, etag, last_modified,
                next_check_at, check_interval_seconds, last_checked_at, last_success_at,
                failure_count, rate_limited_until, last_error, disabled_at
         from feeds
         where user_id = $1 and id = $2",
        user_id,
        id,
    )
    .fetch_optional(pool)
    .await
}

/// Subscribes to a feed.
///
/// `next_check_at` is jittered across the first interval rather than set to `now()`. An
/// OPML import of two hundred feeds would otherwise schedule two hundred simultaneous
/// fetches, and keep doing so on every interval boundary forever.
pub async fn create(
    pool: &PgPool,
    user_id: i64,
    category_id: i64,
    title: &str,
    feed_url: &str,
    site_url: Option<&str>,
    check_interval_seconds: i32,
) -> Result<Feed, sqlx::Error> {
    sqlx::query_as!(
        Feed,
        "insert into feeds (user_id, category_id, title, feed_url, site_url,
                            check_interval_seconds, next_check_at)
         -- $6 is cast explicitly in both places: without it Postgres deduces int4 for the
         -- column and float8 for make_interval's argument, and refuses the inconsistency.
         values ($1, $2, $3, $4, $5, $6::int4,
                 now() + make_interval(secs => random() * $6::int4))
         returning id, user_id, category_id, title, feed_url, site_url, etag, last_modified,
                   next_check_at, check_interval_seconds, last_checked_at, last_success_at,
                   failure_count, rate_limited_until, last_error, disabled_at",
        user_id,
        category_id,
        title.trim(),
        feed_url.trim(),
        site_url,
        check_interval_seconds,
    )
    .fetch_one(pool)
    .await
}

/// Edits the parts of a feed a reader controls. Changing the URL clears the
/// conditional-GET state and the failure count: the stored validators belong to the old
/// URL, and replaying them against a different document would wrongly produce a 304.
pub async fn update(
    pool: &PgPool,
    user_id: i64,
    id: i64,
    category_id: i64,
    title: &str,
    feed_url: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "update feeds
         set title = $4,
             category_id = $3,
             feed_url = $5,
             etag = case when feeds.feed_url = $5 then feeds.etag else null end,
             last_modified = case when feeds.feed_url = $5 then feeds.last_modified else null end,
             failure_count = case when feeds.feed_url = $5 then feeds.failure_count else 0 end,
             last_error = case when feeds.feed_url = $5 then feeds.last_error else null end,
             disabled_at = case when feeds.feed_url = $5 then feeds.disabled_at else null end
         where user_id = $1 and id = $2",
        user_id,
        id,
        category_id,
        title.trim(),
        feed_url.trim(),
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn delete(pool: &PgPool, user_id: i64, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "delete from feeds where user_id = $1 and id = $2",
        user_id,
        id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Feeds the poller should fetch right now, across every user — there is only ever one
/// fetch path, and it is not scoped to a reader the way every other query here is.
///
/// Ordered oldest-due-first and capped, so a very large backlog (a bulk OPML import, say)
/// is worked off a batch at a time across ticks rather than in one unbounded pass.
pub async fn due(pool: &PgPool, limit: i64) -> Result<Vec<Feed>, sqlx::Error> {
    sqlx::query_as!(
        Feed,
        "select id, user_id, category_id, title, feed_url, site_url, etag, last_modified,
                next_check_at, check_interval_seconds, last_checked_at, last_success_at,
                failure_count, rate_limited_until, last_error, disabled_at
         from feeds
         where disabled_at is null and next_check_at <= now()
         order by next_check_at
         limit $1",
        limit,
    )
    .fetch_all(pool)
    .await
}

/// Brings a feed's next check forward to now. The poller is then woken, so a manual
/// refresh shares the scheduler's single fetch path instead of adding a second one.
pub async fn request_refresh(pool: &PgPool, user_id: i64, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "update feeds
         set next_check_at = now(), rate_limited_until = null, disabled_at = null
         where user_id = $1 and id = $2",
        user_id,
        id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// The same, for every feed the reader has.
pub async fn request_refresh_all(pool: &PgPool, user_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "update feeds
         set next_check_at = now(), rate_limited_until = null, disabled_at = null
         where user_id = $1",
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
