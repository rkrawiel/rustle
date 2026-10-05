//! Full-text search over titles and content.

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing};
use serde::Deserialize;

use crate::db::entry::{self, EntrySummary};
use crate::error::AppError;
use crate::state::AppState;
use crate::web::extract::CurrentUser;
use crate::web::layout::{Html, Layout};

pub fn routes() -> Router<AppState> {
    Router::new().route("/search", routing::get(search))
}

#[derive(Debug, Deserialize, Default)]
struct SearchQuery {
    q: Option<String>,
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchPage {
    layout: Layout,
    /// `None` before anything has been searched for yet.
    query: Option<String>,
    entries: Vec<EntrySummary>,
    total: i64,
    limit: i64,
}

async fn search(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<SearchQuery>,
) -> Result<Response, AppError> {
    // A blank term is "nothing searched yet", not "match nothing": `websearch_to_tsquery`
    // would happily return an empty query for it, but that is one round trip to the
    // database to learn what trimming the input already tells us for free.
    let term = params.q.as_deref().map(str::trim).filter(|q| !q.is_empty());

    let limit = i64::from(current.user.entries_per_page);
    let (entries, total) = match term {
        Some(term) => {
            let entries = entry::search(state.db(), current.user.id, term, limit).await?;
            let total = entry::search_count(state.db(), current.user.id, term).await?;
            (entries, total)
        }
        None => (Vec::new(), 0),
    };

    Ok(Html(SearchPage {
        layout: Layout::for_user(&state, &current, "Search")
            .await?
            .searching(term.unwrap_or_default()),
        query: term.map(str::to_owned),
        entries,
        total,
        limit,
    })
    .into_response())
}
