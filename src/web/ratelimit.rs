//! Login attempt limiting.
//!
//! Keyed on the username only, not on the client address. Rustle normally sits behind a
//! reverse proxy, so the peer address is the proxy's and per-IP limiting would need us to
//! trust `X-Forwarded-For` — a header a client can forge, which would turn the limiter
//! into a way to lock the real owner out. And because a Rustle instance has exactly one
//! username, limiting by name is already a global limit on login attempts.
//!
//! In-process rather than a table: an attacker cannot restart the server, so losing the
//! counters on restart is not a bypass, and it saves a database write on every failure.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Attempts allowed per window. Generous enough to survive a forgotten password, far too
/// slow to be worth guessing against Argon2.
const MAX_ATTEMPTS: u32 = 10;
const WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Default)]
pub struct LoginLimiter {
    windows: Arc<Mutex<HashMap<String, Window>>>,
}

struct Window {
    started: Instant,
    attempts: u32,
}

impl LoginLimiter {
    /// Records an attempt and reports whether it is allowed. Call this *before* hashing:
    /// the point is to avoid spending 19 MiB and a core on a guess.
    pub fn allow(&self, username: &str) -> bool {
        self.allow_at(username, Instant::now())
    }

    fn allow_at(&self, username: &str, now: Instant) -> bool {
        let key = username.trim().to_lowercase();
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // One user means one key, so this never grows; the sweep exists so that a
        // future multi-user build does not leak an entry per attempted name.
        windows.retain(|_, window| now.duration_since(window.started) < WINDOW);

        let window = windows.entry(key).or_insert(Window {
            started: now,
            attempts: 0,
        });

        if now.duration_since(window.started) >= WINDOW {
            window.started = now;
            window.attempts = 0;
        }

        window.attempts += 1;
        window.attempts <= MAX_ATTEMPTS
    }

    /// Clears the count after a successful login, so a legitimate owner who fumbled the
    /// password a few times is not left near the limit.
    pub fn reset(&self, username: &str) {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&username.trim().to_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempts_are_allowed_up_to_the_limit_then_refused() {
        let limiter = LoginLimiter::default();
        for attempt in 1..=MAX_ATTEMPTS {
            assert!(
                limiter.allow("reader"),
                "attempt {attempt} should be allowed"
            );
        }
        assert!(
            !limiter.allow("reader"),
            "one past the limit must be refused"
        );
        assert!(!limiter.allow("reader"), "and it stays refused");
    }

    #[test]
    fn the_key_is_case_and_whitespace_insensitive_so_it_cannot_be_dodged() {
        let limiter = LoginLimiter::default();
        for _ in 0..MAX_ATTEMPTS {
            assert!(limiter.allow("reader"));
        }
        assert!(!limiter.allow("  READER  "));
    }

    #[test]
    fn a_successful_login_clears_the_count() {
        let limiter = LoginLimiter::default();
        for _ in 0..MAX_ATTEMPTS {
            limiter.allow("reader");
        }
        assert!(!limiter.allow("reader"));

        limiter.reset("reader");
        assert!(limiter.allow("reader"));
    }

    #[test]
    fn the_window_expires() {
        let limiter = LoginLimiter::default();
        let start = Instant::now();
        for _ in 0..MAX_ATTEMPTS {
            limiter.allow_at("reader", start);
        }
        assert!(!limiter.allow_at("reader", start));
        assert!(limiter.allow_at("reader", start + WINDOW + Duration::from_secs(1)));
    }

    #[test]
    fn different_names_are_counted_separately() {
        let limiter = LoginLimiter::default();
        for _ in 0..=MAX_ATTEMPTS {
            limiter.allow("reader");
        }
        assert!(!limiter.allow("reader"));
        assert!(limiter.allow("someone-else"));
    }
}
