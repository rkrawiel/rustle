//! Session cookie reading and writing.
//!
//! Hand-rolled rather than pulling in a cookie crate: Rustle sets exactly one cookie and
//! needs exactly one attribute set, so a dependency would be more code than this.

use axum::http::{HeaderMap, HeaderValue, header};

use crate::config::Config;
use crate::db::session::SessionToken;

pub const NAME: &str = "rustle_session";

/// Reads our cookie out of the `Cookie` header, ignoring any others present.
pub fn read(headers: &HeaderMap) -> Option<SessionToken> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == NAME)
        .map(|(_, value)| SessionToken::from_cookie(value))
}

/// The `Set-Cookie` value that establishes a session.
///
/// `Secure` comes from the configured base URL's scheme, because a browser will not send
/// a `Secure` cookie over plain http and a development instance on localhost would
/// otherwise appear to accept the password and then bounce straight back to the login
/// form. `SameSite=Lax` is the first layer of CSRF defence. No `Max-Age`: a session
/// cookie dies with the browser session, and the server-side row carries the real expiry.
pub fn set(config: &Config, token: &SessionToken) -> HeaderValue {
    let mut value = format!("{NAME}={}; Path=/; HttpOnly; SameSite=Lax", token.as_str());
    if config.cookie_secure {
        value.push_str("; Secure");
    }
    HeaderValue::from_str(&value).expect("a base64url token makes a valid header value")
}

/// The `Set-Cookie` value that clears the session cookie. Attributes must match the ones
/// used to set it, or the browser keeps the original.
pub fn clear(config: &Config) -> HeaderValue {
    let mut value = format!("{NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0").to_owned();
    if config.cookie_secure {
        value.push_str("; Secure");
    }
    HeaderValue::from_str(&value).expect("a fixed string makes a valid header value")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_for(base_url: &str) -> Config {
        let base_url = base_url.to_owned();
        Config::from_lookup(&move |name| match name {
            "DATABASE_URL" => Some("postgres://rustle@localhost/rustle".to_owned()),
            "RUSTLE_BASE_URL" => Some(base_url.clone()),
            _ => None,
        })
        .unwrap()
    }

    fn with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn reads_our_cookie_from_among_others() {
        let headers = with_cookie("other=1; rustle_session=abc123; another=2");
        assert_eq!(read(&headers).unwrap().as_str(), "abc123");
    }

    #[test]
    fn absent_or_unrelated_cookies_yield_nothing() {
        assert!(read(&HeaderMap::new()).is_none());
        assert!(read(&with_cookie("other=1")).is_none());
        // A prefix match must not count.
        assert!(read(&with_cookie("rustle_session_x=abc")).is_none());
    }

    #[test]
    fn secure_follows_the_base_url_scheme() {
        let token = SessionToken::from_cookie("abc");

        let plain = set(&config_for("http://localhost:8080"), &token);
        assert!(!plain.to_str().unwrap().contains("Secure"));

        let tls = set(&config_for("https://rustle.example"), &token);
        assert!(tls.to_str().unwrap().contains("; Secure"));
    }

    #[test]
    fn the_cookie_is_http_only_and_same_site_lax() {
        let value = set(
            &config_for("https://rustle.example"),
            &SessionToken::from_cookie("abc"),
        );
        let value = value.to_str().unwrap();
        assert!(value.starts_with("rustle_session=abc; Path=/"));
        assert!(value.contains("HttpOnly"));
        assert!(value.contains("SameSite=Lax"));
    }

    #[test]
    fn clearing_repeats_the_attributes_so_the_browser_actually_drops_it() {
        let config = config_for("https://rustle.example");
        let cleared = clear(&config);
        let cleared = cleared.to_str().unwrap();
        assert!(cleared.contains("Max-Age=0"));
        assert!(cleared.contains("Path=/"));
        assert!(cleared.contains("HttpOnly"));
        assert!(cleared.contains("SameSite=Lax"));
        assert!(cleared.contains("Secure"));
    }
}
