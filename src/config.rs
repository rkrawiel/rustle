//! Configuration from the environment, with documented defaults.
//!
//! `DATABASE_URL` is deliberately unprefixed for sqlx compatibility; everything else uses
//! the `RUSTLE_` prefix. Parsing is written against a lookup closure rather than reading
//! the process environment directly, so the defaults and the error cases are testable
//! without mutating global state.

use std::net::SocketAddr;
use std::time::Duration;

use url::Url;

/// Anything that stops Rustle from starting up.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{name} is not a valid {expected}: {value:?}")]
    Invalid {
        name: &'static str,
        expected: &'static str,
        value: String,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub listen_addr: SocketAddr,
    pub base_url: Url,
    pub poll_interval: Duration,
    pub log_level: String,
    pub fetch_concurrency: usize,
    pub session_ttl: Duration,
    pub retention_days: u32,
    pub setup_token: Option<String>,
    pub cookie_secure: bool,
}

pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
pub const DEFAULT_BASE_URL: &str = "http://localhost:8080";
pub const DEFAULT_POLL_INTERVAL_MINUTES: u64 = 60;
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_FETCH_CONCURRENCY: usize = 8;
pub const DEFAULT_SESSION_TTL_DAYS: u64 = 30;
pub const DEFAULT_RETENTION_DAYS: u32 = 0;

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|name| std::env::var(name).ok())
    }

    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let var = |name: &'static str| get(name).filter(|v| !v.is_empty());

        let base_url: Url = parse(&var, "RUSTLE_BASE_URL", DEFAULT_BASE_URL, "URL")?;

        Ok(Self {
            database_url: var("DATABASE_URL").ok_or(ConfigError::Missing("DATABASE_URL"))?,
            listen_addr: parse(
                &var,
                "RUSTLE_LISTEN_ADDR",
                DEFAULT_LISTEN_ADDR,
                "socket address",
            )?,
            poll_interval: Duration::from_secs(
                60 * parse_at_least_one(
                    &var,
                    "RUSTLE_POLL_INTERVAL_MINUTES",
                    DEFAULT_POLL_INTERVAL_MINUTES,
                )?,
            ),
            log_level: var("RUSTLE_LOG_LEVEL").unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_owned()),
            fetch_concurrency: parse(
                &var,
                "RUSTLE_FETCH_CONCURRENCY",
                &DEFAULT_FETCH_CONCURRENCY.to_string(),
                "positive integer",
            )
            .map(|n: usize| n.max(1))?,
            session_ttl: Duration::from_secs(
                60 * 60
                    * 24
                    * parse_at_least_one(
                        &var,
                        "RUSTLE_SESSION_TTL_DAYS",
                        DEFAULT_SESSION_TTL_DAYS,
                    )?,
            ),
            retention_days: parse(
                &var,
                "RUSTLE_RETENTION_DAYS",
                &DEFAULT_RETENTION_DAYS.to_string(),
                "number of days",
            )?,
            setup_token: var("RUSTLE_SETUP_TOKEN"),
            // Browsers never send a Secure cookie over plain http, so a development
            // instance on http://localhost has to opt out or login silently fails.
            cookie_secure: match var("RUSTLE_COOKIE_SECURE") {
                Some(raw) => parse_bool("RUSTLE_COOKIE_SECURE", &raw)?,
                None => base_url.scheme() == "https",
            },
            base_url,
        })
    }

    /// The `User-Agent` sent to every feed, so publishers can identify and contact us.
    pub fn user_agent(&self) -> String {
        format!(
            "Rustle/{} (+{})",
            env!("CARGO_PKG_VERSION"),
            self.base_url.as_str().trim_end_matches('/')
        )
    }
}

type Var<'a> = dyn Fn(&'static str) -> Option<String> + 'a;

fn parse<T: std::str::FromStr>(
    var: &Var<'_>,
    name: &'static str,
    default: &str,
    expected: &'static str,
) -> Result<T, ConfigError> {
    let raw = var(name);
    let raw = raw.as_deref().unwrap_or(default);
    raw.parse().map_err(|_| ConfigError::Invalid {
        name,
        expected,
        value: raw.to_owned(),
    })
}

/// Durations expressed in whole units, where zero would mean "never" and is not wanted.
fn parse_at_least_one(var: &Var<'_>, name: &'static str, default: u64) -> Result<u64, ConfigError> {
    parse::<u64>(var, name, &default.to_string(), "positive integer").map(|n| n.max(1))
}

fn parse_bool(name: &'static str, raw: &str) -> Result<bool, ConfigError> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::Invalid {
            name,
            expected: "boolean",
            value: raw.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_from(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut vars: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        vars.entry("DATABASE_URL".to_owned())
            .or_insert_with(|| "postgres://rustle@localhost/rustle".to_owned());
        Config::from_lookup(&move |name| vars.get(name).cloned())
    }

    #[test]
    fn applies_documented_defaults() {
        let c = config_from(&[]).expect("defaults should be valid");
        assert_eq!(c.listen_addr.to_string(), DEFAULT_LISTEN_ADDR);
        assert_eq!(c.base_url.as_str(), "http://localhost:8080/");
        assert_eq!(c.poll_interval, Duration::from_secs(3600));
        assert_eq!(c.log_level, DEFAULT_LOG_LEVEL);
        assert_eq!(c.fetch_concurrency, DEFAULT_FETCH_CONCURRENCY);
        assert_eq!(c.session_ttl, Duration::from_secs(30 * 24 * 3600));
        assert_eq!(c.retention_days, DEFAULT_RETENTION_DAYS);
        assert_eq!(c.setup_token, None);
    }

    #[test]
    fn database_url_is_required() {
        let err = Config::from_lookup(&|_| None).unwrap_err();
        assert_eq!(err, ConfigError::Missing("DATABASE_URL"));
    }

    #[test]
    fn empty_values_fall_back_to_defaults() {
        let c = config_from(&[("RUSTLE_LOG_LEVEL", ""), ("RUSTLE_FETCH_CONCURRENCY", "")])
            .expect("empty is treated as unset");
        assert_eq!(c.log_level, DEFAULT_LOG_LEVEL);
        assert_eq!(c.fetch_concurrency, DEFAULT_FETCH_CONCURRENCY);
    }

    #[test]
    fn rejects_unparseable_values_and_names_the_variable() {
        let err = config_from(&[("RUSTLE_LISTEN_ADDR", "not-an-address")]).unwrap_err();
        assert_eq!(
            err,
            ConfigError::Invalid {
                name: "RUSTLE_LISTEN_ADDR",
                expected: "socket address",
                value: "not-an-address".to_owned(),
            }
        );
    }

    #[test]
    fn zero_intervals_are_clamped_up_so_the_poller_cannot_spin() {
        let c = config_from(&[
            ("RUSTLE_POLL_INTERVAL_MINUTES", "0"),
            ("RUSTLE_SESSION_TTL_DAYS", "0"),
        ])
        .expect("zero is clamped, not rejected");
        assert_eq!(c.poll_interval, Duration::from_secs(60));
        assert_eq!(c.session_ttl, Duration::from_secs(24 * 3600));
    }

    #[test]
    fn cookie_secure_follows_the_base_url_scheme() {
        let plain = config_from(&[("RUSTLE_BASE_URL", "http://localhost:8080")]).unwrap();
        let tls = config_from(&[("RUSTLE_BASE_URL", "https://rustle.example")]).unwrap();
        assert!(!plain.cookie_secure);
        assert!(tls.cookie_secure);
    }

    #[test]
    fn cookie_secure_can_be_overridden_for_a_proxy_that_terminates_tls() {
        let c = config_from(&[
            ("RUSTLE_BASE_URL", "http://localhost:8080"),
            ("RUSTLE_COOKIE_SECURE", "true"),
        ])
        .unwrap();
        assert!(c.cookie_secure);

        let err = config_from(&[("RUSTLE_COOKIE_SECURE", "maybe")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                name: "RUSTLE_COOKIE_SECURE",
                ..
            }
        ));
    }

    #[test]
    fn user_agent_identifies_the_instance_without_a_trailing_slash() {
        let c = config_from(&[("RUSTLE_BASE_URL", "https://rustle.example/")]).unwrap();
        assert_eq!(
            c.user_agent(),
            format!(
                "Rustle/{} (+https://rustle.example)",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}
