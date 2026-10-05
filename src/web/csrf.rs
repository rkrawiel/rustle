//! CSRF protection, layered.
//!
//! Three defences, cheapest first:
//!
//! 1. `SameSite=Lax` on the session cookie, which already stops cross-site form POSTs in
//!    every current browser.
//! 2. A `Sec-Fetch-Site` check, falling back to `Origin` equality for the handful of
//!    clients that do not send fetch metadata. Ten lines, no template plumbing.
//! 3. A synchroniser token stored in the session row. Rustle already keeps server-side
//!    sessions, which is exactly what double-submit cookies exist to work around, so the
//!    token needs no signing key, no rotation and no integrity argument.
//!
//! Validation lives in the `CsrfForm` extractor rather than middleware: middleware would
//! have to buffer and rebuild the request body, which fights axum's model.

use axum::http::{HeaderMap, header};
use subtle::ConstantTimeEq;
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The request came from another site, per fetch metadata or `Origin`.
    CrossSite,
    TokenMissing,
    TokenMismatch,
}

/// Compares in constant time, so a mismatch leaks nothing about how much matched.
pub fn tokens_match(submitted: &str, expected: &str) -> bool {
    submitted.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Whether the request plausibly originated from this instance.
///
/// `Sec-Fetch-Site: same-origin` is conclusive. Without that header we fall back to
/// comparing `Origin` against the configured base URL. A request with neither header is
/// allowed through to the token check: curl sends neither, and the token is the real
/// defence — this layer only catches browsers that would send one.
pub fn same_site(headers: &HeaderMap, base_url: &Url) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        // "none" is a user-typed address or a bookmark, which cannot be an attack.
        return site == "same-origin" || site == "none";
    }

    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) => Url::parse(origin).is_ok_and(|origin| origin.origin() == base_url.origin()),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn base() -> Url {
        Url::parse("https://rustle.example").unwrap()
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn matching_tokens_are_accepted_and_anything_else_is_not() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "abcd"));
        assert!(!tokens_match("", "abc"));
    }

    #[test]
    fn fetch_metadata_decides_when_the_browser_sends_it() {
        assert!(same_site(
            &headers(&[("sec-fetch-site", "same-origin")]),
            &base()
        ));
        // A typed address or bookmark: no initiator, so not an attack.
        assert!(same_site(&headers(&[("sec-fetch-site", "none")]), &base()));
        assert!(!same_site(
            &headers(&[("sec-fetch-site", "cross-site")]),
            &base()
        ));
        assert!(!same_site(
            &headers(&[("sec-fetch-site", "same-site")]),
            &base()
        ));
    }

    #[test]
    fn fetch_metadata_wins_over_a_spoofable_origin() {
        let both = headers(&[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://rustle.example"),
        ]);
        assert!(!same_site(&both, &base()));
    }

    #[test]
    fn origin_is_the_fallback_when_there_is_no_fetch_metadata() {
        assert!(same_site(
            &headers(&[("origin", "https://rustle.example")]),
            &base()
        ));
        assert!(!same_site(
            &headers(&[("origin", "https://evil.example")]),
            &base()
        ));
        // Same host, different scheme or port is a different origin.
        assert!(!same_site(
            &headers(&[("origin", "http://rustle.example")]),
            &base()
        ));
        assert!(!same_site(&headers(&[("origin", "null")]), &base()));
    }

    #[test]
    fn a_request_with_neither_header_falls_through_to_the_token_check() {
        assert!(same_site(&HeaderMap::new(), &base()));
    }
}
