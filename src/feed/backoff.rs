//! When to check a feed next.
//!
//! All of it is pure, so the policy is testable without a database or a network. The
//! result is written to `feeds.next_check_at`, which is the scheduler's single input.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::RngExt;

/// Never poll faster than this, whatever a feed's `Cache-Control` says.
pub const MIN_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Never wait longer than this, however badly a feed is behaving.
pub const MAX_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Where exponential growth stops: 2^6 = 64 times the base interval, which is already
/// past `MAX_INTERVAL` for any sane configuration.
const MAX_DOUBLINGS: u32 = 6;
/// Spread, as a fraction of the delay, applied to every schedule.
const JITTER: f64 = 0.1;

/// Why we are rescheduling, which decides both the delay and whether the feed's health
/// counter moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 200 with a body we parsed, or 304 Not Modified. Both mean the feed is healthy.
    Success,
    /// The server asked us to wait. **Not** a health failure: counting it would back a
    /// merely throttled feed off exponentially and eventually disable it, which is the
    /// wrong answer to "please slow down".
    RateLimited { retry_after: Option<Duration> },
    /// DNS, TLS, a 5xx with no `Retry-After`, a 4xx, or an unparseable body.
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub next_check_at: DateTime<Utc>,
    /// Consecutive health failures, after this outcome.
    pub failure_count: i32,
    /// Set while a server has asked us to back off, so the UI can say "throttled"
    /// rather than "failing".
    pub rate_limited_until: Option<DateTime<Utc>>,
}

/// Works out the next check from the outcome and the feed's current state.
///
/// `cache_max_age` is the feed's own `Cache-Control: max-age`, honoured only as a *lower*
/// bound on the interval: a publisher may ask us to come back less often, never more.
pub fn next(
    now: DateTime<Utc>,
    base_interval: Duration,
    failure_count: i32,
    outcome: Outcome,
    cache_max_age: Option<Duration>,
) -> Schedule {
    let base = base_interval.max(MIN_INTERVAL);

    match outcome {
        Outcome::Success => {
            let interval = cache_max_age.map_or(base, |max_age| max_age.max(base));
            Schedule {
                next_check_at: now + jittered(interval.clamp(MIN_INTERVAL, MAX_INTERVAL)),
                failure_count: 0,
                rate_limited_until: None,
            }
        }

        Outcome::RateLimited { retry_after } => {
            // Take the server's word for it when it gives one, but never less than our own
            // floor and never more than a day, so a hostile or broken header cannot park a
            // feed indefinitely.
            let delay = retry_after
                .unwrap_or(base)
                .clamp(MIN_INTERVAL, MAX_INTERVAL);
            let until = now + jittered(delay);
            Schedule {
                next_check_at: until,
                // Deliberately unchanged: see `Outcome::RateLimited`.
                failure_count,
                rate_limited_until: Some(until),
            }
        }

        Outcome::Failure => {
            let failure_count = failure_count.saturating_add(1);
            let doublings = (failure_count as u32).saturating_sub(1).min(MAX_DOUBLINGS);
            let delay = base
                .saturating_mul(1u32 << doublings)
                .clamp(MIN_INTERVAL, MAX_INTERVAL);
            Schedule {
                next_check_at: now + jittered(delay),
                failure_count,
                rate_limited_until: None,
            }
        }
    }
}

/// Spreads a delay by ±10%.
///
/// Without this, every feed subscribed in the same minute is checked in the same minute
/// forever, which is both a thundering herd on our side and a visible burst on every
/// publisher's.
fn jittered(delay: Duration) -> chrono::Duration {
    let factor = 1.0 + rand::rng().random_range(-JITTER..=JITTER);
    let seconds = (delay.as_secs_f64() * factor).max(1.0);
    chrono::Duration::milliseconds((seconds * 1000.0) as i64)
}

/// Parses `Retry-After`, which may be either a number of seconds or an HTTP date.
///
/// Garbage is rejected rather than guessed at, so the caller falls back to normal backoff.
pub fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<Duration> {
    let value = value.trim();

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    // RFC 9110 fixdate, e.g. "Wed, 21 Oct 2015 07:28:00 GMT".
    //
    // chrono rejects a date whose weekday name does not match the date itself, and servers
    // do get that wrong. RFC 9110 tells recipients to ignore the weekday, so on failure we
    // drop it and try again rather than discarding the server's instruction over a typo.
    let at = DateTime::parse_from_rfc2822(value)
        .or_else(|_| {
            let without_weekday = value.split_once(',').map_or(value, |(_, rest)| rest.trim());
            DateTime::parse_from_rfc2822(without_weekday)
        })
        .ok()?;

    let delay = at.with_timezone(&Utc) - now;
    // A date in the past means "retry now", not "wait a negative amount".
    delay.to_std().ok().or(Some(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-02T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Delay in seconds, ignoring jitter.
    fn delay_secs(schedule: &Schedule) -> f64 {
        (schedule.next_check_at - now()).num_milliseconds() as f64 / 1000.0
    }

    fn hour() -> Duration {
        Duration::from_secs(3600)
    }

    #[test]
    fn success_resets_the_failure_count_and_waits_one_interval() {
        let schedule = next(now(), hour(), 4, Outcome::Success, None);

        assert_eq!(schedule.failure_count, 0);
        assert_eq!(schedule.rate_limited_until, None);
        // One hour, give or take the 10% jitter.
        assert!(
            (3240.0..=3960.0).contains(&delay_secs(&schedule)),
            "{}",
            delay_secs(&schedule)
        );
    }

    #[test]
    fn failures_back_off_exponentially_and_then_stop_growing() {
        let mut previous = 0.0;
        for failures in 0..4 {
            let schedule = next(now(), hour(), failures, Outcome::Failure, None);
            assert_eq!(schedule.failure_count, failures + 1);
            let delay = delay_secs(&schedule);
            assert!(
                delay > previous,
                "{failures}: {delay} should exceed {previous}"
            );
            previous = delay;
        }

        // Past the cap it is a day, not a week.
        let far = next(now(), hour(), 50, Outcome::Failure, None);
        assert!(delay_secs(&far) <= MAX_INTERVAL.as_secs_f64() * 1.1);
        assert_eq!(far.failure_count, 51);
    }

    #[test]
    fn a_failure_count_at_the_integer_ceiling_does_not_overflow() {
        let schedule = next(now(), hour(), i32::MAX, Outcome::Failure, None);
        assert_eq!(schedule.failure_count, i32::MAX);
        assert!(delay_secs(&schedule) <= MAX_INTERVAL.as_secs_f64() * 1.1);
    }

    #[test]
    fn rate_limiting_does_not_count_as_ill_health() {
        let schedule = next(
            now(),
            hour(),
            2,
            Outcome::RateLimited {
                retry_after: Some(Duration::from_secs(30 * 60)),
            },
            None,
        );

        // The point of the whole variant: a throttled feed must not accumulate failures
        // and eventually get disabled.
        assert_eq!(schedule.failure_count, 2);
        assert!(schedule.rate_limited_until.is_some());
        assert_eq!(schedule.rate_limited_until, Some(schedule.next_check_at));
    }

    #[test]
    fn retry_after_is_clamped_in_both_directions() {
        let too_soon = next(
            now(),
            hour(),
            0,
            Outcome::RateLimited {
                retry_after: Some(Duration::from_secs(1)),
            },
            None,
        );
        assert!(delay_secs(&too_soon) >= MIN_INTERVAL.as_secs_f64() * 0.9);

        let absurd = next(
            now(),
            hour(),
            0,
            Outcome::RateLimited {
                retry_after: Some(Duration::from_secs(365 * 24 * 3600)),
            },
            None,
        );
        assert!(delay_secs(&absurd) <= MAX_INTERVAL.as_secs_f64() * 1.1);
    }

    #[test]
    fn a_429_without_a_retry_after_falls_back_to_the_base_interval() {
        let schedule = next(
            now(),
            hour(),
            0,
            Outcome::RateLimited { retry_after: None },
            None,
        );
        assert!((3240.0..=3960.0).contains(&delay_secs(&schedule)));
    }

    #[test]
    fn cache_control_can_slow_us_down_but_never_speed_us_up() {
        let slower = next(
            now(),
            hour(),
            0,
            Outcome::Success,
            Some(Duration::from_secs(6 * 3600)),
        );
        assert!(
            delay_secs(&slower) > 5.0 * 3600.0,
            "max-age should be honoured"
        );

        let faster = next(
            now(),
            hour(),
            0,
            Outcome::Success,
            Some(Duration::from_secs(60)),
        );
        assert!(
            delay_secs(&faster) >= 3240.0,
            "a short max-age must not make us poll more often than configured"
        );
    }

    #[test]
    fn a_configured_interval_below_the_floor_is_raised_to_it() {
        let schedule = next(now(), Duration::from_secs(1), 0, Outcome::Success, None);
        assert!(delay_secs(&schedule) >= MIN_INTERVAL.as_secs_f64() * 0.9);
    }

    #[test]
    fn jitter_actually_spreads_the_schedule() {
        let delays: Vec<f64> = (0..20)
            .map(|_| delay_secs(&next(now(), hour(), 0, Outcome::Success, None)))
            .collect();

        let first = delays[0];
        assert!(
            delays.iter().any(|delay| (delay - first).abs() > 1.0),
            "twenty schedules should not all be identical: {delays:?}"
        );
        // And all within the ±10% band.
        assert!(delays.iter().all(|delay| (3240.0..=3960.0).contains(delay)));
    }

    #[test]
    fn retry_after_reads_both_seconds_and_http_dates() {
        assert_eq!(
            parse_retry_after("120", now()),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            parse_retry_after("  120  ", now()),
            Some(Duration::from_secs(120))
        );
        // `now()` is a Friday.
        assert_eq!(
            parse_retry_after("Fri, 02 Oct 2026 13:00:00 GMT", now()),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn a_date_whose_weekday_name_is_wrong_is_still_honoured() {
        // chrono rejects an inconsistent weekday, and servers do get it wrong. RFC 9110
        // says to ignore the weekday, so dropping the server's instruction over a typo
        // would be the wrong call.
        assert_eq!(
            parse_retry_after("Thu, 02 Oct 2026 13:00:00 GMT", now()),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn a_retry_after_date_in_the_past_means_now() {
        assert_eq!(
            parse_retry_after("Fri, 02 Oct 2026 11:00:00 GMT", now()),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn unparseable_retry_after_is_refused_rather_than_guessed() {
        assert_eq!(parse_retry_after("soon", now()), None);
        assert_eq!(parse_retry_after("", now()), None);
        assert_eq!(parse_retry_after("-5", now()), None);
        assert_eq!(parse_retry_after("1.5", now()), None);
    }
}
