//! Feed autodiscovery from an HTML page.
//!
//! People paste the address of a site, not of its feed, so `<link rel="alternate">` has to
//! be followed. Built on `html5ever`'s tokenizer, which `ammonia` already brings in, so
//! this costs no extra binary size — a dedicated HTML-parsing crate would have.
//!
//! A tokenizer rather than a full DOM: we need a handful of `<link>` attributes from the
//! head, so building a tree would be wasted work on a page that may be megabytes long.

use std::cell::RefCell;

use html5ever::tokenizer::{
    BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use url::Url;

/// Feed types a `<link rel="alternate">` may advertise, best first. A page that offers
/// both Atom and RSS usually offers the same entries in both.
const FEED_TYPES: [&str; 5] = [
    "application/atom+xml",
    "application/rss+xml",
    "application/feed+json",
    "application/json",
    "application/rdf+xml",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub url: Url,
    pub title: Option<String>,
}

/// Every feed the page advertises, ordered by how much we would rather have it.
///
/// `base` is the URL the HTML came from, used to resolve relative hrefs. Returns an empty
/// vector for a page that advertises nothing, which the caller reports as "no feed found"
/// rather than as an error.
pub fn find_feeds(html: &str, base: &Url) -> Vec<Discovered> {
    let sink = LinkSink::default();
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let queue = BufferQueue::default();
    queue.push_back(html.into());
    let _ = tokenizer.feed(&queue);
    tokenizer.end();

    let mut found: Vec<(usize, Discovered)> = tokenizer
        .sink
        .links
        .borrow()
        .iter()
        .filter_map(|link| {
            let rank = FEED_TYPES
                .iter()
                .position(|candidate| *candidate == link.mime)?;
            let url = base.join(&link.href).ok()?;
            // A feed served over anything but http(s) is not something we can fetch.
            if !matches!(url.scheme(), "http" | "https") {
                return None;
            }
            Some((
                rank,
                Discovered {
                    url,
                    title: link.title.clone().filter(|title| !title.is_empty()),
                },
            ))
        })
        .collect();

    found.sort_by_key(|(rank, _)| *rank);

    let mut seen = Vec::new();
    found
        .into_iter()
        .map(|(_, discovered)| discovered)
        .filter(|discovered| {
            // The same feed is often listed under two MIME types.
            let is_new = !seen.contains(&discovered.url);
            if is_new {
                seen.push(discovered.url.clone());
            }
            is_new
        })
        .collect()
}

#[derive(Debug)]
struct Link {
    mime: String,
    href: String,
    title: Option<String>,
}

#[derive(Default)]
struct LinkSink {
    // RefCell because html5ever hands the sink out by shared reference.
    links: RefCell<Vec<Link>>,
}

impl TokenSink for LinkSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let Token::TagToken(tag) = token else {
            return TokenSinkResult::Continue;
        };
        if &*tag.name != "link" {
            return TokenSinkResult::Continue;
        }

        let get = |wanted: &str| {
            tag.attrs
                .iter()
                // `&*` rather than `as_ref`: html5ever's atoms implement AsRef for both
                // `str` and `[u8]`, so `as_ref` here is ambiguous.
                .find(|attr| &*attr.name.local == wanted)
                .map(|attr| attr.value.trim().to_string())
        };

        // `rel` may list several values, as in `rel="alternate nofollow"`.
        let is_alternate = get("rel").is_some_and(|rel| {
            rel.split_whitespace()
                .any(|value| value.eq_ignore_ascii_case("alternate"))
        });
        if !is_alternate {
            return TokenSinkResult::Continue;
        }

        let Some(href) = get("href").filter(|href| !href.is_empty()) else {
            return TokenSinkResult::Continue;
        };
        let Some(mime) = get("type") else {
            return TokenSinkResult::Continue;
        };

        self.links.borrow_mut().push(Link {
            // `type="application/rss+xml; charset=utf-8"` is legal.
            mime: mime
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_lowercase(),
            href,
            title: get("title"),
        });

        TokenSinkResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/blog/").unwrap()
    }

    fn urls(html: &str) -> Vec<String> {
        find_feeds(html, &base())
            .into_iter()
            .map(|found| found.url.to_string())
            .collect()
    }

    #[test]
    fn finds_an_atom_link_in_the_head() {
        let html = r#"<html><head>
            <link rel="alternate" type="application/atom+xml" href="/feed.xml" title="Atom" />
            </head><body>no</body></html>"#;

        let found = find_feeds(html, &base());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url.as_str(), "https://example.com/feed.xml");
        assert_eq!(found[0].title.as_deref(), Some("Atom"));
    }

    #[test]
    fn resolves_relative_hrefs_against_the_page_url() {
        assert_eq!(
            urls(r#"<link rel="alternate" type="application/rss+xml" href="feed.xml">"#),
            ["https://example.com/blog/feed.xml"]
        );
        assert_eq!(
            urls(r#"<link rel="alternate" type="application/rss+xml" href="//cdn.example/f">"#),
            ["https://cdn.example/f"]
        );
        assert_eq!(
            urls(r#"<link rel="alternate" type="application/rss+xml" href="https://a.example/f">"#),
            ["https://a.example/f"]
        );
    }

    #[test]
    fn prefers_atom_when_a_page_offers_several() {
        let html = r#"<head>
            <link rel="alternate" type="application/rss+xml" href="/rss" />
            <link rel="alternate" type="application/atom+xml" href="/atom" />
            </head>"#;

        assert_eq!(
            urls(html),
            ["https://example.com/atom", "https://example.com/rss"]
        );
    }

    #[test]
    fn the_same_feed_listed_twice_is_returned_once() {
        let html = r#"<head>
            <link rel="alternate" type="application/rss+xml" href="/feed" />
            <link rel="alternate" type="application/atom+xml" href="/feed" />
            </head>"#;

        assert_eq!(urls(html), ["https://example.com/feed"]);
    }

    #[test]
    fn a_rel_list_containing_alternate_still_counts() {
        let html = r#"<link rel="Alternate nofollow" type="application/rss+xml" href="/f">"#;
        assert_eq!(urls(html), ["https://example.com/f"]);
    }

    #[test]
    fn a_content_type_with_parameters_is_still_recognised() {
        let html = r#"<link rel="alternate" type="application/RSS+XML; charset=utf-8" href="/f">"#;
        assert_eq!(urls(html), ["https://example.com/f"]);
    }

    #[test]
    fn ignores_links_that_are_not_feeds() {
        // The classic false positive: a translation or a stylesheet.
        assert!(urls(r#"<link rel="alternate" hreflang="fr" href="/fr/">"#).is_empty());
        assert!(urls(r#"<link rel="alternate" type="text/html" href="/amp">"#).is_empty());
        assert!(urls(r#"<link rel="stylesheet" type="text/css" href="/a.css">"#).is_empty());
        assert!(urls(r#"<link rel="icon" href="/favicon.ico">"#).is_empty());
    }

    #[test]
    fn ignores_a_feed_we_could_not_fetch() {
        let html = r#"<link rel="alternate" type="application/rss+xml" href="feed:///x">"#;
        assert!(urls(html).is_empty());
    }

    #[test]
    fn a_page_with_no_feed_yields_nothing_rather_than_failing() {
        assert!(urls("<html><body><p>Just a page.</p></body></html>").is_empty());
        assert!(urls("").is_empty());
        // Malformed markup must not panic the tokenizer.
        assert!(urls("<html><head><link rel=<<>> type=").is_empty());
    }

    #[test]
    fn an_empty_href_or_missing_type_is_skipped() {
        assert!(urls(r#"<link rel="alternate" type="application/rss+xml" href="">"#).is_empty());
        assert!(urls(r#"<link rel="alternate" href="/feed">"#).is_empty());
    }
}
