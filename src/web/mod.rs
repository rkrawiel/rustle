//! Router assembly and the middleware stack.

pub mod assets;
pub mod auth;
pub mod categories;
pub mod cookie;
pub mod csrf;
pub mod entries;
pub mod extract;
pub mod feeds;
pub mod layout;
pub mod opml;
pub mod ratelimit;
pub mod search;
pub mod settings;

use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::routing::get;
use axum::{Router, response::IntoResponse};
use tower_http::compression::{CompressionLayer, CompressionLevel};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::error::AppError;
use crate::state::AppState;

/// How long a single request may take before the client gets a 408. Generous enough for a
/// cold query plan, short enough that a stuck connection cannot pile up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// `img-src` and `media-src` are open because entry content legitimately embeds images
/// from wherever the publisher hosts them, and a media proxy is out of scope. Rustle's own
/// pages make no third-party requests: no webfonts, no CDN, no analytics. Everything else
/// is denied by default, which matters because we render sanitized third-party HTML.
const CSP: &str = "default-src 'none'; style-src 'self'; script-src 'self'; \
     img-src * data:; media-src *; form-action 'self'; base-uri 'none'; \
     frame-ancestors 'none'";

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/assets/{hash}/{name}", get(assets::serve))
        .merge(auth::routes())
        .merge(categories::routes())
        .merge(entries::routes())
        .merge(feeds::routes())
        .merge(opml::routes())
        .merge(search::routes())
        .merge(settings::routes())
        .fallback(not_found)
        .layer(
            // Fastest, not the default: brotli at default quality costs single-digit
            // milliseconds per HTML response, which is most of the per-page budget.
            CompressionLayer::new()
                .gzip(true)
                .br(true)
                .quality(CompressionLevel::Fastest),
        )
        // `if_not_present`, so the asset handler's own `immutable` wins. Everything else
        // is uncacheable: HTML carries the server-rendered theme and read state, which go
        // stale in a reverse proxy or the browser's heuristic cache.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn not_found() -> AppError {
    AppError::NotFound
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match crate::db::ping(state.db()).await {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(err) => {
            tracing::error!("health check failed: {err}");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}
