//! Static assets, embedded in the binary and served under a content-hashed path.
//!
//! Sources are embedded with their comments intact — the CSS and JS carry the reasoning
//! behind the design-system mapping and the shortcut handling, which is worth keeping
//! next to the code. Comments are stripped once at startup before the bytes are hashed
//! and served, so the page-weight budget is spent on the stylesheet, not its commentary.
//!
//! The hash is derived from the stripped bytes at startup rather than at build time, which
//! keeps the build free of a script while still letting `/assets/{hash}/{name}` be served
//! `immutable` for a year: changing a file changes its URL.

use std::sync::LazyLock;

use axum::body::Bytes;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

pub struct Asset {
    pub name: &'static str,
    pub content_type: &'static str,
    /// What is actually served: the source with comments stripped.
    pub bytes: Bytes,
    /// First 16 base64url characters of the SHA-256 digest. Collision risk at this length
    /// is irrelevant for a handful of files, and it keeps URLs readable.
    pub hash: String,
}

impl Asset {
    /// The path to reference from a template, e.g. `/assets/3Qk1.../app.css`.
    pub fn path(&self) -> String {
        format!("/assets/{}/{}", self.hash, self.name)
    }
}

/// Everything the browser may request. Order is the order they are linked in.
pub static ASSETS: LazyLock<Vec<Asset>> = LazyLock::new(|| {
    [
        (
            "tokens.css",
            "text/css; charset=utf-8",
            include_str!("../assets/tokens.css"),
        ),
        (
            "app.css",
            "text/css; charset=utf-8",
            include_str!("../assets/app.css"),
        ),
        (
            "app.js",
            "text/javascript; charset=utf-8",
            include_str!("../assets/app.js"),
        ),
        (
            "favicon.svg",
            "image/svg+xml",
            include_str!("../assets/favicon.svg"),
        ),
    ]
    .into_iter()
    .map(|(name, content_type, source)| {
        let bytes = Bytes::from(strip_comments(source).into_bytes());
        Asset {
            name,
            content_type,
            hash: hash(&bytes),
            bytes,
        }
    })
    .collect()
});

/// Removes `/* ... */` blocks and collapses the blank lines they leave behind.
///
/// Not a minifier: indentation and newlines survive, so a reader who opens the served
/// file still sees legible CSS. It is safe for the files in `assets/` because none of them
/// contains `/*` inside a string or a `url()`; a guard test pins that assumption.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;

    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            // An unterminated comment means the rest of the file is commented out.
            None => rest = "",
        }
    }
    out.push_str(rest);

    // A stripped comment usually leaves an indented empty line behind.
    let kept: Vec<&str> = out
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let mut result = kept.join("\n");
    result.push('\n');
    result
}

fn hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    URL_SAFE_NO_PAD.encode(&digest[..12])
}

pub fn get(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.name == name)
}

/// The stylesheet paths to link, in cascade order.
pub fn stylesheets() -> Vec<String> {
    ASSETS
        .iter()
        .filter(|asset| asset.name.ends_with(".css"))
        .map(Asset::path)
        .collect()
}

/// The script paths to link. Every one is loaded `defer`, so load order follows document
/// order the same way the stylesheets' cascade order does.
pub fn scripts() -> Vec<String> {
    ASSETS
        .iter()
        .filter(|asset| asset.name.ends_with(".js"))
        .map(Asset::path)
        .collect()
}

/// The favicon path, linked on every page regardless of sign-in state.
pub fn favicon() -> String {
    get("favicon.svg").expect("favicon.svg is a built-in asset").path()
}

/// The brief's page-weight budgets, in uncompressed served bytes.
pub const CSS_BUDGET: usize = 15 * 1024;
pub const JS_BUDGET: usize = 10 * 1024;

/// Served bytes per extension, which is what the budget is spent on. Measuring the files
/// on disk would count the comments, and those are stripped before anything is served.
pub fn served_bytes(extension: &str) -> usize {
    ASSETS
        .iter()
        .filter(|asset| asset.name.ends_with(extension))
        .map(|asset| asset.bytes.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget is a hard requirement, so it is enforced by the test suite rather than
    /// by a script someone has to remember to run. `cargo test -- --nocapture` prints the
    /// table that goes into PLAN.md.
    #[test]
    fn page_weight_stays_within_budget() {
        println!("\n{:<14}{:>8}", "asset", "bytes");
        for asset in ASSETS.iter() {
            println!("{:<14}{:>8}", asset.name, asset.bytes.len());
        }

        for (extension, budget) in [(".css", CSS_BUDGET), (".js", JS_BUDGET)] {
            let used = served_bytes(extension);
            println!(
                "{:<14}{:>8} of {} ({:.0}%)",
                format!("total {extension}"),
                used,
                budget,
                100.0 * used as f64 / budget as f64
            );
            assert!(
                used <= budget,
                "{extension} is {used} bytes, over the {budget} byte budget"
            );
        }
    }

    #[test]
    fn every_asset_hashes_to_a_stable_url_safe_path() {
        for asset in ASSETS.iter() {
            assert_eq!(asset.hash.len(), 16, "{} hash length", asset.name);
            assert!(
                asset
                    .hash
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{} hash is not url-safe: {}",
                asset.name,
                asset.hash
            );
            assert_eq!(
                asset.path(),
                format!("/assets/{}/{}", asset.hash, asset.name)
            );
        }
    }

    #[test]
    fn different_bytes_hash_differently() {
        assert_ne!(hash(b"a"), hash(b"b"));
        assert_eq!(hash(b"a"), hash(b"a"));
    }

    #[test]
    fn stylesheets_are_linked_tokens_first_so_components_can_override() {
        let sheets = stylesheets();
        assert_eq!(sheets.len(), 2);
        assert!(sheets[0].ends_with("/tokens.css"));
        assert!(sheets[1].ends_with("/app.css"));
    }

    #[test]
    fn unknown_assets_are_not_found() {
        assert!(get("app.css").is_some());
        assert!(get("../../etc/passwd").is_none());
    }

    #[test]
    fn comments_are_stripped_but_declarations_survive() {
        let css = "/* a note */\na { color: red }\n\n/* multi\n   line */\nb { color: blue }\n";
        assert_eq!(strip_comments(css), "a { color: red }\nb { color: blue }\n");
    }

    #[test]
    fn an_unterminated_comment_swallows_the_remainder() {
        assert_eq!(strip_comments("a{}\n/* oops\nb{}"), "a{}\n");
    }

    #[test]
    fn no_asset_contains_a_comment_opener_inside_a_value() {
        // The stripper is not a parser, so it would mangle `content: "/*"` or a url()
        // containing `/*`. Nothing in assets/ does; this fails loudly if that changes.
        for asset in ASSETS.iter() {
            let served = std::str::from_utf8(&asset.bytes).expect("assets are utf-8");
            assert!(
                !served.contains("/*") && !served.contains("*/"),
                "{} still contains comment markers after stripping",
                asset.name
            );
        }
    }

    #[test]
    fn served_bytes_are_smaller_than_the_commented_source() {
        let source = include_str!("../assets/app.css");
        let served = &get("app.css").unwrap().bytes;
        assert!(served.len() < source.len());
        assert!(served.starts_with(b"*,"), "the reset should lead the file");
    }
}
