//! The one error type handlers return, and how it becomes a response.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::web::layout::{Html, Layout};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    /// Something was wrong with what the reader submitted, in a way worth naming.
    #[error("{0}")]
    BadRequest(String),

    #[error(transparent)]
    Database(#[from] sqlx::Error),

    /// A failure that is ours, not the reader's: a corrupt password hash, an exhausted
    /// thread pool. The message goes to the log, never to the page.
    #[error("{0}")]
    Internal(String),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// What the reader is told. Internal failures deliberately say nothing specific —
    /// the detail goes to the log instead.
    fn detail(&self) -> String {
        match self {
            Self::NotFound => "That page does not exist.".to_owned(),
            Self::BadRequest(message) => message.clone(),
            Self::Database(_) | Self::Internal(_) => {
                "Rustle hit an internal error. The details are in the server log.".to_owned()
            }
        }
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    layout: Layout,
    heading: String,
    detail: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();

        if status.is_server_error() {
            tracing::error!("{self:?}");
        }

        let heading = match status {
            StatusCode::NOT_FOUND => "Not found".to_owned(),
            _ => status
                .canonical_reason()
                .unwrap_or("Something went wrong")
                .to_owned(),
        };

        let page = ErrorPage {
            layout: Layout::anonymous(heading.clone()),
            heading,
            detail: self.detail(),
        };

        (status, Html(page)).into_response()
    }
}
