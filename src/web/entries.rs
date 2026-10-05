//! Reading: the list views, the entry page, and the read and star toggles.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::entry::{self, Cursor, Direction, EntrySummary, View};
use crate::db::{category, feed};
use crate::error::AppError;
use crate::state::AppState;
use crate::web::extract::{CsrfForm, CurrentUser};
use crate::web::layout::{Html, Layout};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", routing::get(|| async { Redirect::to("/unread") }))
        .route("/unread", routing::get(unread))
        .route("/all", routing::get(all))
        .route("/starred", routing::get(starred))
        .route("/feeds/{id}", routing::get(by_feed))
        .route("/categories/{id}", routing::get(by_category))
        .route("/entries/{id}", routing::get(show))
        .route("/entries/{id}/read", routing::post(toggle_read))
        .route("/entries/{id}/star", routing::post(toggle_star))
        .route("/entries/read-all", routing::post(read_all))
}

/// Query parameters shared by every list page.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ListQuery {
    /// Keyset cursor: the `published_at` of the last entry on the previous page.
    after: Option<DateTime<Utc>>,
    after_id: Option<i64>,
}

impl ListQuery {
    fn cursor(&self) -> Option<Cursor> {
        // Both halves or neither: a timestamp without an id cannot break ties, so a
        // half-supplied cursor is treated as no cursor rather than as a partial one.
        match (self.after, self.after_id) {
            (Some(published_at), Some(id)) => Some(Cursor { published_at, id }),
            _ => None,
        }
    }
}

#[derive(Template)]
#[template(path = "entries.html")]
struct ListPage {
    layout: Layout,
    heading: String,
    /// Appears under the heading: "127 unread", "3 starred".
    summary: String,
    entries: Vec<EntrySummary>,
    /// The view in the form the action forms post back, e.g. `unread` or `feed:12`.
    view_token: String,
    /// This view's own path, so the "Older" link can be built without re-deriving it.
    path: String,
    older: Option<String>,
    /// Shown when the list is empty, phrased for the particular view.
    empty: &'static str,
    /// `Some` only when this view is a single feed, so the "Refresh this feed" shortcut
    /// (`r`) and its no-JS form have a feed to act on.
    refresh_feed_id: Option<i64>,
}

async fn render_list(
    state: &AppState,
    current: &CurrentUser,
    view: View,
    query: &ListQuery,
    heading: String,
    empty: &'static str,
) -> Result<Response, AppError> {
    let limit = i64::from(current.user.entries_per_page);
    let page = entry::list(state.db(), current.user.id, view, query.cursor(), limit).await?;

    let summary = match view {
        View::Unread => plural(page.total, "unread entry", "unread entries"),
        View::Starred => plural(page.total, "starred entry", "starred entries"),
        _ => plural(page.total, "entry", "entries"),
    };

    Ok(Html(ListPage {
        layout: Layout::for_user(state, current, heading.clone())
            .await?
            .at(view.nav()),
        heading,
        summary,
        view_token: view_token(view),
        path: view.path(),
        // Percent-encoded through the same `serde_urlencoded` the `Query<ListQuery>`
        // extractor decodes with, rather than hand-built: `published_at`'s RFC 3339
        // rendering contains a literal `+` before the UTC offset, which is reserved in a
        // query string and would otherwise be read back as a space.
        older: page.older.map(|c| {
            let query = ListQuery {
                after: Some(c.published_at),
                after_id: Some(c.id),
            };
            format!(
                "?{}",
                serde_urlencoded::to_string(query).unwrap_or_default()
            )
        }),
        entries: page.entries,
        empty,
        refresh_feed_id: match view {
            View::Feed(id) => Some(id),
            _ => None,
        },
    })
    .into_response())
}

fn plural(count: i64, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

async fn unread(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<ListQuery>,
) -> Result<Response, AppError> {
    render_list(
        &state,
        &current,
        View::Unread,
        &query,
        "Unread".to_owned(),
        "Nothing unread. Everything here is read.",
    )
    .await
}

async fn all(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<ListQuery>,
) -> Result<Response, AppError> {
    render_list(
        &state,
        &current,
        View::All,
        &query,
        "All entries".to_owned(),
        "No entries yet. Subscribe to a feed and Rustle will start collecting them.",
    )
    .await
}

async fn starred(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<ListQuery>,
) -> Result<Response, AppError> {
    render_list(
        &state,
        &current,
        View::Starred,
        &query,
        "Starred".to_owned(),
        "Nothing starred. Starred entries are kept even when retention clears the rest.",
    )
    .await
}

async fn by_feed(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Query(query): Query<ListQuery>,
) -> Result<Response, AppError> {
    let feed = feed::find(state.db(), current.user.id, id)
        .await?
        .ok_or(AppError::NotFound)?;

    render_list(
        &state,
        &current,
        View::Feed(id),
        &query,
        feed.title,
        "Nothing from this feed yet.",
    )
    .await
}

async fn by_category(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Query(query): Query<ListQuery>,
) -> Result<Response, AppError> {
    let category = category::find(state.db(), current.user.id, id)
        .await?
        .ok_or(AppError::NotFound)?;

    render_list(
        &state,
        &current,
        View::Category(id),
        &query,
        category.title,
        "Nothing in this category yet.",
    )
    .await
}

#[derive(Template)]
#[template(path = "entry.html")]
struct EntryPage {
    layout: Layout,
    entry: entry::Entry,
    view_token: String,
    /// Where "back to the list" goes.
    view_path: String,
    newer: Option<i64>,
    older: Option<i64>,
}

/// The entry page. Opening it marks the entry read, per the brief.
async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Query(query): Query<ViewQuery>,
) -> Result<Response, AppError> {
    let entry = entry::find(state.db(), current.user.id, id)
        .await?
        .ok_or(AppError::NotFound)?;

    let view = query.view();

    // Neighbours are resolved *before* marking read, so that in the unread view "next"
    // still points at what was next when the page was requested.
    let newer =
        entry::neighbour(state.db(), current.user.id, view, &entry, Direction::Newer).await?;
    let older =
        entry::neighbour(state.db(), current.user.id, view, &entry, Direction::Older).await?;

    entry::mark_read(state.db(), current.user.id, id).await?;

    Ok(Html(EntryPage {
        layout: Layout::for_user(&state, &current, entry.title.clone())
            .await?
            .at(view.nav()),
        view_token: view_token(view),
        view_path: view.path(),
        entry,
        newer,
        older,
    })
    .into_response())
}

/// Which view an action was performed from, so the redirect goes back where the reader was.
#[derive(Debug, Deserialize, Default)]
pub struct ViewQuery {
    view: Option<String>,
}

impl ViewQuery {
    fn view(&self) -> View {
        self.view.as_deref().map_or(View::Unread, parse_view_token)
    }
}

#[derive(Debug, Deserialize)]
struct ActionForm {
    /// The view to return to, in the same `feed:12` form the templates emit.
    view: Option<String>,
}

impl ActionForm {
    fn view(&self) -> View {
        self.view.as_deref().map_or(View::Unread, parse_view_token)
    }
}

async fn toggle_read(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<ActionForm>,
) -> Result<Response, AppError> {
    entry::toggle_read(state.db(), current.user.id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Redirect::to(&form.view().path()).into_response())
}

async fn toggle_star(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<ActionForm>,
) -> Result<Response, AppError> {
    entry::toggle_starred(state.db(), current.user.id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Redirect::to(&form.view().path()).into_response())
}

async fn read_all(
    State(state): State<AppState>,
    current: CurrentUser,
    CsrfForm(form): CsrfForm<ActionForm>,
) -> Result<Response, AppError> {
    let view = form.view();
    let marked = entry::mark_all_read(state.db(), current.user.id, view).await?;
    tracing::info!(
        "marked {marked} entr{} read",
        if marked == 1 { "y" } else { "ies" }
    );
    Ok(Redirect::to(&view.path()).into_response())
}

/// A `View` as a form value. Compact and opaque rather than a set of separate fields, so
/// an action form carries one hidden input instead of three.
fn view_token(view: View) -> String {
    match view {
        View::Unread => "unread".to_owned(),
        View::All => "all".to_owned(),
        View::Starred => "starred".to_owned(),
        View::Feed(id) => format!("feed:{id}"),
        View::Category(id) => format!("category:{id}"),
    }
}

/// The inverse. An unrecognised token falls back to the unread view rather than erroring:
/// the value only decides where a redirect lands, and the queries it reaches are scoped by
/// `user_id` regardless, so a bad value cannot reveal anything.
fn parse_view_token(token: &str) -> View {
    match token.split_once(':') {
        Some(("feed", id)) => id.parse().map_or(View::Unread, View::Feed),
        Some(("category", id)) => id.parse().map_or(View::Unread, View::Category),
        _ => match token {
            "all" => View::All,
            "starred" => View::Starred,
            _ => View::Unread,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_tokens_round_trip() {
        for view in [
            View::Unread,
            View::All,
            View::Starred,
            View::Feed(12),
            View::Category(7),
        ] {
            assert_eq!(parse_view_token(&view_token(view)), view);
        }
    }

    #[test]
    fn an_unrecognised_token_falls_back_to_unread() {
        for token in [
            "",
            "nonsense",
            "feed:",
            "feed:abc",
            "category:-",
            "feed:1:2",
        ] {
            assert_eq!(
                parse_view_token(token),
                View::Unread,
                "{token:?} should fall back"
            );
        }
    }

    #[test]
    fn counts_are_pluralised() {
        assert_eq!(plural(0, "entry", "entries"), "0 entries");
        assert_eq!(plural(1, "entry", "entries"), "1 entry");
        assert_eq!(plural(2, "entry", "entries"), "2 entries");
    }
}
