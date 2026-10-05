//! The single user row, and the settings hanging off it.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::theme::Theme;

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub theme: Theme,
    pub entries_per_page: i32,
    pub retention_days: i32,
    pub created_at: DateTime<Utc>,
}

/// The shape the database returns. Separate from `User` only because `theme` is stored as
/// text and `Theme` is not a sqlx type; one `From` keeps that conversion in a single place.
///
/// The column list is repeated in each query below rather than shared through a constant:
/// `query_as!` needs a string literal at macro-expansion time, so `concat!` of a shared
/// fragment does not work.
struct Row {
    id: i64,
    username: String,
    password_hash: String,
    theme: String,
    entries_per_page: i32,
    retention_days: i32,
    created_at: DateTime<Utc>,
}

impl From<Row> for User {
    fn from(row: Row) -> Self {
        Self {
            id: row.id,
            username: row.username,
            password_hash: row.password_hash,
            theme: Theme::from_db(&row.theme),
            entries_per_page: row.entries_per_page,
            retention_days: row.retention_days,
            created_at: row.created_at,
        }
    }
}

/// Whether setup has already happened. Drives both the `/setup` guard and the redirect
/// every other route performs when there is nobody to log in as.
pub async fn exists(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!("select exists (select 1 from users)")
        .fetch_one(pool)
        .await?;
    Ok(found.unwrap_or(false))
}

/// Creates the one user. The `/setup` guard is what enforces single-user; this will simply
/// fail with a unique violation if called twice with the same name.
///
/// `retention_days` seeds from `RUSTLE_RETENTION_DAYS` rather than the column's own
/// default of 0 (keep forever), so an operator's configured policy applies to the account
/// from the moment it exists rather than only after a first visit to Settings.
pub async fn create(
    pool: &PgPool,
    username: &str,
    password_hash: &str,
    retention_days: i32,
) -> Result<User, sqlx::Error> {
    sqlx::query_as!(
        Row,
        "insert into users (username, password_hash, retention_days)
         values ($1, $2, $3)
         returning id, username, password_hash, theme, entries_per_page, retention_days,
                   created_at",
        username,
        password_hash,
        retention_days,
    )
    .fetch_one(pool)
    .await
    .map(User::from)
}

pub async fn find_by_username(pool: &PgPool, username: &str) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as!(
        Row,
        "select id, username, password_hash, theme, entries_per_page, retention_days,
                created_at
         from users
         where username = $1",
        username,
    )
    .fetch_optional(pool)
    .await
    .map(|row| row.map(User::from))
}

pub async fn find_by_id(pool: &PgPool, id: i64) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as!(
        Row,
        "select id, username, password_hash, theme, entries_per_page, retention_days,
                created_at
         from users
         where id = $1",
        id,
    )
    .fetch_optional(pool)
    .await
    .map(|row| row.map(User::from))
}

pub async fn set_password_hash(
    pool: &PgPool,
    id: i64,
    password_hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "update users set password_hash = $2 where id = $1",
        id,
        password_hash
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_theme(pool: &PgPool, id: i64, theme: Theme) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "update users set theme = $2 where id = $1",
        id,
        theme.as_str()
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Entries per page and retention together, since the Settings page submits them as one
/// form. The database's own `check` constraints are the last word on the valid ranges;
/// the handler validates first only so a bad value gets a sentence back instead of a
/// generic 500.
pub async fn set_preferences(
    pool: &PgPool,
    id: i64,
    entries_per_page: i32,
    retention_days: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "update users set entries_per_page = $2, retention_days = $3 where id = $1",
        id,
        entries_per_page,
        retention_days,
    )
    .execute(pool)
    .await?;
    Ok(())
}
