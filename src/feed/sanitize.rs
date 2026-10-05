//! Cleaning entry content, once, at ingest.
//!
//! Sanitizing at ingest rather than at render matters twice over: a list page renders
//! fifty entries and must not pay for fifty HTML parses, and content that reached the
//! database unsanitized would be one template mistake away from executing.
//!
//! Two things come out of the same pass: the HTML we store and show, and a plain-text
//! version. The search vector is built from the plain text, because a `tsvector` over
//! markup matches on `div` and `href`.

use std::collections::HashSet;

use ammonia::{Builder, UrlRelative};
use url::Url;

/// Attributes forced onto every link and image we keep.
///
/// `noopener`/`noreferrer` because entry links open in a new tab and must not hand the
/// opener over; `nofollow` because a feed reader is not an endorsement. `lazy` loading and
/// `no-referrer` on images limit both bandwidth and what a publisher's image host learns.
fn policy(base: Option<&Url>) -> Builder<'static> {
    let mut builder = Builder::default();

    builder
        // Structure and semantics only. Scripts, styles, forms, iframes, objects and
        // event handlers are all absent from the default allowlist and stay absent.
        .rm_tags([
            "iframe", "object", "embed", "form", "input", "button", "style",
        ])
        .link_rel(Some("noopener noreferrer nofollow"))
        .set_tag_attribute_value("a", "target", "_blank")
        .set_tag_attribute_value("img", "loading", "lazy")
        .set_tag_attribute_value("img", "referrerpolicy", "no-referrer")
        // Tracking pixels are the common case for a 1x1 image with no alt; width/height
        // are not in the allowlist anyway, so they carry no layout information.
        .rm_tag_attributes("img", ["width", "height"])
        .url_schemes(HashSet::from_iter(["http", "https", "mailto"]));

    match base {
        // ammonia's default for relative URLs is Deny, which silently drops every
        // `<img src="/photo.png">` in a feed — a very visible failure for the reader and
        // a confusing one to diagnose.
        Some(base) => {
            builder.url_relative(UrlRelative::RewriteWithBase(base.clone()));
        }
        // With no base to resolve against, passing them through is better than dropping
        // them: a relative link may still work for someone reading on the origin site.
        None => {
            builder.url_relative(UrlRelative::PassThrough);
        }
    }

    builder
}

/// The sanitized HTML, ready to store and render.
pub fn html(raw: &str, base: Option<&Url>) -> String {
    policy(base).clean(raw).to_string()
}

/// The same content with all markup removed, for the search vector.
///
/// Runs over the *sanitized* HTML so that text inside a stripped `<script>` never reaches
/// the index. Block-level tags become spaces, so "a</p><p>b" does not index as "ab".
pub fn plain_text(sanitized_html: &str) -> String {
    let mut out = String::with_capacity(sanitized_html.len());
    let mut in_tag = false;

    for ch in sanitized_html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    // Entities survive the tag strip, so decode them, then collapse whitespace.
    let decoded = decode_entities(&out);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The five predefined XML entities plus `&nbsp;`, which is all ammonia's output contains.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        // Ampersand last, or `&amp;lt;` would decode twice into `<`.
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/posts/one").unwrap()
    }

    #[test]
    fn keeps_the_markup_an_article_is_made_of() {
        let raw = "<p>A <strong>bold</strong> claim, <em>probably</em>.</p>\
                   <ul><li>one</li></ul><blockquote>quoted</blockquote><pre><code>fn x</code></pre>";
        let clean = html(raw, Some(&base()));

        for expected in [
            "<p>",
            "<strong>",
            "<em>",
            "<ul>",
            "<li>",
            "<blockquote>",
            "<pre>",
            "<code>",
        ] {
            assert!(
                clean.contains(expected),
                "{expected} should survive: {clean}"
            );
        }
    }

    #[test]
    fn strips_scripts_styles_and_event_handlers() {
        let raw = r#"<p onclick="steal()">text</p>
            <script>alert(1)</script>
            <style>body{display:none}</style>
            <form action="/pay"><input name="card" /><button>Go</button></form>
            <iframe src="https://evil.example"></iframe>"#;

        let clean = html(raw, Some(&base()));

        assert!(clean.contains("text"));
        assert!(!clean.contains("onclick"));
        assert!(!clean.contains("alert"));
        assert!(!clean.contains("<script"));
        assert!(!clean.contains("<style"));
        assert!(!clean.contains("display:none"));
        assert!(!clean.contains("<form"));
        assert!(!clean.contains("<input"));
        assert!(!clean.contains("<iframe"));
    }

    #[test]
    fn relative_urls_are_rewritten_against_the_entry() {
        let clean = html(
            r#"<p><img src="/photo.png" alt="a"><a href="../other">there</a></p>"#,
            Some(&base()),
        );

        assert!(
            clean.contains("https://example.com/photo.png"),
            "a relative image must not be dropped: {clean}"
        );
        assert!(clean.contains("https://example.com/other"), "{clean}");
    }

    #[test]
    fn relative_urls_pass_through_when_there_is_no_base() {
        let clean = html(r#"<img src="/photo.png" alt="a">"#, None);
        assert!(clean.contains("/photo.png"), "{clean}");
    }

    #[test]
    fn links_open_safely_in_a_new_tab() {
        let clean = html(r#"<a href="https://a.example/x">there</a>"#, Some(&base()));

        assert!(clean.contains(r#"target="_blank""#), "{clean}");
        assert!(clean.contains("noopener"), "{clean}");
        assert!(clean.contains("noreferrer"), "{clean}");
        assert!(clean.contains("nofollow"), "{clean}");
    }

    #[test]
    fn images_are_lazy_and_do_not_leak_a_referrer() {
        let clean = html(
            r#"<img src="https://a.example/p.png" alt="a">"#,
            Some(&base()),
        );
        assert!(clean.contains(r#"loading="lazy""#), "{clean}");
        assert!(clean.contains(r#"referrerpolicy="no-referrer""#), "{clean}");
    }

    #[test]
    fn javascript_and_data_urls_are_removed() {
        let clean = html(
            r#"<a href="javascript:alert(1)">x</a><img src="data:image/svg+xml,<svg/onload=1>">"#,
            Some(&base()),
        );
        assert!(!clean.contains("javascript:"), "{clean}");
        assert!(!clean.contains("data:"), "{clean}");
        assert!(!clean.contains("onload"), "{clean}");
    }

    #[test]
    fn plain_text_drops_markup_and_collapses_whitespace() {
        let clean = html("<p>First   line.</p>\n<p>Second line.</p>", Some(&base()));
        assert_eq!(plain_text(&clean), "First line. Second line.");
    }

    #[test]
    fn adjacent_blocks_do_not_run_their_words_together() {
        // Without a space for the tag, this would index as "onetwo".
        let clean = html("<p>one</p><p>two</p>", Some(&base()));
        assert_eq!(plain_text(&clean), "one two");
    }

    #[test]
    fn plain_text_never_contains_tag_or_attribute_names() {
        let clean = html(
            r#"<div class="wrapper"><a href="https://a.example/x">link</a></div>"#,
            Some(&base()),
        );
        let text = plain_text(&clean);

        assert_eq!(text, "link");
        // The whole reason the search vector is built from this and not from the HTML.
        for noise in ["div", "href", "class", "http", "nofollow"] {
            assert!(!text.contains(noise), "{noise:?} leaked into {text:?}");
        }
    }

    #[test]
    fn entities_are_decoded_once_and_only_once() {
        assert_eq!(plain_text("<p>Tom &amp; Jerry</p>"), "Tom & Jerry");
        assert_eq!(plain_text("<p>a &lt; b</p>"), "a < b");
        assert_eq!(plain_text("<p>&amp;lt;</p>"), "&lt;", "no double decoding");
        assert_eq!(plain_text("<p>a&nbsp;b</p>"), "a b");
    }

    #[test]
    fn empty_and_text_only_content_are_handled() {
        assert_eq!(html("", Some(&base())), "");
        assert_eq!(plain_text(""), "");
        assert_eq!(plain_text(&html("just words", Some(&base()))), "just words");
    }
}
