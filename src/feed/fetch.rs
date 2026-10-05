//! Fetching a feed over HTTP.
//!
//! Three hazards shape this module, and none of them is hypothetical for a self-hosted
//! reader that fetches addresses its owner pasted in:
//!
//! * **Unbounded bodies.** `Response::bytes()` buffers whatever arrives, `Content-Length`
//!   is only a hint, and gzip means the decompressed size is unbounded regardless. One
//!   oversized feed would exhaust a process with a 30 MB budget, so the body is streamed
//!   and abandoned past a cap.
//! * **SSRF.** A feed URL, autodiscovery and redirects together let someone aim Rustle at
//!   `127.0.0.1` or at a cloud metadata endpoint. Every hop is checked, not just the first.
//! * **Conditional GET.** Most polls should transfer nothing at all, which is both polite
//!   and the difference between a feed reader and a crawler.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::{Client, StatusCode, redirect};
use url::{Host, Url};

use crate::config::Config;

/// Abandon a body past this, measured **after** decompression.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;

/// What a poll produced.
#[derive(Debug)]
pub enum Fetched {
    /// A body to parse, with any new validators to store.
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
        /// The URL the body actually came from, after redirects. Relative links resolve
        /// against this, not against what we asked for.
        final_url: Url,
        /// `Cache-Control: max-age`, honoured only as a lower bound on the interval.
        max_age: Option<Duration>,
    },
    /// 304: nothing changed. A success, and the cheapest possible poll.
    NotModified,
    /// The server asked us to wait. Carries `Retry-After` when it gave one.
    RateLimited { retry_after: Option<Duration> },
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{0}")]
    Blocked(String),
    #[error("HTTP {0}")]
    Status(StatusCode),
    #[error("the feed is larger than {}MB", MAX_BODY_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("{0}")]
    Transport(String),
}

/// Which addresses may be fetched.
///
/// This exists because the SSRF guard and the test suite want opposite things: the guard's
/// whole job is to refuse loopback, and a `wiremock` server lives on `127.0.0.1`. Making
/// the choice an explicit type keeps the production path unambiguous — `Fetcher::new`
/// always picks `PublicOnly`, and no configuration value can change that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressPolicy {
    /// Refuse anything that is not a public internet address.
    PublicOnly,
    /// Allow loopback as well. **Tests only**; deliberately not reachable from `Config`.
    AllowLoopback,
}

impl AddressPolicy {
    fn check(self, url: &Url) -> Result<(), &'static str> {
        match self {
            Self::PublicOnly => is_public(url),
            Self::AllowLoopback => {
                if matches!(url.scheme(), "http" | "https") {
                    Ok(())
                } else {
                    Err("only http and https are fetched")
                }
            }
        }
    }
}

/// The one HTTP client the whole process shares, plus its address policy.
///
/// Sharing one client is the point: polling two hundred feeds through fresh TLS handshakes
/// every hour is slower and more expensive for everyone than reusing pooled connections.
#[derive(Clone)]
pub struct Fetcher {
    client: Client,
    policy: AddressPolicy,
}

impl Fetcher {
    pub fn new(config: &Config) -> Result<Self, reqwest::Error> {
        Self::build(&config.user_agent(), AddressPolicy::PublicOnly)
    }

    /// A fetcher that will talk to a local mock server. Tests only.
    pub fn allowing_loopback(user_agent: &str) -> Result<Self, reqwest::Error> {
        Self::build(user_agent, AddressPolicy::AllowLoopback)
    }

    fn build(user_agent: &str, policy: AddressPolicy) -> Result<Self, reqwest::Error> {
        install_crypto_provider();

        let client = Client::builder()
            .user_agent(user_agent.to_owned())
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .gzip(true)
            .brotli(true)
            // Checked on *every* hop. A redirect to a private address is the obvious way
            // around a check that only looks at the URL the reader typed.
            .redirect(redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                match policy.check(attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(reason) => attempt.error(reason),
                }
            }))
            .build()?;

        Ok(Self { client, policy })
    }
}

/// Installs ring as the process-wide rustls provider, once.
///
/// reqwest's `rustls-no-provider` feature is what keeps `aws-lc-rs` — and its cmake and
/// NASM build requirements — out of the dependency graph. The cost is that reqwest
/// **panics** when building a client if no provider has been installed, so this has to run
/// before the first `Client::builder()`. Doing it here rather than in `main` means the
/// test suite cannot forget to.
fn install_crypto_provider() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        // An error means something else installed one first, which is equally fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Fetches a feed, sending whatever validators we hold for it.
pub async fn fetch(
    fetcher: &Fetcher,
    url: &Url,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<Fetched, FetchError> {
    fetcher
        .policy
        .check(url)
        .map_err(|reason| FetchError::Blocked(reason.to_owned()))?;

    let mut request = fetcher.client.get(url.clone());
    // Both validators, because servers honour one or the other and rarely both.
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if let Some(last_modified) = last_modified {
        request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
    }

    let response = request
        .send()
        .await
        .map_err(|err| FetchError::Transport(describe(&err)))?;

    let status = response.status();

    if status == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }

    // 503 with a Retry-After is a server asking for patience, not a broken feed; without
    // one it is just an error and gets normal backoff.
    let retry_after = header(&response, reqwest::header::RETRY_AFTER);
    if status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::SERVICE_UNAVAILABLE && retry_after.is_some())
    {
        return Ok(Fetched::RateLimited {
            retry_after: retry_after
                .as_deref()
                .and_then(|value| super::backoff::parse_retry_after(value, chrono::Utc::now())),
        });
    }

    if !status.is_success() {
        return Err(FetchError::Status(status));
    }

    let etag = header(&response, reqwest::header::ETAG);
    let last_modified = header(&response, reqwest::header::LAST_MODIFIED);
    let max_age = header(&response, reqwest::header::CACHE_CONTROL)
        .as_deref()
        .and_then(parse_max_age);
    let final_url = Url::parse(response.url().as_str()).unwrap_or_else(|_| url.clone());

    let bytes = read_capped(response).await?;

    Ok(Fetched::Body {
        bytes,
        etag,
        last_modified,
        final_url,
        max_age,
    })
}

/// Reads a body chunk by chunk, abandoning it past the cap.
///
/// Deliberately not `response.bytes()`: that allocates for the whole body before we can
/// object to its size.
async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>, FetchError> {
    // `Content-Length` is advisory, so it is a cheap early exit rather than the check.
    if response
        .content_length()
        .is_some_and(|len| len > MAX_BODY_BYTES as u64)
    {
        return Err(FetchError::TooLarge);
    }

    let mut response = response;
    let mut body = Vec::with_capacity(16 * 1024);

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| FetchError::Transport(describe(&err)))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

/// Rejects anything that is not a public internet address.
///
/// Hostnames are not resolved here — a DNS lookup would be a second round trip, and the
/// name could resolve differently by the time the request goes out. What this catches is
/// the realistic attack: a literal address or a redirect to one. For hostnames, the
/// redirect policy re-checks each hop, and an attacker who controls DNS for a name the
/// reader typed in already has easier options.
fn is_public(url: &Url) -> Result<(), &'static str> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https are fetched");
    }

    match url.host() {
        Some(Host::Ipv4(ip)) => check_ip(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => check_ip(IpAddr::V6(ip)),
        Some(Host::Domain(name)) => {
            // `localhost` and friends resolve to loopback everywhere, so the literal name
            // is worth refusing even without resolving it.
            let name = name.to_ascii_lowercase();
            if name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local") {
                Err("that address is on this machine")
            } else {
                Ok(())
            }
        }
        None => Err("that address has no host"),
    }
}

fn check_ip(ip: IpAddr) -> Result<(), &'static str> {
    let blocked = match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                // 169.254.0.0/16, which is where cloud metadata services live.
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                // 100.64.0.0/10, carrier-grade NAT.
                || matches!(v4.octets(), [100, b, ..] if (64..128).contains(&b))
                // 192.0.0.0/24 and the 198.18.0.0/15 benchmarking range.
                || matches!(v4.octets(), [192, 0, 0, _])
                || matches!(v4.octets(), [198, b, ..] if b == 18 || b == 19)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique-local and fe80::/10 link-local.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // An IPv4-mapped address is the obvious way around an IPv4-only check.
                || v6.to_ipv4_mapped().is_some_and(|v4| check_ip(IpAddr::V4(v4)).is_err())
        }
    };

    if blocked {
        Err("that address is not on the public internet")
    } else {
        Ok(())
    }
}

fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)?
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// Pulls `max-age` out of a `Cache-Control` value, ignoring the rest of it.
fn parse_max_age(value: &str) -> Option<Duration> {
    value
        .split(',')
        .filter_map(|directive| directive.trim().split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("max-age"))
        .and_then(|(_, seconds)| seconds.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// reqwest's `Display` is terse about causes, and "error sending request" with no reason
/// is useless in a feed's status column.
fn describe(err: &reqwest::Error) -> String {
    let mut message = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(url: &str) -> bool {
        is_public(&Url::parse(url).unwrap()).is_err()
    }

    #[test]
    fn public_addresses_are_allowed() {
        assert!(!blocked("https://example.com/feed.xml"));
        assert!(!blocked("http://example.com/feed.xml"));
        assert!(!blocked("https://1.1.1.1/feed"));
        assert!(!blocked("https://[2606:4700:4700::1111]/feed"));
    }

    #[test]
    fn loopback_and_private_ranges_are_refused() {
        for url in [
            "http://127.0.0.1/feed",
            "http://127.1.2.3/feed",
            "http://10.0.0.5/feed",
            "http://192.168.1.1/feed",
            "http://172.16.0.1/feed",
            "http://0.0.0.0/feed",
            "http://[::1]/feed",
            "http://[fd00::1]/feed",
            "http://[fe80::1]/feed",
        ] {
            assert!(blocked(url), "{url} should be refused");
        }
    }

    #[test]
    fn the_cloud_metadata_endpoint_is_refused() {
        // The single most valuable SSRF target on a hosted machine.
        assert!(blocked("http://169.254.169.254/latest/meta-data/"));
        assert!(blocked("http://169.254.0.1/"));
    }

    #[test]
    fn an_ipv4_mapped_ipv6_address_cannot_smuggle_a_private_target() {
        assert!(blocked("http://[::ffff:127.0.0.1]/feed"));
        assert!(blocked("http://[::ffff:10.0.0.1]/feed"));
        assert!(!blocked("http://[::ffff:1.1.1.1]/feed"));
    }

    #[test]
    fn carrier_nat_and_reserved_ranges_are_refused() {
        assert!(blocked("http://100.64.0.1/feed"));
        assert!(blocked("http://100.127.255.1/feed"));
        assert!(blocked("http://192.0.0.1/feed"));
        assert!(blocked("http://198.18.0.1/feed"));
        // Just outside carrier-grade NAT.
        assert!(!blocked("http://100.128.0.1/feed"));
        assert!(!blocked("http://100.63.255.1/feed"));
    }

    #[test]
    fn names_that_always_mean_this_machine_are_refused() {
        assert!(blocked("http://localhost/feed"));
        assert!(blocked("http://LOCALHOST/feed"));
        assert!(blocked("http://api.localhost/feed"));
        assert!(blocked("http://printer.local/feed"));
    }

    #[test]
    fn non_http_schemes_are_refused() {
        assert!(blocked("file:///etc/passwd"));
        assert!(blocked("ftp://example.com/feed"));
        assert!(blocked("gopher://example.com/feed"));
    }

    #[test]
    fn max_age_is_read_out_of_cache_control() {
        assert_eq!(parse_max_age("max-age=600"), Some(Duration::from_secs(600)));
        assert_eq!(
            parse_max_age("public, max-age=600, must-revalidate"),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            parse_max_age("Max-Age = 600"),
            Some(Duration::from_secs(600))
        );
    }

    #[test]
    fn cache_control_without_a_usable_max_age_yields_nothing() {
        assert_eq!(parse_max_age("no-cache"), None);
        assert_eq!(parse_max_age("max-age=soon"), None);
        assert_eq!(parse_max_age("max-age="), None);
        assert_eq!(parse_max_age(""), None);
        // s-maxage is for shared caches, not for us.
        assert_eq!(parse_max_age("s-maxage=600"), None);
    }
}
