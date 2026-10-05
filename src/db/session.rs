//! Server-side sessions.
//!
//! The cookie carries 32 random bytes, base64url-encoded. Only their SHA-256 digest is
//! stored, so a database dump is not a set of live sessions. The digest is a lookup key,
//! not a password, so a fast hash is the right choice — Argon2 would be pointless on
//! 256 bits of entropy.
//!
//! Each session also carries its CSRF token, which is what makes the synchroniser-token
//! pattern work without any extra signing key.
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rand::RngExt;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::time::Duration;
/// 256 bits, which is well past anything guessable.
const TOKEN_BYTES: usize = 32;
/// What the cookie holds. Kept distinct from the stored digest so the two cannot be
/// confused at a call site.
#[derive(Debug, Clone)]
pub struct SessionToken(String);
impl SessionToken {
    /// rand 0.10 dropped `rngs::OsRng`; `rand::rng()` is the thread-local ChaCha CSPRNG,
    /// seeded and periodically reseeded from the operating system, and infallible.
    pub fn generate() -> Self {
        let bytes: [u8; TOKEN_BYTES] = rand::rng().random();
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }
    /// Accepts whatever arrived in the cookie. Nothing is trusted about it beyond being
    /// used as a lookup key.
    pub fn from_cookie(value: &str) -> Self {
        Self(value.to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    fn digest(&self) -> Vec<u8> {
        Sha256::digest(self.0.as_bytes()).to_vec()
    }
}
#[derive(Debug, Clone)]
pub struct Session {
    pub user_id: i64,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}
/// Issues a session and returns the token to put in the cookie. The token is deliberately
/// the only thing that ever leaves this function unhashed.
pub async fn create(
    pool: &PgPool,
    user_id: i64,
    ttl: Duration,
) -> Result<(SessionToken, Session), sqlx::Error> {
    let token = SessionToken::generate();
    let csrf = SessionToken::generate();
    let expires_at =
        Utc::now() + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::days(30));
    sqlx::query!(
        "insert into sessions (id_hash, user_id, csrf_token, expires_at)
         values ($1, $2, $3, $4)",
        token.digest(),
        user_id,
        csrf.as_str(),
        expires_at,
    )
    .execute(pool)
    .await?;
    Ok((
        token,
        Session {
            user_id,
            csrf_token: csrf.0,
            expires_at,
        },
    ))
}
/// Loads a live session, refreshing `last_seen_at`. Expired rows are treated as absent;
/// the cleanup task deletes them.
pub async fn load(pool: &PgPool, token: &SessionToken) -> Result<Option<Session>, sqlx::Error> {
    let row = sqlx::query!(
        "update sessions
         set last_seen_at = now()
         where id_hash = $1 and expires_at > now()
         returning user_id, csrf_token, expires_at",
        token.digest(),
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| Session {
        user_id: row.user_id,
        csrf_token: row.csrf_token,
        expires_at: row.expires_at,
    }))
}
pub async fn delete(pool: &PgPool, token: &SessionToken) -> Result<(), sqlx::Error> {
    sqlx::query!("delete from sessions where id_hash = $1", token.digest())
        .execute(pool)
        .await?;
    Ok(())
}
/// Ends every session except the one given. Called on a password change, so that a
/// stolen session cannot survive the owner reacting to the theft.
pub async fn delete_others(
    pool: &PgPool,
    user_id: i64,
    keep: &SessionToken,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "delete from sessions where user_id = $1 and id_hash <> $2",
        user_id,
        keep.digest(),
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
/// Housekeeping for the background task.
pub async fn delete_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!("delete from sessions where expires_at <= now()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_tokens_are_long_and_unique() {
        let a = SessionToken::generate();
        let b = SessionToken::generate();
        assert_ne!(a.as_str(), b.as_str());
        // 32 bytes base64url without padding.
        assert_eq!(a.as_str().len(), 43);
    }
    #[test]
    fn the_cookie_value_is_never_what_gets_stored() {
        let token = SessionToken::generate();
        assert_ne!(token.digest(), token.as_str().as_bytes());
        assert_eq!(token.digest().len(), 32);
        // Same token, same key: the digest has to be stable to be a usable lookup.
        assert_eq!(
            token.digest(),
            SessionToken::from_cookie(token.as_str()).digest()
        );
    }
}
