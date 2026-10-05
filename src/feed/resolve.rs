//! Working out what feed a pasted address actually refers to.
//!
//! People paste the address of a site far more often than the address of its feed, so a
//! reader that only accepts feed URLs is a reader that looks broken. This fetches whatever
//! was given, and if it is an HTML page rather than a feed, follows the page's own
//! `<link rel="alternate">`.

use url::Url;

use crate::feed::fetch::{self, Fetched, Fetcher};
use crate::feed::{discover, parse};

#[derive(Debug, Clone)]
pub struct Resolved {
    /// The address to subscribe to, which may not be the one that was pasted.
    pub feed_url: Url,
    /// The feed's own title, so the subscription starts with a real name rather than a
    /// hostname standing in for one.
    pub title: Option<String>,
    pub site_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("{0}")]
    Unreachable(String),
    #[error("that address is a page with no feed on it")]
    NoFeedFound,
    #[error("that page offers a feed, but it could not be read: {0}")]
    FeedUnreadable(String),
}

/// Resolves an address to a feed, following one level of autodiscovery.
///
/// Only one level: a page's feed link pointing at another page is either a loop or a
/// mistake, and chasing it would turn a subscribe button into a crawler.
pub async fn resolve(fetcher: &Fetcher, pasted: &Url) -> Result<Resolved, ResolveError> {
    let (bytes, final_url) = get(fetcher, pasted).await?;
    let now = chrono::Utc::now();

    // Try it as a feed first. Most of the time that is what it is.
    if let Ok(channel) = parse::parse(&bytes, &final_url, now) {
        return Ok(Resolved {
            feed_url: final_url,
            title: channel.title,
            site_url: channel.site_url,
        });
    }

    // Not a feed, so treat it as a page and look for one. Lossy decoding rather than
    // refusing: a mislabelled charset should not stop us finding a `<link>` tag.
    let html = String::from_utf8_lossy(&bytes);
    let candidate = discover::find_feeds(&html, &final_url)
        .into_iter()
        .next()
        .ok_or(ResolveError::NoFeedFound)?;

    let (bytes, feed_url) = get(fetcher, &candidate.url).await?;
    let channel = parse::parse(&bytes, &feed_url, now)
        .map_err(|err| ResolveError::FeedUnreadable(err.to_string()))?;

    Ok(Resolved {
        feed_url,
        // The page's link title is often better than the feed's own ("Comments on …"
        // versus a bare site name), so it wins when the page gave one.
        title: candidate.title.or(channel.title),
        site_url: channel.site_url.or_else(|| Some(final_url.to_string())),
    })
}

/// Fetches unconditionally — there are no stored validators for an address nobody is
/// subscribed to yet.
async fn get(fetcher: &Fetcher, url: &Url) -> Result<(Vec<u8>, Url), ResolveError> {
    match fetch::fetch(fetcher, url, None, None).await {
        Ok(Fetched::Body {
            bytes, final_url, ..
        }) => Ok((bytes, final_url)),
        // Neither can happen without validators, but saying so beats a silent fallthrough.
        Ok(Fetched::NotModified) => Err(ResolveError::Unreachable(
            "the server reported no changes to something we have never fetched".to_owned(),
        )),
        Ok(Fetched::RateLimited { .. }) => Err(ResolveError::Unreachable(
            "that server is asking us to slow down; try again in a few minutes".to_owned(),
        )),
        Err(err) => Err(ResolveError::Unreachable(err.to_string())),
    }
}
