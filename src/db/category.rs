//! Categories. Every feed belongs to exactly one.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// What a new instance gets, so that adding a feed never requires making a category first.
pub const DEFAULT_TITLE: &str = "Uncategorized";

#[derive(Debug, Clone)]
pub struct Category {
    pub id: i64,
    pub title: String,
    pub created_at: DateTime<Utc>,
}

/// A category with its feed count, for the categories page.
#[derive(Debug, Clone)]
pub struct CategoryWithCounts {
    pub id: i64,
    pub title: String,
    pub feed_count: i64,
    pub unread_count: i64,
}

pub async fn list(pool: &PgPool, user_id: i64) -> Result<Vec<Category>, sqlx::Error> {
    sqlx::query_as!(
        Category,
        "select id, title, created_at from categories
         where user_id = $1
         order by lower(title)",
        user_id,
    )
    .fetch_all(pool)
    .await
}

/// Categories with how many feeds and unread entries each holds. One query rather than a
/// count per row, and the unread join hits the partial index.
pub async fn list_with_counts(
    pool: &PgPool,
    user_id: i64,
) -> Result<Vec<CategoryWithCounts>, sqlx::Error> {
    sqlx::query!(
        "select c.id,
                c.title,
                count(distinct f.id) as feed_count,
                count(e.id) as unread_count
         from categories c
         left join feeds f on f.category_id = c.id
         left join entries e on e.feed_id = f.id and e.read_at is null
         where c.user_id = $1
         group by c.id, c.title
         order by lower(c.title)",
        user_id,
    )
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| CategoryWithCounts {
                id: row.id,
                title: row.title,
                feed_count: row.feed_count.unwrap_or(0),
                unread_count: row.unread_count.unwrap_or(0),
            })
            .collect()
    })
}

pub async fn find(pool: &PgPool, user_id: i64, id: i64) -> Result<Option<Category>, sqlx::Error> {
    sqlx::query_as!(
        Category,
        "select id, title, created_at from categories where user_id = $1 and id = $2",
        user_id,
        id,
    )
    .fetch_optional(pool)
    .await
}

pub async fn create(pool: &PgPool, user_id: i64, title: &str) -> Result<Category, sqlx::Error> {
    sqlx::query_as!(
        Category,
        "insert into categories (user_id, title) values ($1, $2)
         returning id, title, created_at",
        user_id,
        title.trim(),
    )
    .fetch_one(pool)
    .await
}

/// Finds a category by title or creates it. Used by OPML import, where the same category
/// name appears on many feeds.
pub async fn find_or_create(
    pool: &PgPool,
    user_id: i64,
    title: &str,
) -> Result<Category, sqlx::Error> {
    sqlx::query_as!(
        Category,
        "insert into categories (user_id, title) values ($1, $2)
         on conflict (user_id, title) do update set title = categories.title
         returning id, title, created_at",
        user_id,
        title.trim(),
    )
    .fetch_one(pool)
    .await
}

pub async fn rename(
    pool: &PgPool,
    user_id: i64,
    id: i64,
    title: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "update categories set title = $3 where user_id = $1 and id = $2",
        user_id,
        id,
        title.trim(),
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Deletes an empty category. The `on delete restrict` on `feeds.category_id` means a
/// category with feeds in it raises a foreign-key error rather than taking the feeds and
/// all their entries with it; the handler turns that into a readable message.
pub async fn delete(pool: &PgPool, user_id: i64, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "delete from categories where user_id = $1 and id = $2",
        user_id,
        id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}
