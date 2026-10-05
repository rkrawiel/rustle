//! Serving the embedded, content-hashed static assets.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::assets;

/// A year, and immutable: the path contains a hash of the bytes, so changing a file
/// changes its URL and no cache can serve a stale one.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// The hash in the path is not verified against the asset. It exists to bust caches, not
/// to authenticate anything, and rejecting a stale hash would only break a page mid-deploy
/// that is still linking the previous URL.
pub async fn serve(Path((_hash, name)): Path<(String, String)>) -> Response {
    match assets::get(&name) {
        Some(asset) => (
            [
                (header::CONTENT_TYPE, asset.content_type),
                (header::CACHE_CONTROL, IMMUTABLE),
            ],
            asset.bytes.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
