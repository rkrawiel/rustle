//! The background poller: fetches due feeds, and runs retention and session cleanup.
//!
//! There is only one code path that fetches a feed — this one. Manual refresh (`src/web/
//! feeds.rs`) and a new subscription (Phase 4) only bring a feed's `next_check_at` forward
//! and wake this loop; the actual `ingest::refresh_feed` call always happens here.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::db::{entry, feed, session};
use crate::feed::ingest;
use crate::state::AppState;

/// How often the poller looks for due feeds. Independent of any one feed's own interval
/// (`feeds.check_interval_seconds`, set from `RUSTLE_POLL_INTERVAL_MINUTES` at subscribe
/// time) — this is only how promptly a newly-due feed gets noticed.
const FEED_TICK: Duration = Duration::from_secs(60);

/// How often retention and expired-session cleanup run. Cheap, but pointless to repeat as
/// often as the feed tick.
const CLEANUP_TICK: Duration = Duration::from_secs(60 * 60);

/// Feeds fetched per tick before the next tick looks again — comfortably above the "well
/// under 200 feeds" scale this project targets (`PLAN.md` §1), so one tick normally drains
/// the whole due set; a larger backlog (a bulk OPML import) is worked off over several.
const BATCH: i64 = 500;

/// Runs until `shutdown` is cancelled. Spawned once at startup; `AppState` is cheap to
/// clone, so the task owns its handle rather than borrowing the server's.
pub async fn run(state: AppState, shutdown: CancellationToken) {
    let mut feed_tick = time::interval(FEED_TICK);
    feed_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut cleanup_tick = time::interval(CLEANUP_TICK);
    cleanup_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!("poller stopped");
                return;
            }
            _ = feed_tick.tick() => poll_due_feeds(&state).await,
            () = state.poller_woken() => poll_due_feeds(&state).await,
            _ = cleanup_tick.tick() => run_cleanup(&state).await,
        }
    }
}

async fn poll_due_feeds(state: &AppState) {
    let due = match feed::due(state.db(), BATCH).await {
        Ok(due) => due,
        Err(err) => {
            tracing::error!("selecting due feeds failed: {err}");
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    tracing::debug!("polling {} due feed(s)", due.len());

    let limiter = Arc::new(Semaphore::new(state.config().fetch_concurrency));
    let mut tasks = JoinSet::new();
    for due_feed in due {
        let pool = state.db().clone();
        let fetcher = state.fetcher().clone();
        let limiter = Arc::clone(&limiter);
        tasks.spawn(async move {
            // Held for the whole fetch, not just the acquire: this is what makes
            // `RUSTLE_FETCH_CONCURRENCY` an actual concurrency cap rather than a limit on
            // how many fetches may *start* at once.
            let _permit = limiter
                .acquire()
                .await
                .expect("the semaphore is never closed");
            let id = due_feed.id;
            if let Err(err) = ingest::refresh_feed(&pool, &fetcher, &due_feed).await {
                tracing::warn!(feed.id = id, "polling failed: {err}");
            }
        });
    }

    // `JoinSet` isolates a panic in one feed's fetch to that one task, so a single
    // malformed feed cannot take the rest of this poll — or the next one — down with it.
    while let Some(result) = tasks.join_next().await {
        if let Err(err) = result {
            tracing::error!("a feed poll task panicked: {err}");
        }
    }
}

async fn run_cleanup(state: &AppState) {
    match entry::delete_expired(state.db()).await {
        Ok(0) => {}
        Ok(removed) => {
            tracing::info!(
                "retention cleanup removed {removed} entr{}",
                if removed == 1 { "y" } else { "ies" }
            );
        }
        Err(err) => tracing::error!("retention cleanup failed: {err}"),
    }

    match session::delete_expired(state.db()).await {
        Ok(0) => {}
        Ok(removed) => tracing::debug!("purged {removed} expired session(s)"),
        Err(err) => tracing::error!("session cleanup failed: {err}"),
    }
}
