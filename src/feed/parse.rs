//! Turning a fetched document into entries.
//!
//! Feeds in the wild are not well behaved: dates are missing or absurd, GUIDs are absent
//! or rotate on every fetch, and content changes on every fetch for reasons that have
//! nothing to do with the article. Each of those has a specific defence here, because
//! getting any of them wrong shows up as duplicated entries or read state that will not
//! stay read.

use chrono::{DateTime, TimeDelta, Utc};
use feed_rs::model::Feed as ParsedFeed;
use sha2::{Digest, Sha256};
use url::Url;

use crate::feed::sanitize;

/// Dates outside this window are not a timezone bug, they are nonsense: feeds do emit
/// year 0001 and year 9999. Left unchecked they pin themselves to the top or the bottom
/// of every list, and `ORDER BY published_at DESC` looks permanently broken.
const EARLIEST_PLAUSIBLE: &str = "1990-01-01T00:00:00Z";
/// Clock skew allowance for a publisher slightly ahead of us. Anything further in the
/// future is treated as unknown rather than allowed to sit above everything else forever.
const FUTURE_TOLERANCE: TimeDelta = TimeDelta::days(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEntry {
    /// SHA-256 of whatever we could use to identify the entry. Stored rather than the raw
    /// value because feeds put very long URLs in `guid`.
    pub guid_hash: Vec<u8>,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    /// Sanitized HTML.
    pub content: String,
    /// The same content with markup removed, for the search vector.
    pub content_text: String,
    /// SHA-256 of the content, so a re-fetch that changed nothing writes nothing.
    pub content_hash: Vec<u8>,
    pub published_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ParsedChannel {
    /// The feed's own title, which replaces the placeholder taken from the URL.
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub entries: Vec<ParsedEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("that is not a feed we can read: {0}")]
    NotAFeed(String),
}

/// Parses a feed document.
///
/// `feed_url` is where the bytes came from, used both to resolve relative links and as the
/// base for sanitizing relative image sources.
pub fn parse(
    bytes: &[u8],
    feed_url: &Url,
    now: DateTime<Utc>,
) -> Result<ParsedChannel, ParseError> {
    let parsed = feed_rs::parser::Builder::new()
        .base_uri(Some(feed_url.as_str()))
        // feed-rs's own sanitizer is deliberately off: `sanitize.rs` needs to rewrite
        // relative URLs against the site and force rel/target/loading attributes.
        .sanitize_content(false)
        .id_generator(deterministic_id)
        .build()
        .parse(bytes)
        .map_err(|err| ParseError::NotAFeed(err.to_string()))?;

    let site_url = site_url(&parsed);
    // Relative URLs inside entry content resolve against the site, not the feed file,
    // when the feed tells us where the site is.
    let content_base = site_url
        .as_deref()
        .and_then(|url| Url::parse(url).ok())
        .unwrap_or_else(|| feed_url.clone());

    let entries = parsed
        .entries
        .iter()
        .map(|entry| convert(entry, &content_base, now))
        .collect();

    Ok(ParsedChannel {
        title: parsed
            .title
            .as_ref()
            .map(|text| text.content.trim().to_owned())
            .filter(|title| !title.is_empty()),
        site_url,
        entries,
    })
}

fn site_url(parsed: &ParsedFeed) -> Option<String> {
    parsed
        .links
        .iter()
        // `rel="alternate"` is the site; `rel="self"` is the feed itself, and storing that
        // as the site URL would send readers back to the XML.
        .find(|link| link.rel.as_deref() == Some("alternate"))
        .or_else(|| parsed.links.iter().find(|link| link.rel.is_none()))
        .map(|link| link.href.clone())
}

fn convert(entry: &feed_rs::model::Entry, content_base: &Url, now: DateTime<Utc>) -> ParsedEntry {
    let url = entry
        .links
        .first()
        .map(|link| link.href.clone())
        .filter(|href| !href.trim().is_empty());

    let raw_content = entry
        .content
        .as_ref()
        .and_then(|content| content.body.clone())
        // RSS without `content:encoded` has only a summary, which is better than nothing.
        .or_else(|| entry.summary.as_ref().map(|text| text.content.clone()))
        .unwrap_or_default();

    let content = sanitize::html(&raw_content, Some(content_base));
    let content_text = sanitize::plain_text(&content);

    let title = entry
        .title
        .as_ref()
        .map(|text| text.content.trim().to_owned())
        .filter(|title| !title.is_empty())
        // An untitled entry is normal in a link-blog feed; showing the URL beats "".
        .or_else(|| url.clone())
        .unwrap_or_else(|| "Untitled".to_owned());

    let published_at = clamp_date(entry.published.or(entry.updated), now);

    ParsedEntry {
        guid_hash: guid_hash(entry),
        title,
        url,
        author: entry
            .authors
            .first()
            .and_then(|person| person.name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        content_hash: Sha256::digest(content.as_bytes()).to_vec(),
        content,
        content_text,
        published_at,
    }
}

/// Replaces feed-rs's identifier fallback for entries that carry no `<guid>` or `<id>`.
///
/// This matters more than it looks. feed-rs's default generator ends in
/// `util::uuid_gen()` — a **random** UUID — when an entry has neither a link nor a
/// title-and-URI pair. A random identity changes on every fetch, so the entry would be
/// inserted again every time the feed is polled, forever. Hashing the title instead is
/// stable across fetches, which is the only property that matters here.
///
/// Entries that *do* declare an `id` or `guid` never reach this function: feed-rs uses
/// the declared value and only calls the generator to fill a gap.
fn deterministic_id(
    links: &[feed_rs::model::Link],
    title: &Option<feed_rs::model::Text>,
    uri: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();

    match links.first().filter(|link| !link.href.trim().is_empty()) {
        Some(link) => hasher.update(link.href.trim().as_bytes()),
        None => hasher.update(uri.unwrap_or_default().as_bytes()),
    }
    // A NUL separator so "ab" + "c" cannot collide with "a" + "bc".
    hasher.update([0]);
    if let Some(title) = title {
        hasher.update(title.content.trim().as_bytes());
    }

    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What identifies an entry across fetches, hashed.
///
/// `entry.id` is always populated — either by the feed itself or by `deterministic_id`
/// above — so this only has to hash it. The hash is stored rather than the raw value
/// because feeds put very long URLs in `guid`, and a fixed-width `bytea` keys better.
fn guid_hash(entry: &feed_rs::model::Entry) -> Vec<u8> {
    Sha256::digest(entry.id.trim().as_bytes()).to_vec()
}

/// Keeps a publication date inside the range a real article can have.
///
/// Note what this deliberately does *not* do: clamp to a rolling window like "the last
/// year". A feed's first fetch legitimately includes its whole archive, and squashing
/// those into the present would destroy the ordering it is meant to protect.
fn clamp_date(candidate: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
    let earliest: DateTime<Utc> = EARLIEST_PLAUSIBLE
        .parse()
        .expect("the compiled-in floor is a valid timestamp");

    match candidate {
        Some(at) if at >= earliest && at <= now + FUTURE_TOLERANCE => at,
        // Missing, ancient or post-dated: the moment we saw it is the best answer we have.
        _ => now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-02T12:00:00Z".parse().unwrap()
    }

    fn feed_url() -> Url {
        Url::parse("https://example.com/feed.xml").unwrap()
    }

    fn parse_fixture(name: &str) -> ParsedChannel {
        let path = format!("tests/fixtures/{name}");
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{path}: {err}"));
        parse(&bytes, &feed_url(), now()).unwrap_or_else(|err| panic!("{path}: {err}"))
    }

    #[test]
    fn reads_atom() {
        let channel = parse_fixture("atom.xml");

        assert_eq!(channel.title.as_deref(), Some("Atom Example"));
        assert_eq!(channel.site_url.as_deref(), Some("https://atom.example/"));
        assert_eq!(channel.entries.len(), 2);

        let first = &channel.entries[0];
        assert_eq!(first.title, "First Atom entry");
        assert_eq!(first.url.as_deref(), Some("https://atom.example/first"));
        assert_eq!(first.author.as_deref(), Some("Ada"));
        assert!(first.content.contains("<p>"));
        assert!(first.content_text.contains("body of the first"));
        assert_eq!(
            first.published_at,
            "2026-09-30T08:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn reads_rss_2() {
        let channel = parse_fixture("rss2.xml");

        assert_eq!(channel.title.as_deref(), Some("RSS 2.0 Example"));
        assert_eq!(channel.site_url.as_deref(), Some("https://rss2.example/"));
        assert_eq!(channel.entries.len(), 2);
        assert_eq!(channel.entries[0].title, "First RSS entry");
        // content:encoded wins over description when both are present.
        assert!(channel.entries[0].content_text.contains("full content"));
    }

    #[test]
    fn reads_rss_1_rdf() {
        let channel = parse_fixture("rss1.xml");

        assert_eq!(channel.title.as_deref(), Some("RSS 1.0 Example"));
        assert_eq!(channel.entries.len(), 1);
        assert_eq!(channel.entries[0].title, "An RDF item");
    }

    #[test]
    fn an_unparseable_date_becomes_the_fetch_time() {
        let channel = parse_fixture("malformed-dates.xml");

        // "not a date", year 9999 and year 0001 all land on `now`.
        assert!(
            channel
                .entries
                .iter()
                .all(|entry| entry.published_at == now()),
            "{:?}",
            channel
                .entries
                .iter()
                .map(|e| (e.title.clone(), e.published_at))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_genuinely_old_archive_entry_keeps_its_date() {
        // The reason the clamp is a plausibility floor and not a rolling window.
        let old = clamp_date(Some("1999-06-01T00:00:00Z".parse().unwrap()), now());
        assert_eq!(
            old,
            "1999-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn dates_outside_the_plausible_range_are_replaced() {
        assert_eq!(clamp_date(None, now()), now());
        assert_eq!(
            clamp_date(Some("0001-01-01T00:00:00Z".parse().unwrap()), now()),
            now()
        );
        assert_eq!(
            clamp_date(Some("9999-01-01T00:00:00Z".parse().unwrap()), now()),
            now()
        );
        // A publisher an hour ahead of us is fine; a year ahead is not.
        let slightly_ahead: DateTime<Utc> = "2026-10-02T13:00:00Z".parse().unwrap();
        assert_eq!(clamp_date(Some(slightly_ahead), now()), slightly_ahead);
        assert_eq!(
            clamp_date(Some("2027-10-02T12:00:00Z".parse().unwrap()), now()),
            now()
        );
    }

    #[test]
    fn entries_without_a_guid_fall_back_to_their_link() {
        let channel = parse_fixture("no-guid.xml");
        assert_eq!(channel.entries.len(), 2);

        // Two entries with no `<guid>` but different links must not collide.
        assert_ne!(channel.entries[0].guid_hash, channel.entries[1].guid_hash);

        // And identity is derived from the link, so it survives a re-fetch. This is the
        // property that matters: feed-rs's own fallback ends in a random UUID, which would
        // make every poll insert every entry again.
        let again = parse_fixture("no-guid.xml");
        assert_eq!(channel.entries[0].guid_hash, again.entries[0].guid_hash);
        assert_eq!(channel.entries[1].guid_hash, again.entries[1].guid_hash);
    }

    #[test]
    fn an_entry_with_neither_guid_nor_link_still_gets_a_stable_identity() {
        let channel = parse_fixture("no-guid-no-link.xml");
        let first = &channel.entries[0];

        assert_eq!(first.guid_hash.len(), 32);
        // Re-parsing the same bytes gives the same identity, which is what stops a
        // re-fetch from duplicating the entry.
        let again = parse_fixture("no-guid-no-link.xml");
        assert_eq!(first.guid_hash, again.entries[0].guid_hash);
    }

    #[test]
    fn identical_content_hashes_identically_so_a_refetch_writes_nothing() {
        let first = parse_fixture("atom.xml");
        let second = parse_fixture("atom.xml");

        assert_eq!(
            first.entries[0].content_hash,
            second.entries[0].content_hash
        );
        // And different content does not.
        assert_ne!(first.entries[0].content_hash, first.entries[1].content_hash);
    }

    #[test]
    fn content_is_sanitized_and_relative_urls_resolved_against_the_site() {
        let channel = parse_fixture("hostile.xml");
        let entry = &channel.entries[0];

        assert!(!entry.content.contains("<script"));
        assert!(!entry.content.contains("onerror"));
        assert!(!entry.content.contains("javascript:"));
        // Resolved against the feed's declared site, not the feed file's directory.
        assert!(
            entry.content.contains("https://hostile.example/photo.png"),
            "{}",
            entry.content
        );
        assert!(entry.content.contains(r#"target="_blank""#));
    }

    #[test]
    fn an_untitled_entry_falls_back_to_its_link() {
        let channel = parse_fixture("no-title.xml");
        assert_eq!(channel.entries[0].title, "https://notitle.example/first");
    }

    #[test]
    fn a_document_that_is_not_a_feed_is_refused() {
        let html = b"<!doctype html><html><body>Just a page</body></html>";
        assert!(matches!(
            parse(html, &feed_url(), now()),
            Err(ParseError::NotAFeed(_))
        ));
        assert!(parse(b"", &feed_url(), now()).is_err());
        assert!(parse(b"\xff\xfe not xml", &feed_url(), now()).is_err());
    }

    #[test]
    fn a_feed_with_no_entries_parses_to_an_empty_list() {
        let channel = parse_fixture("empty.xml");
        assert!(channel.entries.is_empty());
        assert_eq!(channel.title.as_deref(), Some("Nothing Here Yet"));
    }

    #[test]
    fn the_self_link_is_not_mistaken_for_the_site() {
        let channel = parse_fixture("atom.xml");
        // atom.xml declares both rel="self" (the feed) and rel="alternate" (the site).
        assert_eq!(channel.site_url.as_deref(), Some("https://atom.example/"));
    }
}
