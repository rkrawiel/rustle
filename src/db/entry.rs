//! Entries: the articles themselves, and the read and starred state over them.
//!
//! **One static query per view, not one dynamic query.** sqlx restricts `query_as!` to a
//! string literal, so a shared `WHERE` builder is impossible — and that is the right
//! outcome anyway. The tempting single query with nullable filter parameters
//! (`AND ($2::bool IS NOT TRUE OR read_at IS NULL)`) would stop Postgres matching the
//! partial indexes in `0001_baseline.sql`, which is where the sub-millisecond list
//! queries come from. The repetition below is the price of index-matched reads.
//!
//! Paging is **keyset**, not `OFFSET`. Not for speed — at 10k rows `OFFSET` is
//! sub-millisecond — but for correctness: opening an entry marks it read, so the unread
//! list shrinks under the reader and offset paging silently skips entries. The comparator
//! `(published_at, id) < ($1, $2)` is also exactly what entry prev/next needs, so both
//! use the same ordering.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// One row of a list page.
#[derive(Debug, Clone)]
pub struct EntrySummary {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    pub url: Option<String>,
    pub published_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    pub starred_at: Option<DateTime<Utc>>,
}

impl EntrySummary {
    pub fn is_unread(&self) -> bool {
        self.read_at.is_none()
    }

    pub fn is_starred(&self) -> bool {
        self.starred_at.is_some()
    }
}

/// A single entry, with its content.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub content: String,
    pub published_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    pub starred_at: Option<DateTime<Utc>>,
}

/// Which list is being shown. Also what `mark_all_read` and prev/next are scoped to, so a
/// "mark all as read" from the starred view cannot touch anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Unread,
    All,
    Starred,
    Feed(i64),
    Category(i64),
}

impl View {
    /// The navigation item to mark as current.
    pub fn nav(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::All => "all",
            Self::Starred => "starred",
            Self::Feed(_) | Self::Category(_) => "feeds",
        }
    }

    /// The path this view lives at, for redirecting back after an action.
    pub fn path(self) -> String {
        match self {
            Self::Unread => "/unread".to_owned(),
            Self::All => "/all".to_owned(),
            Self::Starred => "/starred".to_owned(),
            Self::Feed(id) => format!("/feeds/{id}"),
            Self::Category(id) => format!("/categories/{id}"),
        }
    }
}

/// Where a keyset page starts. `None` is the first page.
///
/// Both halves are needed: `published_at` alone is not unique — a feed that publishes ten
/// entries with the same timestamp is normal — so `id` breaks the tie and keeps the order
/// total.
#[derive(Debug, Clone, Copy)]
pub struct Cursor {
    pub published_at: DateTime<Utc>,
    pub id: i64,
}

/// A page of entries, plus what the pager needs.
#[derive(Debug)]
pub struct Page {
    pub entries: Vec<EntrySummary>,
    /// The cursor for the next (older) page, or `None` at the end of the list.
    pub older: Option<Cursor>,
    /// Total matching entries, for "127 unread". Counted separately because the page
    /// query deliberately stops at `limit`.
    pub total: i64,
}

pub async fn list(
    pool: &PgPool,
    user_id: i64,
    view: View,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Page, sqlx::Error> {
    // One extra row, so the presence of a next page is known without a second query.
    let fetch = limit + 1;

    // A null timestamp means "first page". `$2 is null or (…) < ($2, $3)` still matches
    // the index, unlike a nullable *filter* parameter would, because the comparison it
    // guards is unchanged — only whether it applies at all.
    let after = cursor.map(|c| c.published_at);
    let after_id = cursor.map_or(i64::MAX, |c| c.id);

    let mut rows = match view {
        View::Unread => {
            sqlx::query_as!(
                EntrySummary,
                "select e.id, e.feed_id, f.title as feed_title, e.title, e.url,
                        e.published_at, e.read_at, e.starred_at
                 from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1
                   and e.read_at is null
                   and ($2::timestamptz is null or (e.published_at, e.id) < ($2, $3))
                 order by e.published_at desc, e.id desc
                 limit $4",
                user_id,
                after,
                after_id,
                fetch,
            )
            .fetch_all(pool)
            .await?
        }
        View::All => {
            sqlx::query_as!(
                EntrySummary,
                "select e.id, e.feed_id, f.title as feed_title, e.title, e.url,
                        e.published_at, e.read_at, e.starred_at
                 from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1
                   and ($2::timestamptz is null or (e.published_at, e.id) < ($2, $3))
                 order by e.published_at desc, e.id desc
                 limit $4",
                user_id,
                after,
                after_id,
                fetch,
            )
            .fetch_all(pool)
            .await?
        }
        View::Starred => {
            sqlx::query_as!(
                EntrySummary,
                "select e.id, e.feed_id, f.title as feed_title, e.title, e.url,
                        e.published_at, e.read_at, e.starred_at
                 from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1
                   and e.starred_at is not null
                   and ($2::timestamptz is null or (e.published_at, e.id) < ($2, $3))
                 order by e.published_at desc, e.id desc
                 limit $4",
                user_id,
                after,
                after_id,
                fetch,
            )
            .fetch_all(pool)
            .await?
        }
        View::Feed(feed_id) => {
            sqlx::query_as!(
                EntrySummary,
                "select e.id, e.feed_id, f.title as feed_title, e.title, e.url,
                        e.published_at, e.read_at, e.starred_at
                 from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1
                   and e.feed_id = $5
                   and ($2::timestamptz is null or (e.published_at, e.id) < ($2, $3))
                 order by e.published_at desc, e.id desc
                 limit $4",
                user_id,
                after,
                after_id,
                fetch,
                feed_id,
            )
            .fetch_all(pool)
            .await?
        }
        View::Category(category_id) => {
            // `category_id` lives on `feeds`, not on `entries`, on purpose: at well under
            // 200 feeds the planner serves this from the per-feed index, and denormalising
            // would add an `UPDATE entries` on every recategorisation.
            sqlx::query_as!(
                EntrySummary,
                "select e.id, e.feed_id, f.title as feed_title, e.title, e.url,
                        e.published_at, e.read_at, e.starred_at
                 from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1
                   and f.category_id = $5
                   and ($2::timestamptz is null or (e.published_at, e.id) < ($2, $3))
                 order by e.published_at desc, e.id desc
                 limit $4",
                user_id,
                after,
                after_id,
                fetch,
                category_id,
            )
            .fetch_all(pool)
            .await?
        }
    };

    let older = (rows.len() as i64 > limit).then(|| {
        // Drop the probe row and page from the last one we are actually showing.
        rows.truncate(limit as usize);
        let last = &rows[rows.len() - 1];
        Cursor {
            published_at: last.published_at,
            id: last.id,
        }
    });

    Ok(Page {
        total: count(pool, user_id, view).await?,
        entries: rows,
        older,
    })
}

/// How many entries the view holds in total.
pub async fn count(pool: &PgPool, user_id: i64, view: View) -> Result<i64, sqlx::Error> {
    let count = match view {
        View::Unread => {
            sqlx::query_scalar!(
                "select count(*) from entries where user_id = $1 and read_at is null",
                user_id
            )
            .fetch_one(pool)
            .await?
        }
        View::All => {
            sqlx::query_scalar!("select count(*) from entries where user_id = $1", user_id)
                .fetch_one(pool)
                .await?
        }
        View::Starred => {
            sqlx::query_scalar!(
                "select count(*) from entries where user_id = $1 and starred_at is not null",
                user_id
            )
            .fetch_one(pool)
            .await?
        }
        View::Feed(feed_id) => {
            sqlx::query_scalar!(
                "select count(*) from entries where user_id = $1 and feed_id = $2",
                user_id,
                feed_id
            )
            .fetch_one(pool)
            .await?
        }
        View::Category(category_id) => {
            sqlx::query_scalar!(
                "select count(*) from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1 and f.category_id = $2",
                user_id,
                category_id
            )
            .fetch_one(pool)
            .await?
        }
    };
    Ok(count.unwrap_or(0))
}

/// The number shown next to "Unread" in the navigation.
///
/// A plain count over the partial index `entries (user_id, feed_id) where read_at is null`,
/// which is index-only and covers only the unread rows — a set that does not grow with
/// history. A counter column maintained by triggers would turn mark-all-read into a
/// hot-row update contending with the poller's inserts, for no measurable gain here.
pub async fn total_unread(pool: &PgPool, user_id: i64) -> Result<i64, sqlx::Error> {
    count(pool, user_id, View::Unread).await
}

pub async fn find(pool: &PgPool, user_id: i64, id: i64) -> Result<Option<Entry>, sqlx::Error> {
    sqlx::query_as!(
        Entry,
        "select e.id, e.feed_id, f.title as feed_title, e.title, e.url, e.author,
                e.content, e.published_at, e.read_at, e.starred_at
         from entries e join feeds f on f.id = e.feed_id
         where e.user_id = $1 and e.id = $2",
        user_id,
        id,
    )
    .fetch_optional(pool)
    .await
}

/// The entry before or after this one *within the same view*, so prev/next never walks out
/// of the list the reader is in.
///
/// `Unread` is the exception: it uses the `All` ordering, because opening an entry marks it
/// read and would otherwise remove it from its own neighbour list — "next" would then skip
/// one every time.
pub async fn neighbour(
    pool: &PgPool,
    user_id: i64,
    view: View,
    from: &Entry,
    direction: Direction,
) -> Result<Option<i64>, sqlx::Error> {
    let at = from.published_at;
    let id = from.id;

    let found = match (view, direction) {
        (View::Starred, Direction::Older) => {
            sqlx::query_scalar!(
                "select id from entries
                 where user_id = $1 and starred_at is not null
                   and (published_at, id) < ($2, $3)
                 order by published_at desc, id desc limit 1",
                user_id,
                at,
                id
            )
            .fetch_optional(pool)
            .await?
        }
        (View::Starred, Direction::Newer) => {
            sqlx::query_scalar!(
                "select id from entries
                 where user_id = $1 and starred_at is not null
                   and (published_at, id) > ($2, $3)
                 order by published_at asc, id asc limit 1",
                user_id,
                at,
                id
            )
            .fetch_optional(pool)
            .await?
        }
        (View::Feed(feed_id), Direction::Older) => {
            sqlx::query_scalar!(
                "select id from entries
                 where user_id = $1 and feed_id = $4 and (published_at, id) < ($2, $3)
                 order by published_at desc, id desc limit 1",
                user_id,
                at,
                id,
                feed_id
            )
            .fetch_optional(pool)
            .await?
        }
        (View::Feed(feed_id), Direction::Newer) => {
            sqlx::query_scalar!(
                "select id from entries
                 where user_id = $1 and feed_id = $4 and (published_at, id) > ($2, $3)
                 order by published_at asc, id asc limit 1",
                user_id,
                at,
                id,
                feed_id
            )
            .fetch_optional(pool)
            .await?
        }
        (View::Category(category_id), Direction::Older) => {
            sqlx::query_scalar!(
                "select e.id from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1 and f.category_id = $4
                   and (e.published_at, e.id) < ($2, $3)
                 order by e.published_at desc, e.id desc limit 1",
                user_id,
                at,
                id,
                category_id
            )
            .fetch_optional(pool)
            .await?
        }
        (View::Category(category_id), Direction::Newer) => {
            sqlx::query_scalar!(
                "select e.id from entries e join feeds f on f.id = e.feed_id
                 where e.user_id = $1 and f.category_id = $4
                   and (e.published_at, e.id) > ($2, $3)
                 order by e.published_at asc, e.id asc limit 1",
                user_id,
                at,
                id,
                category_id
            )
            .fetch_optional(pool)
            .await?
        }
        (View::All | View::Unread, Direction::Older) => {
            sqlx::query_scalar!(
                "select id from entries
                 where user_id = $1 and (published_at, id) < ($2, $3)
                 order by published_at desc, id desc limit 1",
                user_id,
                at,
                id
            )
            .fetch_optional(pool)
            .await?
        }
        (View::All | View::Unread, Direction::Newer) => {
            sqlx::query_scalar!(
                "select id from entries
                 where user_id = $1 and (published_at, id) > ($2, $3)
                 order by published_at asc, id asc limit 1",
                user_id,
                at,
                id
            )
            .fetch_optional(pool)
            .await?
        }
    };

    Ok(found)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Further down the list: published before this one.
    Older,
    Newer,
}

/// Marks an entry read, if it was not already. Returns whether anything changed, so
/// opening an already-read entry does not pointlessly write.
pub async fn mark_read(pool: &PgPool, user_id: i64, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "update entries set read_at = now()
         where user_id = $1 and id = $2 and read_at is null",
        user_id,
        id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Flips read state. Returns the new state, or `None` if there is no such entry.
pub async fn toggle_read(
    pool: &PgPool,
    user_id: i64,
    id: i64,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!(
        "update entries
         set read_at = case when read_at is null then now() else null end
         where user_id = $1 and id = $2
         returning read_at is not null as \"is_read!\"",
        user_id,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Flips starred state. Returns the new state, or `None` if there is no such entry.
pub async fn toggle_starred(
    pool: &PgPool,
    user_id: i64,
    id: i64,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!(
        "update entries
         set starred_at = case when starred_at is null then now() else null end
         where user_id = $1 and id = $2
         returning starred_at is not null as \"is_starred!\"",
        user_id,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Marks everything in one view read, and nothing outside it.
///
/// Scoped per view rather than "everything": a reader who hits `A` on a single feed's page
/// means that feed, and silently clearing their whole backlog would be unrecoverable.
pub async fn mark_all_read(pool: &PgPool, user_id: i64, view: View) -> Result<u64, sqlx::Error> {
    let result = match view {
        View::Unread | View::All => {
            sqlx::query!(
                "update entries set read_at = now() where user_id = $1 and read_at is null",
                user_id
            )
            .execute(pool)
            .await?
        }
        View::Starred => {
            sqlx::query!(
                "update entries set read_at = now()
                 where user_id = $1 and read_at is null and starred_at is not null",
                user_id
            )
            .execute(pool)
            .await?
        }
        View::Feed(feed_id) => {
            sqlx::query!(
                "update entries set read_at = now()
                 where user_id = $1 and read_at is null and feed_id = $2",
                user_id,
                feed_id
            )
            .execute(pool)
            .await?
        }
        View::Category(category_id) => {
            sqlx::query!(
                "update entries set read_at = now()
                 where user_id = $1 and read_at is null
                   and feed_id in (select id from feeds where category_id = $2)",
                user_id,
                category_id
            )
            .execute(pool)
            .await?
        }
    };
    Ok(result.rows_affected())
}

/// Removes read, unstarred entries past each reader's own retention period.
///
/// Not scoped to one user: retention is a per-user *preference*, stored on `users` for
/// when multi-user arrives, but the cleanup itself is instance-wide, like the poller.
/// `retention_days = 0` means "keep forever" and is excluded rather than treated as
/// "expired immediately" — see `PLAN.md` §2. Starred entries are never touched, whatever
/// the setting: the brief's exemption is unconditional, not a longer retention period.
pub async fn delete_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "delete from entries e
         using users u
         where e.user_id = u.id
           and u.retention_days > 0
           and e.read_at is not null
           and e.starred_at is null
           and e.published_at < now() - make_interval(days => u.retention_days)"
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// A page of full-text search results, ranked by relevance rather than by date.
///
/// `websearch_to_tsquery` reads the term the way a search-engine box does — quoted
/// phrases, `-exclusions`, bare `or` — rather than `tsquery`'s own terse operator syntax,
/// which is not something a reader should have to learn to use the search box.
pub async fn search(
    pool: &PgPool,
    user_id: i64,
    term: &str,
    limit: i64,
) -> Result<Vec<EntrySummary>, sqlx::Error> {
    sqlx::query_as!(
        EntrySummary,
        "select e.id, e.feed_id, f.title as feed_title, e.title, e.url,
                e.published_at, e.read_at, e.starred_at
         from entries e join feeds f on f.id = e.feed_id
         where e.user_id = $1 and e.search @@ websearch_to_tsquery('english', $2)
         order by ts_rank(e.search, websearch_to_tsquery('english', $2)) desc,
                  e.published_at desc
         limit $3",
        user_id,
        term,
        limit,
    )
    .fetch_all(pool)
    .await
}

/// How many entries match, regardless of `search`'s `limit` — for "showing 50 of 212".
pub async fn search_count(pool: &PgPool, user_id: i64, term: &str) -> Result<i64, sqlx::Error> {
    let count = sqlx::query_scalar!(
        "select count(*) from entries
         where user_id = $1 and search @@ websearch_to_tsquery('english', $2)",
        user_id,
        term,
    )
    .fetch_one(pool)
    .await?;
    Ok(count.unwrap_or(0))
}
