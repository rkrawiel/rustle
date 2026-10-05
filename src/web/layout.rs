//! The shared context that every page template embeds, and the askama-to-axum bridge.

use askama::Template;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::assets;
use crate::db::entry;
use crate::error::AppError;
use crate::state::AppState;
use crate::theme::Theme;
use crate::web::extract::CurrentUser;

/// Everything `base.html` needs, independent of which page is being rendered.
///
/// Built in one place so a page cannot forget to pass the theme, the stylesheet paths or —
/// the one that would actually be a bug — the CSRF token that every form needs.
pub struct Layout {
    /// Goes in `<title>`, before the product name.
    pub title: String,
    pub theme: Theme,
    pub favicon: String,
    pub stylesheets: Vec<String>,
    /// Linked only when signed in — `base.html` has nothing for them to act on before
    /// that, same as the keyboard shortcuts they implement.
    pub scripts: Vec<String>,
    /// Empty before sign-in, where there are no state-changing forms.
    pub csrf_token: String,
    pub signed_in: bool,
    /// Which navigation item is current. Matched by name in `base.html`.
    pub current: &'static str,
    pub unread_count: i64,
    /// The term in the nav search box. Empty except on the search results page itself,
    /// so the box shows what was searched for rather than going blank on every reload.
    pub search_query: String,
}

impl Layout {
    /// For pages rendered before there is a user row to read a preference from.
    pub fn anonymous(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            theme: Theme::System,
            favicon: assets::favicon(),
            stylesheets: assets::stylesheets(),
            scripts: Vec::new(),
            csrf_token: String::new(),
            signed_in: false,
            current: "",
            unread_count: 0,
            search_query: String::new(),
        }
    }

    /// For a signed-in page: the reader's theme, their CSRF token, and the unread count
    /// the navigation shows.
    pub async fn for_user(
        state: &AppState,
        current: &CurrentUser,
        title: impl Into<String>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            title: title.into(),
            theme: current.user.theme,
            favicon: assets::favicon(),
            stylesheets: assets::stylesheets(),
            scripts: assets::scripts(),
            csrf_token: current.session.csrf_token.clone(),
            signed_in: true,
            current: "",
            unread_count: entry::total_unread(state.db(), current.user.id).await?,
            search_query: String::new(),
        })
    }

    /// Marks a navigation item as the current page.
    pub fn at(mut self, current: &'static str) -> Self {
        self.current = current;
        self
    }

    /// Shows a term in the nav search box, for the search results page.
    pub fn searching(mut self, query: impl Into<String>) -> Self {
        self.search_query = query.into();
        self
    }
}

const HTML: &str = "text/html; charset=utf-8";

/// Renders an askama template into a response.
///
/// askama implements no `IntoResponse` of its own and `askama_axum` no longer exists, so
/// this is the bridge. The content type is fixed rather than taken from the template,
/// because askama 0.16's `Template` trait exposes no MIME constant and every template
/// Rustle has is HTML. A render failure is a bug in our own template, not bad input, so
/// it is logged and reported as a 500 rather than surfaced to the reader.
pub struct Html<T>(pub T);

impl<T: Template> IntoResponse for Html<T> {
    fn into_response(self) -> Response {
        match self.0.render() {
            Ok(body) => ([(header::CONTENT_TYPE, HTML)], body).into_response(),
            Err(err) => {
                tracing::error!("template render failed: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "Rustle could not render this page.",
                )
                    .into_response()
            }
        }
    }
}
