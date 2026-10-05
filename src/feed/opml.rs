//! OPML import and export.
//!
//! Hand-rolled over `quick-xml` rather than using an OPML crate, because real-world OPML
//! is looser than the spec: exports in the wild nest outlines to arbitrary depth, put the
//! feed URL in `xmlUrl` or `xmlurl`, and label the feed with `title`, `text`, or neither.
//! Import is therefore deliberately forgiving — any outline carrying an `xmlUrl` is a
//! feed, and its category is the title of the nearest enclosing outline that is not one.

use quick_xml::XmlVersion;
use quick_xml::escape::escape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpmlFeed {
    pub title: String,
    pub feed_url: String,
    pub site_url: Option<String>,
    /// `None` means the file did not group this feed, so the importer uses the default.
    pub category: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OpmlError {
    #[error("that does not look like an OPML file: {0}")]
    Malformed(String),
    #[error("no feeds found in that file")]
    Empty,
}

/// Every feed in the document, in file order.
pub fn parse(xml: &str) -> Result<Vec<OpmlFeed>, OpmlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    // Real exports are not always well-formed about closing tags, and refusing the whole
    // file over one of those would be worse than importing what we can read.
    reader.config_mut().check_end_names = false;

    let mut feeds = Vec::new();
    // Titles of the enclosing outlines that are not themselves feeds. `None` is a group
    // with no usable name, which still has to occupy a level so `pop` stays aligned.
    let mut groups: Vec<Option<String>> = Vec::new();
    let mut buf = Vec::new();

    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|err| OpmlError::Malformed(err.to_string()))?;

        match event {
            Event::Eof => break,

            // A container outline: either a feed with children, or a category.
            Event::Start(ref tag) if is_outline(tag) => {
                match read_feed(tag, &groups) {
                    Some(feed) => {
                        feeds.push(feed);
                        // It is a feed, but it still opened an element, so it occupies a
                        // level. Unnamed, so it never becomes anyone's category.
                        groups.push(None);
                    }
                    None => groups.push(label(tag)),
                }
            }

            // Self-closing `<outline ... />`. No children and no matching `End`, so it
            // must not touch the group stack.
            Event::Empty(ref tag) if is_outline(tag) => {
                if let Some(feed) = read_feed(tag, &groups) {
                    feeds.push(feed);
                }
            }

            Event::End(ref tag) if tag.local_name().as_ref() == "outline" => {
                groups.pop();
            }

            _ => {}
        }
        buf.clear();
    }

    if feeds.is_empty() {
        return Err(OpmlError::Empty);
    }
    Ok(feeds)
}

/// Writes an OPML 2.0 document, grouping feeds under one outline per category.
///
/// Input must already be sorted by category, which is what `db::feed::list_summaries`
/// returns, so grouping is a single pass with no intermediate map.
pub fn write(title: &str, feeds: &[OpmlFeed]) -> String {
    let mut out = String::with_capacity(512 + feeds.len() * 160);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<opml version=\"2.0\">\n  <head>\n    <title>");
    out.push_str(&escape(title));
    out.push_str("</title>\n  </head>\n  <body>\n");

    let mut current: Option<&str> = None;
    for feed in feeds {
        let category = feed.category.as_deref().unwrap_or("");

        if current != Some(category) {
            if current.is_some() {
                out.push_str("    </outline>\n");
            }
            out.push_str("    <outline text=\"");
            out.push_str(&escape(category));
            out.push_str("\">\n");
            current = Some(category);
        }

        out.push_str("      <outline type=\"rss\" text=\"");
        out.push_str(&escape(&feed.title));
        out.push_str("\" title=\"");
        out.push_str(&escape(&feed.title));
        out.push_str("\" xmlUrl=\"");
        out.push_str(&escape(&feed.feed_url));
        out.push('"');
        if let Some(site_url) = &feed.site_url {
            out.push_str(" htmlUrl=\"");
            out.push_str(&escape(site_url));
            out.push('"');
        }
        out.push_str(" />\n");
    }

    if current.is_some() {
        out.push_str("    </outline>\n");
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

fn is_outline(tag: &BytesStart<'_>) -> bool {
    tag.local_name().as_ref() == "outline"
}

/// An outline is a feed if and only if it carries a non-empty `xmlUrl`.
fn read_feed(tag: &BytesStart<'_>, groups: &[Option<String>]) -> Option<OpmlFeed> {
    let attrs = attributes(tag);
    let feed_url = attr(&attrs, "xmlurl")?;

    Some(OpmlFeed {
        title: label(tag).unwrap_or_else(|| feed_url.clone()),
        feed_url,
        site_url: attr(&attrs, "htmlurl"),
        category: groups.iter().rev().flatten().next().cloned(),
    })
}

/// The human-readable name, from whichever attribute the exporter chose to use.
fn label(tag: &BytesStart<'_>) -> Option<String> {
    let attrs = attributes(tag);
    attr(&attrs, "title").or_else(|| attr(&attrs, "text"))
}

fn attr(attrs: &[(String, String)], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .filter(|value| !value.is_empty())
}

/// Attribute names lowercased and values trimmed, because `xmlUrl`, `xmlurl` and `XMLURL`
/// all occur in the wild, as do URLs padded with newlines.
fn attributes(tag: &BytesStart<'_>) -> Vec<(String, String)> {
    tag.attributes()
        .filter_map(Result::ok)
        .filter_map(|attr| {
            let name = attr.key.local_name().as_ref().to_lowercase();
            let value = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .ok()?
                .trim()
                .to_owned();
            Some((name, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(title: &str, url: &str, category: Option<&str>) -> OpmlFeed {
        OpmlFeed {
            title: title.to_owned(),
            feed_url: url.to_owned(),
            site_url: None,
            category: category.map(str::to_owned),
        }
    }

    #[test]
    fn reads_a_flat_document() {
        let xml = r#"<?xml version="1.0"?>
            <opml version="1.0"><head><title>Subs</title></head><body>
              <outline type="rss" text="Alpha" xmlUrl="https://a.example/feed" />
              <outline type="rss" text="Beta" xmlUrl="https://b.example/feed" />
            </body></opml>"#;

        let feeds = parse(xml).unwrap();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0], feed("Alpha", "https://a.example/feed", None));
        assert_eq!(feeds[1].title, "Beta");
    }

    #[test]
    fn takes_the_category_from_the_enclosing_outline() {
        let xml = r#"<opml><body>
              <outline text="News">
                <outline type="rss" text="Alpha" xmlUrl="https://a.example/feed" />
              </outline>
              <outline text="Code">
                <outline type="rss" text="Beta" xmlUrl="https://b.example/feed" />
              </outline>
              <outline type="rss" text="Loose" xmlUrl="https://c.example/feed" />
            </body></opml>"#;

        let feeds = parse(xml).unwrap();
        assert_eq!(feeds[0].category.as_deref(), Some("News"));
        assert_eq!(feeds[1].category.as_deref(), Some("Code"));
        assert_eq!(feeds[2].category, None, "an ungrouped feed has no category");
    }

    #[test]
    fn nested_groups_use_the_nearest_named_ancestor() {
        let xml = r#"<opml><body>
              <outline text="Everything">
                <outline text="Rust">
                  <outline type="rss" text="Alpha" xmlUrl="https://a.example/feed" />
                </outline>
                <outline type="rss" text="Beta" xmlUrl="https://b.example/feed" />
              </outline>
            </body></opml>"#;

        let feeds = parse(xml).unwrap();
        assert_eq!(feeds[0].category.as_deref(), Some("Rust"));
        assert_eq!(feeds[1].category.as_deref(), Some("Everything"));
    }

    #[test]
    fn a_feed_outline_with_children_does_not_become_a_category() {
        // Some exporters wrap each feed in a non-self-closing outline.
        let xml = r#"<opml><body>
              <outline text="News">
                <outline type="rss" text="Alpha" xmlUrl="https://a.example/feed"></outline>
                <outline type="rss" text="Beta" xmlUrl="https://b.example/feed"></outline>
              </outline>
            </body></opml>"#;

        let feeds = parse(xml).unwrap();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].category.as_deref(), Some("News"));
        assert_eq!(
            feeds[1].category.as_deref(),
            Some("News"),
            "the first feed must not have become the second one's category"
        );
    }

    #[test]
    fn attribute_names_are_matched_case_insensitively() {
        let xml = r#"<opml><body>
              <outline XMLURL="https://a.example/feed" TEXT="Alpha" HTMLURL="https://a.example/" />
            </body></opml>"#;

        let feeds = parse(xml).unwrap();
        assert_eq!(feeds[0].feed_url, "https://a.example/feed");
        assert_eq!(feeds[0].title, "Alpha");
        assert_eq!(feeds[0].site_url.as_deref(), Some("https://a.example/"));
    }

    #[test]
    fn title_wins_over_text_and_the_url_is_the_last_resort() {
        let both = r#"<opml><body>
              <outline title="Preferred" text="Fallback" xmlUrl="https://a.example/feed" />
            </body></opml>"#;
        assert_eq!(parse(both).unwrap()[0].title, "Preferred");

        let neither = r#"<opml><body><outline xmlUrl="https://a.example/feed" /></body></opml>"#;
        assert_eq!(parse(neither).unwrap()[0].title, "https://a.example/feed");
    }

    #[test]
    fn whitespace_and_entities_in_attributes_are_resolved() {
        let xml = "<opml><body><outline text=\"Tom &amp; Jerry\"\n \
                    xmlUrl=\"  https://a.example/feed  \" /></body></opml>";
        let feeds = parse(xml).unwrap();
        assert_eq!(feeds[0].title, "Tom & Jerry");
        assert_eq!(feeds[0].feed_url, "https://a.example/feed");
    }

    #[test]
    fn an_outline_without_a_feed_url_is_not_a_feed() {
        let xml = r#"<opml><body><outline text="Just a folder" /></body></opml>"#;
        assert!(matches!(parse(xml), Err(OpmlError::Empty)));

        let blank = r#"<opml><body><outline text="x" xmlUrl="" /></body></opml>"#;
        assert!(matches!(parse(blank), Err(OpmlError::Empty)));
    }

    #[test]
    fn a_file_that_is_not_opml_at_all_is_refused() {
        assert!(matches!(parse("not xml"), Err(OpmlError::Empty)));
        assert!(matches!(parse(""), Err(OpmlError::Empty)));
        assert!(parse("<opml><body><outline").is_err());
    }

    #[test]
    fn export_groups_by_category_in_order() {
        let feeds = [
            feed("Alpha", "https://a.example/feed", Some("News")),
            feed("Beta", "https://b.example/feed", Some("News")),
            feed("Gamma", "https://c.example/feed", Some("Code")),
        ];

        let xml = write("Rustle subscriptions", &feeds);

        assert_eq!(xml.matches("<outline text=\"News\">").count(), 1);
        assert_eq!(xml.matches("<outline text=\"Code\">").count(), 1);
        assert_eq!(xml.matches("</outline>").count(), 2, "one close per group");
        assert!(xml.contains("<title>Rustle subscriptions</title>"));
    }

    #[test]
    fn export_escapes_text_that_would_otherwise_break_the_document() {
        let feeds = [OpmlFeed {
            title: "Tom & \"Jerry\" <br>".to_owned(),
            feed_url: "https://a.example/feed?a=1&b=2".to_owned(),
            site_url: Some("https://a.example/".to_owned()),
            category: Some("News & Views".to_owned()),
        }];

        let xml = write("Subs", &feeds);
        assert!(!xml.contains("<br>"));
        assert!(xml.contains("Tom &amp; &quot;Jerry&quot; &lt;br&gt;"));
        assert!(xml.contains("a=1&amp;b=2"));

        // And it survives a round trip unchanged.
        let parsed = parse(&xml).unwrap();
        assert_eq!(parsed, feeds);
    }

    #[test]
    fn round_trips_through_write_and_parse() {
        let feeds = [
            OpmlFeed {
                title: "Alpha".to_owned(),
                feed_url: "https://a.example/feed".to_owned(),
                site_url: Some("https://a.example/".to_owned()),
                category: Some("News".to_owned()),
            },
            OpmlFeed {
                title: "Beta".to_owned(),
                feed_url: "https://b.example/feed".to_owned(),
                site_url: None,
                category: Some("News".to_owned()),
            },
        ];

        assert_eq!(parse(&write("Subs", &feeds)).unwrap(), feeds);
    }

    #[test]
    fn an_uncategorised_feed_round_trips_as_uncategorised() {
        // `None` is written as an empty group title and must read back as `None`, not as
        // an empty-string category.
        let feeds = [feed("Alpha", "https://a.example/feed", None)];
        assert_eq!(parse(&write("Subs", &feeds)).unwrap(), feeds);
    }
}
