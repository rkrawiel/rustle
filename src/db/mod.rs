//! The repository layer. Every query here is compile-time checked against the schema in
//! `migrations/`; see `PLAN.md` §4 for the `.sqlx` cache workflow.

pub mod category;
pub mod entry;
pub mod feed;
pub mod session;
pub mod user;

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Embedded at compile time, so a release binary carries its own schema and needs no
/// migration files on disk.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        // Keep two connections warm: acquiring a cold connection costs more than the
        // whole query budget for a list page.
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

/// Round-trips a trivial query, so `/health` reports on the database and not just on the
/// web server being able to accept a socket.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query!("select 1 as alive").fetch_one(pool).await?;
    Ok(())
}
