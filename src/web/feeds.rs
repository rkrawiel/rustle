//! The subscription panel: add, edit, delete and refresh feeds.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing};
use serde::Deserialize;
use url::Url;

use crate::db::category::{self, Category};
use crate::db::feed::{self, FeedSummary};
use crate::error::AppError;
use crate::feed::resolve;
use crate::state::AppState;
use crate::web::categories::is_unique_violation;
use crate::web::extract::{CsrfForm, CurrentUser, NoFields};
use crate::web::layout::{Html, Layout};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/feeds", routing::get(index).post(create))
        .route("/feeds/new", routing::get(new))
        .route("/feeds/refresh", routing::post(refresh_all))
        .route("/feeds/{id}/edit", routing::get(edit))
        .route("/feeds/{id}", routing::post(update))
        .route("/feeds/{id}/delete", routing::post(delete))
        .route("/feeds/{id}/refresh", routing::post(refresh))
}

#[derive(Template)]
#[template(path = "feeds.html")]
struct FeedsPage {
    layout: Layout,
    feeds: Vec<FeedSummary>,
}

async fn index(State(state): State<AppState>, current: CurrentUser) -> Result<Response, AppError> {
    let feeds = feed::list_summaries(state.db(), current.user.id).await?;

    Ok(Html(FeedsPage {
        layout: Layout::for_user(&state, &current, "Feeds")
            .await?
            .at("feeds"),
        feeds,
    })
    .into_response())
}

#[derive(Template)]
#[template(path = "feed_form.html")]
struct FeedFormPage {
    layout: Layout,
    categories: Vec<Category>,
    /// `None` when subscribing, `Some` when editing.
    feed_id: Option<i64>,
    title: String,
    feed_url: String,
    /// `0` means no category is selected, which no real category id can be.
    category_id: i64,
    error: Option<String>,
}

async fn new(State(state): State<AppState>, current: CurrentUser) -> Result<Response, AppError> {
    let categories = category::list(state.db(), current.user.id).await?;

    Ok(Html(FeedFormPage {
        layout: Layout::for_user(&state, &current, "Add a feed")
            .await?
            .at("feeds"),
        categories,
        feed_id: None,
        title: String::new(),
        feed_url: String::new(),
        category_id: 0,
        error: None,
    })
    .into_response())
}

async fn edit(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let existing = feed::find(state.db(), current.user.id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let categories = category::list(state.db(), current.user.id).await?;

    Ok(Html(FeedFormPage {
        layout: Layout::for_user(&state, &current, format!("Edit {}", existing.title))
            .await?
            .at("feeds"),
        categories,
        feed_id: Some(existing.id),
        title: existing.title,
        feed_url: existing.feed_url,
        category_id: existing.category_id,
        error: None,
    })
    .into_response())
}

#[derive(Deserialize)]
struct FeedSubmission {
    url: String,
    /// Absent means "put it in the default category", which is what the add form does
    /// when the reader has not made any categories yet.
    category_id: Option<i64>,
    /// Only the edit form sends a title; subscribing takes the title from the feed.
    title: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    CsrfForm(form): CsrfForm<FeedSubmission>,
) -> Result<Response, AppError> {
    let url = parse_url(&form.url)?;

    let category_id = match form.category_id {
        Some(id) => {
            category::find(state.db(), current.user.id, id)
                .await?
                .ok_or(AppError::NotFound)?
                .id
        }
        None => {
            category::find_or_create(state.db(), current.user.id, category::DEFAULT_TITLE)
                .await?
                .id
        }
    };

    // Fetch the address before saving anything, so that pasting a site URL subscribes to
    // the feed that page advertises, and so an unreachable address is reported now rather
    // than as a failing subscription an hour later.
    let resolved = resolve::resolve(state.fetcher(), &url)
        .await
        .map_err(|err| AppError::BadRequest(err.to_string()))?;

    let title = resolved
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| resolved.feed_url.host_str().unwrap_or("Untitled feed"));

    let interval = state.config().poll_interval.as_secs().min(i32::MAX as u64) as i32;

    match feed::create(
        state.db(),
        current.user.id,
        category_id,
        title,
        resolved.feed_url.as_str(),
        resolved.site_url.as_deref(),
        interval,
    )
    .await
    {
        Ok(created) => {
            // Poll it straight away rather than waiting out the jittered first interval:
            // a new subscription with no entries in it looks broken.
            feed::request_refresh(state.db(), current.user.id, created.id).await?;
            state.wake_poller();
            Ok(Redirect::to("/feeds").into_response())
        }
        Err(err) if is_unique_violation(&err) => Err(AppError::BadRequest(
            "You are already subscribed to that feed.".to_owned(),
        )),
        Err(err) => Err(err.into()),
    }
}

async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<FeedSubmission>,
) -> Result<Response, AppError> {
    let url = parse_url(&form.url)?;
    let title = form
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| AppError::BadRequest("A feed needs a title.".to_owned()))?;

    let category_id = form
        .category_id
        .ok_or_else(|| AppError::BadRequest("Pick a category.".to_owned()))?;
    category::find(state.db(), current.user.id, category_id)
        .await?
        .ok_or(AppError::NotFound)?;

    match feed::update(
        state.db(),
        current.user.id,
        id,
        category_id,
        title,
        url.as_str(),
    )
    .await
    {
        Ok(true) => Ok(Redirect::to("/feeds").into_response()),
        Ok(false) => Err(AppError::NotFound),
        Err(err) if is_unique_violation(&err) => Err(AppError::BadRequest(
            "Another subscription already uses that address.".to_owned(),
        )),
        Err(err) => Err(err.into()),
    }
}

async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    _form: CsrfForm<NoFields>,
) -> Result<Response, AppError> {
    if !feed::delete(state.db(), current.user.id, id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to("/feeds").into_response())
}

async fn refresh(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    _form: CsrfForm<NoFields>,
) -> Result<Response, AppError> {
    if !feed::request_refresh(state.db(), current.user.id, id).await? {
        return Err(AppError::NotFound);
    }
    // The poller does the fetching; see `feed::scheduler`. Bringing `next_check_at`
    // forward and waking it is the whole of "refresh now", which is why there is only
    // ever one code path that fetches a feed.
    state.wake_poller();
    Ok(Redirect::to("/feeds").into_response())
}

async fn refresh_all(
    State(state): State<AppState>,
    current: CurrentUser,
    _form: CsrfForm<NoFields>,
) -> Result<Response, AppError> {
    let count = feed::request_refresh_all(state.db(), current.user.id).await?;
    tracing::info!("queued {count} feed(s) for an immediate check");
    state.wake_poller();
    Ok(Redirect::to("/feeds").into_response())
}

/// Accepts what someone would actually paste, including a bare hostname.
fn parse_url(raw: &str) -> Result<Url, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("Enter a feed address.".to_owned()));
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };

    let url = Url::parse(&candidate)
        .map_err(|_| AppError::BadRequest(format!("{trimmed:?} is not a valid address.")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::BadRequest(
            "Only http and https addresses can be fetched.".to_owned(),
        ));
    }
    if url.host_str().is_none() {
        return Err(AppError::BadRequest(
            "That address has no hostname.".to_owned(),
        ));
    }

    Ok(url)
}
