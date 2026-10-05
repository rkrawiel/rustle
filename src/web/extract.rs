//! Extractors: who is asking, and is this form request genuine.

use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use serde::de::DeserializeOwned;

use crate::db::session::{Session, SessionToken};
use crate::db::{session, user};
use crate::state::AppState;
use crate::web::{cookie, csrf};

/// An authenticated request. Every page behind the login wall takes this, so forgetting
/// the check is not possible — there is no handler shape that reads a user without it.
pub struct CurrentUser {
    pub user: user::User,
    pub session: Session,
    pub token: SessionToken,
}

/// Where an unauthenticated request is sent. `/setup` when nobody exists yet, otherwise
/// the login form.
pub struct NotAuthenticated(Response);

impl IntoResponse for NotAuthenticated {
    fn into_response(self) -> Response {
        self.0
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = NotAuthenticated;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let redirect = |to: &str| NotAuthenticated(Redirect::to(to).into_response());

        let token = cookie::read(&parts.headers).ok_or_else(|| redirect("/login"))?;

        let session = session::load(state.db(), &token)
            .await
            .map_err(|err| {
                tracing::error!("loading session failed: {err}");
                NotAuthenticated(StatusCode::INTERNAL_SERVER_ERROR.into_response())
            })?
            .ok_or_else(|| redirect("/login"))?;

        // A session whose user has been deleted is not a session.
        let user = user::find_by_id(state.db(), session.user_id)
            .await
            .map_err(|err| {
                tracing::error!("loading user failed: {err}");
                NotAuthenticated(StatusCode::INTERNAL_SERVER_ERROR.into_response())
            })?
            .ok_or_else(|| redirect("/login"))?;

        Ok(Self {
            user,
            session,
            token,
        })
    }
}

/// A form body whose CSRF token has been checked against the session.
///
/// Validating here rather than in middleware means a state-changing handler cannot
/// accidentally skip the check: it either takes `CsrfForm<T>` and is protected, or it
/// takes `Form<T>` and does not compile against the router, because nothing constructs
/// `Form` for these routes.
pub struct CsrfForm<T>(pub T);

#[derive(Debug)]
pub struct CsrfRejection(csrf::Rejection);

impl IntoResponse for CsrfRejection {
    fn into_response(self) -> Response {
        tracing::warn!("rejected a state-changing request: {:?}", self.0);
        // 403 rather than a redirect: this is either an attack or a stale tab, and
        // silently replaying it would be worse than saying no.
        (
            StatusCode::FORBIDDEN,
            "This request could not be verified. Reload the page and try again.",
        )
            .into_response()
    }
}

impl<T: DeserializeOwned> FromRequest<AppState> for CsrfForm<T> {
    type Rejection = Response;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let (mut parts, body) = request.into_parts();

        if !csrf::same_site(&parts.headers, &state.config().base_url) {
            return Err(CsrfRejection(csrf::Rejection::CrossSite).into_response());
        }

        let current = CurrentUser::from_request_parts(&mut parts, state)
            .await
            .map_err(IntoResponse::into_response)?;

        // Two passes over the same bytes, not one combined struct: serde_urlencoded does
        // not support `#[serde(flatten)]`. Serde ignores unknown fields by default, so
        // each pass simply skips what the other one wants.
        let bytes = Bytes::from_request(Request::from_parts(parts, body), state)
            .await
            .map_err(IntoResponse::into_response)?;

        let submitted: Token = serde_urlencoded::from_bytes(&bytes)
            .map_err(|_| CsrfRejection(csrf::Rejection::TokenMissing).into_response())?;

        let Some(submitted) = submitted.csrf_token else {
            return Err(CsrfRejection(csrf::Rejection::TokenMissing).into_response());
        };
        if !csrf::tokens_match(&submitted, &current.session.csrf_token) {
            return Err(CsrfRejection(csrf::Rejection::TokenMismatch).into_response());
        }

        let value: T = serde_urlencoded::from_bytes(&bytes).map_err(|err| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("That form could not be read: {err}"),
            )
                .into_response()
        })?;

        Ok(Self(value))
    }
}

/// The first pass: only the token, everything else ignored.
#[derive(serde::Deserialize)]
struct Token {
    csrf_token: Option<String>,
}

/// For a form that carries nothing but its CSRF token, such as logout or a toggle whose
/// target is already in the path. `serde_urlencoded` has no unit-type deserialiser, so
/// this stands in for `CsrfForm<()>`.
#[derive(Debug, serde::Deserialize)]
pub struct NoFields {}
