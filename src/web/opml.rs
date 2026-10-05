//! OPML import and export over HTTP.

use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing};

use crate::db::{category, feed};
use crate::error::AppError;
use crate::feed::opml::{self, OpmlFeed};
use crate::state::AppState;
use crate::web::categories::is_unique_violation;
use crate::web::csrf;
use crate::web::extract::CurrentUser;

/// A subscription list is a few hundred kilobytes at most. The limit is set on this route
/// alone rather than globally, because every other body here is a small form.
const MAX_UPLOAD: usize = 2 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/opml/export", routing::get(export))
        .route(
            "/opml/import",
            routing::post(import).layer(DefaultBodyLimit::max(MAX_UPLOAD)),
        )
}

async fn export(State(state): State<AppState>, current: CurrentUser) -> Result<Response, AppError> {
    // Already ordered by category then title, which is what `opml::write` needs to group
    // in a single pass, and which also makes the exported file stable between runs.
    let feeds = feed::list_summaries(state.db(), current.user.id).await?;

    let feeds: Vec<OpmlFeed> = feeds
        .into_iter()
        .map(|feed| OpmlFeed {
            title: feed.title,
            feed_url: feed.feed_url,
            site_url: feed.site_url,
            category: Some(feed.category_title),
        })
        .collect();

    let document = opml::write("Rustle subscriptions", &feeds);

    Ok((
        [
            (header::CONTENT_TYPE, "text/x-opml; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"rustle-subscriptions.opml\"",
            ),
        ],
        document,
    )
        .into_response())
}

/// A multipart upload, so `CsrfForm` does not apply.
///
/// The CSRF field is rendered before the file input, and multipart fields arrive in
/// document order, so the token is checked before the upload is read. That ordering is
/// the reason the field comes first in `feeds.html`.
async fn import(
    State(state): State<AppState>,
    current: CurrentUser,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    if !csrf::same_site(&headers, &state.config().base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let mut verified = false;
    let mut document = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| AppError::BadRequest(format!("That upload could not be read: {err}")))?
    {
        match field.name() {
            Some("csrf_token") => {
                let submitted = field.text().await.unwrap_or_default();
                if !csrf::tokens_match(&submitted, &current.session.csrf_token) {
                    return Ok(StatusCode::FORBIDDEN.into_response());
                }
                verified = true;
            }
            Some("opml") => {
                if !verified {
                    // The token must precede the file, or we would be buffering an
                    // unverified upload.
                    return Ok(StatusCode::FORBIDDEN.into_response());
                }
                document = Some(field.text().await.map_err(|err| {
                    AppError::BadRequest(format!("That file is not readable text: {err}"))
                })?);
            }
            _ => {}
        }
    }

    let document = document
        .ok_or_else(|| AppError::BadRequest("Choose an OPML file to import.".to_owned()))?;

    let parsed = opml::parse(&document).map_err(|err| AppError::BadRequest(err.to_string()))?;
    let interval = state.config().poll_interval.as_secs().min(i32::MAX as u64) as i32;

    let mut added = 0;
    let mut skipped = 0;

    for entry in parsed {
        let category_title = entry
            .category
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(category::DEFAULT_TITLE);

        let category =
            category::find_or_create(state.db(), current.user.id, category_title).await?;

        match feed::create(
            state.db(),
            current.user.id,
            category.id,
            &entry.title,
            &entry.feed_url,
            entry.site_url.as_deref(),
            interval,
        )
        .await
        {
            Ok(_) => added += 1,
            // Importing a file that overlaps what is already subscribed is the normal
            // case, not an error: skip the duplicates and carry on.
            Err(err) if is_unique_violation(&err) => skipped += 1,
            Err(err) => return Err(err.into()),
        }
    }

    tracing::info!("OPML import added {added} feed(s), skipped {skipped} already subscribed");
    Ok(Redirect::to("/feeds").into_response())
}
