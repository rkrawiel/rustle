//! `refresh_feed` — the only code path that fetches a feed.
//!
//! The scheduler calls it, and "refresh now" calls it by bringing `next_check_at` forward
//! and waking the scheduler. There is deliberately no second, synchronous fetch path: two
//! would drift apart, and a user mashing refresh could then outrun the concurrency cap.

use chrono::Utc;
use sqlx::PgPool;
use url::Url;

use crate::db::feed::Feed;
use crate::feed::backoff::{self, Outcome};
use crate::feed::fetch::{self, Fetched, Fetcher};
use crate::feed::parse;

/// Consecutive failures after which a feed stops being polled.
///
/// Counted in attempts rather than days for simplicity, but the exponential backoff means
/// twelve failures already span well over a week — so this is "give up on a feed that has
/// been dead for a long time", not "give up after twelve bad minutes".
const DISABLE_AFTER_FAILURES: i32 = 12;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ingested {
    pub added: u64,
    pub updated: u64,
    /// Entries seen again whose content had not changed. Counted because it is the normal
    /// case, and a poll that reports nothing but this is working correctly.
    pub unchanged: u64,
}

/// Polls one feed and records the result.
///
/// Never returns an error for a feed that simply failed: the failure is recorded on the
/// row and reflected in the schedule, which is what the subscription panel reads. Only a
/// database problem propagates, because that is ours and not the publisher's.
pub async fn refresh_feed(
    pool: &PgPool,
    fetcher: &Fetcher,
    feed: &Feed,
) -> Result<Ingested, sqlx::Error> {
    let now = Utc::now();
    let base_interval = std::time::Duration::from_secs(feed.check_interval_seconds.max(1) as u64);

    let Ok(url) = Url::parse(&feed.feed_url) else {
        record_failure(pool, feed, now, base_interval, "that address is not valid").await?;
        return Ok(Ingested::default());
    };

    let fetched = match fetch::fetch(
        fetcher,
        &url,
        feed.etag.as_deref(),
        feed.last_modified.as_deref(),
    )
    .await
    {
        Ok(fetched) => fetched,
        Err(err) => {
            record_failure(pool, feed, now, base_interval, &err.to_string()).await?;
            return Ok(Ingested::default());
        }
    };

    match fetched {
        Fetched::RateLimited { retry_after } => {
            let schedule = backoff::next(
                now,
                base_interval,
                feed.failure_count,
                Outcome::RateLimited { retry_after },
                None,
            );
            // The failure count is deliberately left alone, so a throttled feed is not
            // also treated as a dying one.
            sqlx::query!(
                "update feeds
                 set last_checked_at = $2,
                     next_check_at = $3,
                     rate_limited_until = $4,
                     last_error = $5
                 where id = $1",
                feed.id,
                now,
                schedule.next_check_at,
                schedule.rate_limited_until,
                Some("the server asked us to slow down"),
            )
            .execute(pool)
            .await?;
            Ok(Ingested::default())
        }

        Fetched::NotModified => {
            // A 304 is a success: the feed is alive and we transferred nothing.
            let schedule = backoff::next(
                now,
                base_interval,
                feed.failure_count,
                Outcome::Success,
                None,
            );
            record_success(pool, feed.id, now, &schedule, None, None, None, None).await?;
            Ok(Ingested::default())
        }

        Fetched::Body {
            bytes,
            etag,
            last_modified,
            final_url,
            max_age,
        } => {
            let channel = match parse::parse(&bytes, &final_url, now) {
                Ok(channel) => channel,
                Err(err) => {
                    record_failure(pool, feed, now, base_interval, &err.to_string()).await?;
                    return Ok(Ingested::default());
                }
            };

            let schedule = backoff::next(
                now,
                base_interval,
                feed.failure_count,
                Outcome::Success,
                max_age,
            );

            record_success(
                pool,
                feed.id,
                now,
                &schedule,
                etag.as_deref(),
                last_modified.as_deref(),
                // The feed's own title replaces the placeholder taken from the URL, but
                // only on the first successful fetch: after that the reader may have
                // renamed it, and a poll should not undo that.
                channel
                    .title
                    .as_deref()
                    .filter(|_| feed.last_success_at.is_none()),
                channel.site_url.as_deref(),
            )
            .await?;

            store_entries(pool, feed, &channel.entries).await
        }
    }
}

/// Inserts new entries and updates the ones whose content really changed.
async fn store_entries(
    pool: &PgPool,
    feed: &Feed,
    entries: &[parse::ParsedEntry],
) -> Result<Ingested, sqlx::Error> {
    let mut counts = Ingested::default();

    for entry in entries {
        // `xmax = 0` is the standard Postgres trick for telling an INSERT apart from an
        // ON CONFLICT UPDATE in a single statement.
        //
        // The conflict clause only writes when `content_hash` differs. Some feeds rewrite
        // their content on every fetch — an ad token, a rendered timestamp — and treating
        // that as a change would reset read state over and over.
        let row = sqlx::query!(
            "insert into entries (user_id, feed_id, guid_hash, title, url, author,
                                  content, content_text, content_hash, published_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             on conflict (feed_id, guid_hash) do update
               set title = excluded.title,
                   url = excluded.url,
                   author = excluded.author,
                   content = excluded.content,
                   content_text = excluded.content_text,
                   content_hash = excluded.content_hash
               where entries.content_hash <> excluded.content_hash
             returning (xmax = 0) as \"inserted!\"",
            feed.user_id,
            feed.id,
            entry.guid_hash,
            entry.title,
            entry.url,
            entry.author,
            entry.content,
            entry.content_text,
            entry.content_hash,
            entry.published_at,
        )
        .fetch_optional(pool)
        .await?;

        match row {
            Some(row) if row.inserted => counts.added += 1,
            Some(_) => counts.updated += 1,
            // No row returned: the conflict matched but the `where` suppressed the write,
            // which is the "nothing actually changed" case.
            None => counts.unchanged += 1,
        }
    }

    Ok(counts)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one UPDATE, one parameter per column"
)]
async fn record_success(
    pool: &PgPool,
    feed_id: i64,
    now: chrono::DateTime<Utc>,
    schedule: &backoff::Schedule,
    etag: Option<&str>,
    last_modified: Option<&str>,
    title: Option<&str>,
    site_url: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "update feeds
         set last_checked_at = $2,
             last_success_at = $2,
             next_check_at = $3,
             failure_count = 0,
             rate_limited_until = null,
             last_error = null,
             disabled_at = null,
             etag = coalesce($4, feeds.etag),
             last_modified = coalesce($5, feeds.last_modified),
             title = coalesce($6, feeds.title),
             site_url = coalesce($7, feeds.site_url)
         where id = $1",
        feed_id,
        now,
        schedule.next_check_at,
        etag,
        last_modified,
        title,
        site_url,
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn record_failure(
    pool: &PgPool,
    feed: &Feed,
    now: chrono::DateTime<Utc>,
    base_interval: std::time::Duration,
    message: &str,
) -> Result<(), sqlx::Error> {
    let schedule = backoff::next(
        now,
        base_interval,
        feed.failure_count,
        Outcome::Failure,
        None,
    );

    // A feed that has failed this many times in a row is gone, not slow. Stop asking,
    // and show it as disabled so the reader can fix or remove it.
    let disabled_at = (schedule.failure_count >= DISABLE_AFTER_FAILURES).then_some(now);
    if disabled_at.is_some() {
        tracing::warn!(
            "disabling feed {} after {} consecutive failures: {message}",
            feed.feed_url,
            schedule.failure_count
        );
    }

    sqlx::query!(
        "update feeds
         set last_checked_at = $2,
             next_check_at = $3,
             failure_count = $4,
             last_error = $5,
             rate_limited_until = null,
             disabled_at = coalesce($6, feeds.disabled_at)
         where id = $1",
        feed.id,
        now,
        schedule.next_check_at,
        schedule.failure_count,
        message,
        disabled_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}
