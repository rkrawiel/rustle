//! Setup, sign in, sign out, CSRF and rate limiting.

mod support;

use axum::http::{StatusCode, header};
use sqlx::PgPool;
use support::{
    app_with_state, assert_redirects_to, body_string, csrf_token, get, get_as, post_form,
    post_form_as, session_cookie, sign_up,
};

const PASSWORD: &str = "correct+horse+battery";

#[sqlx::test]
async fn setup_offers_a_form_while_there_is_no_user(pool: PgPool) {
    let (router, state) = app_with_state(pool);

    let body = body_string(get(router, "/setup").await).await;

    assert!(body.contains("Set up Rustle"));
    assert!(body.contains(r#"name="setup_token""#));
    // The token must never reach the page: anyone who can load /setup would then have it,
    // which is the whole thing the token exists to prevent.
    assert!(
        !body.contains(state.setup_token()),
        "the setup token leaked into the form"
    );
}

#[sqlx::test]
async fn setup_refuses_a_wrong_token_without_creating_anything(pool: PgPool) {
    let (router, _) = app_with_state(pool.clone());

    let response = post_form(
        router,
        "/setup",
        &format!("setup_token=guessed&username=reader&password={PASSWORD}"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK, "the form is re-rendered");
    assert!(
        body_string(response)
            .await
            .contains("setup token is not correct")
    );

    let users: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[sqlx::test]
async fn setup_creates_the_user_and_signs_them_in(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());

    let response = post_form(
        router,
        "/setup",
        &format!(
            "setup_token={}&username=+reader+&password={PASSWORD}",
            state.setup_token()
        ),
    )
    .await;

    assert_redirects_to(&response, "/unread");
    let cookie = session_cookie(&response).expect("a session cookie should be set");
    assert!(cookie.starts_with("rustle_session="));

    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Lax"));
    // The default base URL is http://localhost, where a Secure cookie would never be sent.
    assert!(!set_cookie.contains("Secure"));

    let username: String = sqlx::query_scalar("select username from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(username, "reader", "the username should be trimmed");
}

#[sqlx::test]
async fn setup_rejects_a_password_under_the_minimum(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());

    let response = post_form(
        router,
        "/setup",
        &format!(
            "setup_token={}&username=reader&password=short",
            state.setup_token()
        ),
    )
    .await;

    assert!(body_string(response).await.contains("too short"));
    let users: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[sqlx::test]
async fn setup_is_gone_for_good_once_a_user_exists(pool: PgPool) {
    let (router, state) = app_with_state(pool);
    sign_up(&router, state.setup_token()).await;

    assert_eq!(
        get(router.clone(), "/setup").await.status(),
        StatusCode::NOT_FOUND
    );

    let second = post_form(
        router,
        "/setup",
        &format!(
            "setup_token={}&username=intruder&password={PASSWORD}",
            state.setup_token()
        ),
    )
    .await;
    assert_eq!(second.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_cross_site_post_to_setup_is_forbidden(pool: PgPool) {
    let (router, state) = app_with_state(pool);

    let response = support::post_cross_site(
        router,
        "/setup",
        &format!(
            "setup_token={}&username=reader&password={PASSWORD}",
            state.setup_token()
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn login_redirects_to_setup_while_there_is_no_user(pool: PgPool) {
    let (router, _) = app_with_state(pool);
    assert_redirects_to(&get(router, "/login").await, "/setup");
}

#[sqlx::test]
async fn a_correct_password_starts_a_new_session(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let first = sign_up(&router, state.setup_token()).await;

    let response = post_form(
        router,
        "/login",
        &format!("username=reader&password={PASSWORD}"),
    )
    .await;

    assert_redirects_to(&response, "/unread");
    let second = session_cookie(&response).expect("login should set a cookie");
    // A fresh id every time is what prevents session fixation.
    assert_ne!(first, second);
}

#[sqlx::test]
async fn a_wrong_password_and_an_unknown_user_are_indistinguishable(pool: PgPool) {
    let (router, state) = app_with_state(pool);
    sign_up(&router, state.setup_token()).await;

    let wrong_password = post_form(
        router.clone(),
        "/login",
        "username=reader&password=wrong+horse+battery",
    )
    .await;
    let unknown_user = post_form(
        router,
        "/login",
        &format!("username=nobody&password={PASSWORD}"),
    )
    .await;

    assert!(session_cookie(&wrong_password).is_none());
    assert!(session_cookie(&unknown_user).is_none());

    // Same wording either way, or the page tells an attacker which names exist. Compared
    // on the status message alone, since the rest of the page echoes the typed username.
    let first = status_message(&body_string(wrong_password).await);
    let second = status_message(&body_string(unknown_user).await);
    assert!(first.contains("do not match"), "{first}");
    assert_eq!(first, second);
}

/// The text of the page's status message, with tags and whitespace collapsed.
fn status_message(html: &str) -> String {
    let (_, after) = html
        .split_once(r#"class="status-label""#)
        .expect("the page should carry a status message");
    let (message, _) = after.split_once("</p>").expect("an unterminated status");
    message
        .replace('>', " ")
        .replace("</span", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[sqlx::test]
async fn login_attempts_are_rate_limited_before_any_hashing(pool: PgPool) {
    let (router, state) = app_with_state(pool);
    sign_up(&router, state.setup_token()).await;

    let mut limited = None;
    for attempt in 1..=12 {
        let response = post_form(
            router.clone(),
            "/login",
            "username=reader&password=nope+nope+nope",
        )
        .await;
        let body = body_string(response).await;
        if body.contains("Too many sign-in attempts") {
            limited = Some(attempt);
            break;
        }
    }

    assert_eq!(limited, Some(11), "the 11th attempt should be refused");

    // Even the correct password is refused while the window is open, which is the point.
    let correct = post_form(
        router,
        "/login",
        &format!("username=reader&password={PASSWORD}"),
    )
    .await;
    assert!(session_cookie(&correct).is_none());
}

#[sqlx::test]
async fn pages_behind_the_login_wall_redirect_when_there_is_no_session(pool: PgPool) {
    let (router, _) = app_with_state(pool);

    // Logout is a state-changing route, so it goes through the authenticated path.
    let response = post_form(router, "/logout", "csrf_token=anything").await;
    assert_redirects_to(&response, "/login");
}

#[sqlx::test]
async fn a_forged_session_cookie_is_not_a_session(pool: PgPool) {
    let (router, state) = app_with_state(pool);
    sign_up(&router, state.setup_token()).await;

    let response = post_form_as(
        router,
        "/logout",
        "rustle_session=not-a-real-token",
        "csrf_token=anything",
    )
    .await;

    assert_redirects_to(&response, "/login");
}

#[sqlx::test]
async fn a_state_changing_post_without_a_csrf_token_is_forbidden(pool: PgPool) {
    let (router, state) = app_with_state(pool);
    let cookie = sign_up(&router, state.setup_token()).await;

    let response = post_form_as(router, "/logout", &cookie, "unrelated=1").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn a_state_changing_post_with_the_wrong_csrf_token_is_forbidden(pool: PgPool) {
    let (router, state) = app_with_state(pool);
    let cookie = sign_up(&router, state.setup_token()).await;

    let response = post_form_as(
        router,
        "/logout",
        &cookie,
        "csrf_token=borrowed-from-elsewhere",
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn a_cross_site_post_is_forbidden_even_with_the_right_token(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;

    let response =
        support::post_cross_site_as(router, "/logout", &cookie, &format!("csrf_token={token}"))
            .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn logout_clears_the_cookie_and_the_session_row(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;

    let response = post_form_as(router, "/logout", &cookie, &format!("csrf_token={token}")).await;

    assert_redirects_to(&response, "/login");
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(set_cookie.contains("Max-Age=0"));

    let sessions: i64 = sqlx::query_scalar("select count(*) from sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

#[sqlx::test]
async fn changing_the_password_ends_every_other_session(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let keep = sign_up(&router, state.setup_token()).await;

    // A second sign-in, standing in for a session someone else holds.
    let other = post_form(
        router.clone(),
        "/login",
        &format!("username=reader&password={PASSWORD}"),
    )
    .await;
    let other = session_cookie(&other).unwrap();
    assert_ne!(keep, other);

    let token: String =
        sqlx::query_scalar("select csrf_token from sessions order by created_at limit 1")
            .fetch_one(&pool)
            .await
            .unwrap();

    let response = post_form_as(
        router.clone(),
        "/settings/password",
        &keep,
        &format!(
            "csrf_token={token}&current_password={PASSWORD}&new_password=a+whole+new+passphrase"
        ),
    )
    .await;
    assert_redirects_to(&response, "/settings");

    let remaining: i64 = sqlx::query_scalar("select count(*) from sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1, "only the session that changed it survives");

    // The other holder is logged out.
    assert_redirects_to(
        &post_form_as(router.clone(), "/logout", &other, "csrf_token=x").await,
        "/login",
    );

    // And the new password works.
    let signed_in = post_form(
        router,
        "/login",
        "username=reader&password=a+whole+new+passphrase",
    )
    .await;
    assert!(session_cookie(&signed_in).is_some());
}

#[sqlx::test]
async fn changing_the_password_requires_the_current_one(pool: PgPool) {
    let (router, state) = app_with_state(pool.clone());
    let cookie = sign_up(&router, state.setup_token()).await;
    let token = csrf_token(&pool).await;

    let response = post_form_as(
        router,
        "/settings/password",
        &cookie,
        &format!("csrf_token={token}&current_password=wrong+horse+battery&new_password=a+whole+new+passphrase"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body_string(response)
            .await
            .contains("current password is not correct")
    );
}

#[sqlx::test]
async fn a_signed_in_reader_keeps_their_session_across_requests(pool: PgPool) {
    let (router, state) = app_with_state(pool);
    let cookie = sign_up(&router, state.setup_token()).await;

    // /setup is 404 for everyone now, which proves the request was served rather than
    // bounced to the login form.
    let response = get_as(router, "/setup", &cookie).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
