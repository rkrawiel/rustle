//! Category management.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing};
use serde::Deserialize;

use crate::db::category::{self, CategoryWithCounts};
use crate::error::AppError;
use crate::state::AppState;
use crate::web::extract::{CsrfForm, CurrentUser};
use crate::web::layout::{Html, Layout};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/categories", routing::get(index).post(create))
        .route("/categories/{id}", routing::post(update))
        .route("/categories/{id}/delete", routing::post(delete))
}

#[derive(Template)]
#[template(path = "categories.html")]
struct CategoriesPage {
    layout: Layout,
    categories: Vec<CategoryWithCounts>,
}

async fn index(State(state): State<AppState>, current: CurrentUser) -> Result<Response, AppError> {
    let categories = category::list_with_counts(state.db(), current.user.id).await?;

    Ok(Html(CategoriesPage {
        layout: Layout::for_user(&state, &current, "Categories")
            .await?
            .at("categories"),
        categories,
    })
    .into_response())
}

#[derive(Deserialize)]
struct TitleForm {
    title: String,
}

async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    CsrfForm(form): CsrfForm<TitleForm>,
) -> Result<Response, AppError> {
    let title = form.title.trim();
    if title.is_empty() {
        return Err(AppError::BadRequest("A category needs a name.".to_owned()));
    }

    category::create(state.db(), current.user.id, title)
        .await
        .map_err(|err| duplicate_title(err, title))?;

    Ok(Redirect::to("/categories").into_response())
}

async fn update(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<TitleForm>,
) -> Result<Response, AppError> {
    let title = form.title.trim();
    if title.is_empty() {
        return Err(AppError::BadRequest("A category needs a name.".to_owned()));
    }

    let renamed = category::rename(state.db(), current.user.id, id, title)
        .await
        .map_err(|err| duplicate_title(err, title))?;

    if !renamed {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to("/categories").into_response())
}

async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    _form: CsrfForm<crate::web::extract::NoFields>,
) -> Result<Response, AppError> {
    match category::delete(state.db(), current.user.id, id).await {
        Ok(true) => Ok(Redirect::to("/categories").into_response()),
        Ok(false) => Err(AppError::NotFound),
        // `feeds.category_id` is `on delete restrict`, so a category with feeds in it
        // raises a foreign-key violation rather than quietly deleting the feeds and every
        // entry in them. Say what to do about it instead of showing a database error.
        Err(err) if is_still_referenced(&err) => Err(AppError::BadRequest(
            "That category still has feeds in it. Move them to another category first.".to_owned(),
        )),
        Err(err) => Err(err.into()),
    }
}

fn duplicate_title(err: sqlx::Error, title: &str) -> AppError {
    if is_unique_violation(&err) {
        AppError::BadRequest(format!("There is already a category called {title:?}."))
    } else {
        err.into()
    }
}

/// `23505` is Postgres' unique-violation code.
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// A row that cannot be deleted because something still references it.
///
/// Postgres raises `23001` (`restrict_violation`) for an explicit `on delete restrict`,
/// which is what `feeds.category_id` declares — **not** the `23503`
/// (`foreign_key_violation`) you get from an unqualified reference. Both are matched so
/// that a future schema change to either form still produces a readable message.
fn is_still_referenced(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(db)
            if matches!(db.code().as_deref(), Some("23001") | Some("23503"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the SQLSTATE that `on delete restrict` actually produces. Matching `23503`
    /// alone looks right and silently turns this into a 500.
    #[sqlx::test]
    async fn a_restrict_violation_is_recognised_rather_than_becoming_a_500(pool: sqlx::PgPool) {
        sqlx::query("insert into users (id, username, password_hash) values (1, 'r', 'x')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into categories (id, user_id, title) values (1, 1, 'News')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into feeds (user_id, category_id, title, feed_url, check_interval_seconds)
             values (1, 1, 'f', 'https://a.example/f', 3600)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let err = category::delete(&pool, 1, 1)
            .await
            .expect_err("restrict should bite");

        assert!(is_still_referenced(&err), "got {err:?}");
        let sqlx::Error::Database(db) = &err else {
            panic!("expected a database error, got {err:?}")
        };
        assert_eq!(
            db.code().as_deref(),
            Some("23001"),
            "restrict_violation, not 23503"
        );
    }
}
