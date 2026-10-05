//! Setup, sign in, sign out and change password.

use askama::Template;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router, routing};
use serde::Deserialize;

use crate::db::{session, user};
use crate::error::AppError;
use crate::password::{self, PasswordError};
use crate::state::AppState;
use crate::web::extract::{CsrfForm, CurrentUser, NoFields};
use crate::web::layout::{Html, Layout};
use crate::web::{cookie, csrf};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/setup", routing::get(setup_form).post(setup_submit))
        .route("/login", routing::get(login_form).post(login_submit))
        .route("/logout", routing::post(logout))
        .route("/settings/password", routing::post(change_password))
}

// ---------------------------------------------------------------- setup

#[derive(Template)]
#[template(path = "setup.html")]
struct SetupPage {
    layout: Layout,
    error: Option<String>,
    username: String,
    setup_token: String,
    min_password_length: usize,
}

impl SetupPage {
    fn blank() -> Self {
        Self {
            layout: Layout::anonymous("Set up"),
            error: None,
            username: String::new(),
            setup_token: String::new(),
            min_password_length: password::MIN_LENGTH,
        }
    }
}

/// `/setup` exists only until there is a user, and then returns 404 permanently rather
/// than redirecting: a route that no longer exists should say so.
async fn require_no_user(state: &AppState) -> Result<(), AppError> {
    if user::exists(state.db()).await? {
        return Err(AppError::NotFound);
    }
    Ok(())
}

async fn setup_form(State(state): State<AppState>) -> Result<Response, AppError> {
    require_no_user(&state).await?;
    Ok(Html(SetupPage::blank()).into_response())
}

#[derive(Deserialize)]
struct SetupSubmission {
    setup_token: String,
    username: String,
    password: String,
}

/// Not a `CsrfForm`: there is no session yet to hold a token. The setup token plays that
/// role — it is an unguessable secret that must be submitted with the form — and the
/// `Sec-Fetch-Site` check still applies.
async fn setup_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SetupSubmission>,
) -> Result<Response, AppError> {
    require_no_user(&state).await?;

    if !csrf::same_site(&headers, &state.config().base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let reject = |message: &str, form: &SetupSubmission| {
        Html(SetupPage {
            error: Some(message.to_owned()),
            username: form.username.clone(),
            setup_token: form.setup_token.clone(),
            ..SetupPage::blank()
        })
        .into_response()
    };

    if !csrf::tokens_match(form.setup_token.trim(), state.setup_token()) {
        tracing::warn!("rejected a setup attempt with a wrong token");
        return Ok(reject(
            "That setup token is not correct. It is printed in the server log.",
            &form,
        ));
    }

    let username = form.username.trim();
    if username.is_empty() {
        return Ok(reject("Pick a username.", &form));
    }

    let password_hash = match state.hasher().hash(form.password.clone()).await {
        Ok(hash) => hash,
        Err(PasswordError::TooShort) => {
            return Ok(reject(
                &format!(
                    "That password is too short. Use at least {} characters.",
                    password::MIN_LENGTH
                ),
                &form,
            ));
        }
        Err(err) => return Err(AppError::Internal(err.to_string())),
    };

    let retention_days = i32::try_from(state.config().retention_days).unwrap_or(i32::MAX);
    let created = user::create(state.db(), username, &password_hash, retention_days).await?;
    tracing::info!("created user {:?}", created.username);

    // Signed in straight away: having just proved both the setup token and the password,
    // another login form would be theatre.
    start_session(&state, created.id).await
}

// ---------------------------------------------------------------- login

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    layout: Layout,
    error: Option<String>,
    username: String,
}

impl LoginPage {
    fn blank() -> Self {
        Self {
            layout: Layout::anonymous("Sign in"),
            error: None,
            username: String::new(),
        }
    }
}

async fn login_form(State(state): State<AppState>) -> Result<Response, AppError> {
    // Nobody to sign in as yet: send them to setup instead of an unusable form.
    if !user::exists(state.db()).await? {
        return Ok(Redirect::to("/setup").into_response());
    }
    Ok(Html(LoginPage::blank()).into_response())
}

#[derive(Deserialize)]
struct LoginSubmission {
    username: String,
    password: String,
}

async fn login_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LoginSubmission>,
) -> Result<Response, AppError> {
    if !csrf::same_site(&headers, &state.config().base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let username = form.username.trim().to_owned();

    // The same message for a wrong name and a wrong password, so neither can be probed.
    let deny = |message: &str| {
        Html(LoginPage {
            error: Some(message.to_owned()),
            username: username.clone(),
            ..LoginPage::blank()
        })
        .into_response()
    };

    // Before hashing, so a flood of guesses cannot spend 19 MiB and a core each.
    if !state.login_limiter().allow(&username) {
        tracing::warn!("rate limited login attempts for {username:?}");
        return Ok(deny(
            "Too many sign-in attempts. Wait a few minutes and try again.",
        ));
    }

    let Some(account) = user::find_by_username(state.db(), &username).await? else {
        // Burn the same CPU time as a real verification, or the response time tells an
        // attacker which usernames exist.
        state.hasher().verify_dummy(form.password).await;
        return Ok(deny("That username and password do not match."));
    };

    let verified = state
        .hasher()
        .verify(form.password, account.password_hash.clone())
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?;

    if !verified {
        return Ok(deny("That username and password do not match."));
    }

    state.login_limiter().reset(&username);
    start_session(&state, account.id).await
}

// ---------------------------------------------------------------- logout

async fn logout(
    State(state): State<AppState>,
    current: CurrentUser,
    _form: CsrfForm<NoFields>,
) -> Result<Response, AppError> {
    session::delete(state.db(), &current.token).await?;
    Ok((
        [(header::SET_COOKIE, cookie::clear(state.config()))],
        Redirect::to("/login"),
    )
        .into_response())
}

// ---------------------------------------------------------------- change password

#[derive(Deserialize)]
struct PasswordChange {
    current_password: String,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    current: CurrentUser,
    CsrfForm(form): CsrfForm<PasswordChange>,
) -> Result<Response, AppError> {
    let verified = state
        .hasher()
        .verify(form.current_password, current.user.password_hash.clone())
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?;

    if !verified {
        return Err(AppError::BadRequest(
            "Your current password is not correct.".to_owned(),
        ));
    }

    let hash = match state.hasher().hash(form.new_password).await {
        Ok(hash) => hash,
        Err(PasswordError::TooShort) => {
            return Err(AppError::BadRequest(format!(
                "That password is too short. Use at least {} characters.",
                password::MIN_LENGTH
            )));
        }
        Err(err) => return Err(AppError::Internal(err.to_string())),
    };

    user::set_password_hash(state.db(), current.user.id, &hash).await?;

    // The whole point of changing a password is often that someone else has it. Leaving
    // their session alive would defeat the exercise.
    let ended = session::delete_others(state.db(), current.user.id, &current.token).await?;
    tracing::info!("password changed, ended {ended} other session(s)");

    Ok(Redirect::to("/settings").into_response())
}

// ---------------------------------------------------------------- shared

/// Issues a session and redirects to the reader. The session id is new every time, which
/// is what prevents session fixation.
async fn start_session(state: &AppState, user_id: i64) -> Result<Response, AppError> {
    let (token, _session) =
        session::create(state.db(), user_id, state.config().session_ttl).await?;

    Ok((
        [(header::SET_COOKIE, cookie::set(state.config(), &token))],
        Redirect::to("/unread"),
    )
        .into_response())
}
