//! Shared application state, cheap to clone and handed to every handler.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Notify;

use crate::config::Config;
use crate::db::session::SessionToken;
use crate::feed::fetch::Fetcher;
use crate::password::Hasher;
use crate::web::ratelimit::LoginLimiter;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    config: Config,
    db: PgPool,
    hasher: Hasher,
    login_limiter: LoginLimiter,
    setup_token: String,
    poll_now: Notify,
    fetcher: Fetcher,
}

impl AppState {
    pub fn new(config: Config, db: PgPool) -> Self {
        let fetcher = Fetcher::new(&config).expect("the HTTP client configuration is valid");
        Self::with_fetcher(config, db, fetcher)
    }

    /// Lets a test substitute a fetcher that will talk to a local mock server.
    pub fn with_fetcher(config: Config, db: PgPool, fetcher: Fetcher) -> Self {
        // An operator can pin the token to script an unattended first run; otherwise it is
        // generated per process and printed to the log. A restart invalidating an unused
        // token is the right trade: it keeps a token out of the shell history and off disk.
        let setup_token = config
            .setup_token
            .clone()
            .unwrap_or_else(|| SessionToken::generate().as_str().to_owned());

        Self(Arc::new(Inner {
            config,
            db,
            hasher: Hasher::default(),
            login_limiter: LoginLimiter::default(),
            setup_token,
            poll_now: Notify::new(),
            fetcher,
        }))
    }

    pub fn config(&self) -> &Config {
        &self.0.config
    }

    pub fn db(&self) -> &PgPool {
        &self.0.db
    }

    pub fn hasher(&self) -> &Hasher {
        &self.0.hasher
    }

    pub fn login_limiter(&self) -> &LoginLimiter {
        &self.0.login_limiter
    }

    /// The one-time token `/setup` demands, so that a fresh public instance cannot be
    /// claimed by whoever finds it first.
    pub fn setup_token(&self) -> &str {
        &self.0.setup_token
    }

    pub fn fetcher(&self) -> &Fetcher {
        &self.0.fetcher
    }
}

impl AppState {
    /// Wakes the poller so a manual refresh is acted on immediately instead of at the
    /// next tick. The poller selects due feeds either way, so this adds no second code
    /// path for fetching — only a nudge.
    pub fn wake_poller(&self) {
        self.0.poll_now.notify_one();
    }

    /// Awaited by the poller's `select!`. Notifications issued while nothing is waiting
    /// are remembered, so a refresh requested during a fetch is not lost.
    pub async fn poller_woken(&self) {
        self.0.poll_now.notified().await;
    }
}
