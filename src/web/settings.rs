//! The settings page: theme, reading preferences, and (routed here but implemented in
//! `web::auth`) the change-password form.

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing};
use serde::Deserialize;

use crate::db::user;
use crate::error::AppError;
use crate::password;
use crate::state::AppState;
use crate::theme::Theme;
use crate::web::extract::{CsrfForm, CurrentUser};
use crate::web::layout::{Html, Layout};

/// The database's own `check` constraints are the last word; these mirror them so a bad
/// value gets a sentence back rather than a generic 500.
const MIN_ENTRIES_PER_PAGE: i32 = 10;
const MAX_ENTRIES_PER_PAGE: i32 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings", routing::get(show).post(update_preferences))
        .route("/settings/theme", routing::post(update_theme))
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    layout: Layout,
    username: String,
    theme: Theme,
    entries_per_page: i32,
    retention_days: i32,
    min_entries_per_page: i32,
    max_entries_per_page: i32,
    min_password_length: usize,
}

async fn show(State(state): State<AppState>, current: CurrentUser) -> Result<Response, AppError> {
    Ok(Html(SettingsPage {
        layout: Layout::for_user(&state, &current, "Settings")
            .await?
            .at("settings"),
        username: current.user.username.clone(),
        theme: current.user.theme,
        entries_per_page: current.user.entries_per_page,
        retention_days: current.user.retention_days,
        min_entries_per_page: MIN_ENTRIES_PER_PAGE,
        max_entries_per_page: MAX_ENTRIES_PER_PAGE,
        min_password_length: password::MIN_LENGTH,
    })
    .into_response())
}

#[derive(Debug, Deserialize)]
struct Preferences {
    entries_per_page: i32,
    retention_days: i32,
}

async fn update_preferences(
    State(state): State<AppState>,
    current: CurrentUser,
    CsrfForm(form): CsrfForm<Preferences>,
) -> Result<Response, AppError> {
    if !(MIN_ENTRIES_PER_PAGE..=MAX_ENTRIES_PER_PAGE).contains(&form.entries_per_page) {
        return Err(AppError::BadRequest(format!(
            "Entries per page must be between {MIN_ENTRIES_PER_PAGE} and {MAX_ENTRIES_PER_PAGE}."
        )));
    }
    if form.retention_days < 0 {
        return Err(AppError::BadRequest(
            "Retention cannot be a negative number of days.".to_owned(),
        ));
    }

    user::set_preferences(
        state.db(),
        current.user.id,
        form.entries_per_page,
        form.retention_days,
    )
    .await?;

    Ok(Redirect::to("/settings").into_response())
}

#[derive(Debug, Deserialize)]
struct ThemeChoice {
    theme: String,
}

async fn update_theme(
    State(state): State<AppState>,
    current: CurrentUser,
    CsrfForm(form): CsrfForm<ThemeChoice>,
) -> Result<Response, AppError> {
    let theme = Theme::from_db(&form.theme);
    user::set_theme(state.db(), current.user.id, theme).await?;
    Ok(Redirect::to("/settings").into_response())
}
