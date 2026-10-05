//! Password hashing with Argon2id.
//!
//! Two things are deliberate here. The work happens on a blocking thread, because Argon2
//! is CPU-bound by design and would otherwise stall the async runtime. And it is gated
//! behind a one-permit semaphore: each hash allocates 19 MiB, so unbounded concurrent
//! logins would both peg a core and blow straight through the 30 MB memory budget. One
//! permit is right for a single-user reader, where two simultaneous logins are already
//! a surprise.

use std::sync::Arc;

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use tokio::sync::Semaphore;

/// OWASP's second recommended Argon2id profile: 19 MiB, two passes, one lane.
const MEMORY_KIB: u32 = 19 * 1024;
const ITERATIONS: u32 = 2;
const PARALLELISM: u32 = 1;

/// The shortest password Rustle will accept. Length is the only rule: composition rules
/// push people towards `Password1!` and are worse than useless.
pub const MIN_LENGTH: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password must be at least {MIN_LENGTH} characters")]
    TooShort,
    /// Hashing or verification itself failed, which means a corrupt stored hash or an
    /// exhausted machine, never a wrong password.
    #[error("password hashing failed: {0}")]
    Hashing(String),
}

/// Serialises hashing across the process. Cloned into `AppState`.
#[derive(Clone)]
pub struct Hasher {
    permit: Arc<Semaphore>,
}

impl Default for Hasher {
    fn default() -> Self {
        Self {
            permit: Arc::new(Semaphore::new(1)),
        }
    }
}

impl Hasher {
    pub async fn hash(&self, password: String) -> Result<String, PasswordError> {
        if password.chars().count() < MIN_LENGTH {
            return Err(PasswordError::TooShort);
        }
        self.run(move || hash_blocking(&password)).await
    }

    /// Returns `Ok(false)` for a wrong password, and an error only when the stored hash
    /// cannot be parsed.
    pub async fn verify(&self, password: String, hash: String) -> Result<bool, PasswordError> {
        self.run(move || verify_blocking(&password, &hash)).await
    }

    /// Verifies against a throwaway hash so that an unknown username costs the same as a
    /// known one. Without this, response time tells an attacker which names exist.
    pub async fn verify_dummy(&self, password: String) {
        let _ = self.verify(password, dummy_hash()).await;
    }

    async fn run<T, F>(&self, work: F) -> Result<T, PasswordError>
    where
        F: FnOnce() -> Result<T, PasswordError> + Send + 'static,
        T: Send + 'static,
    {
        let permit = Arc::clone(&self.permit)
            .acquire_owned()
            .await
            .map_err(|_| PasswordError::Hashing("hasher is shut down".to_owned()))?;

        tokio::task::spawn_blocking(move || {
            let result = work();
            drop(permit);
            result
        })
        .await
        .map_err(|err| PasswordError::Hashing(err.to_string()))?
    }
}

fn argon2() -> Argon2<'static> {
    let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, None)
        .expect("the compiled-in Argon2 parameters are valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

fn hash_blocking(password: &str) -> Result<String, PasswordError> {
    // password-hash 0.6 generates a recommended-length random salt itself; the parameters
    // come from the configured `Argon2` instance.
    let hash: PasswordHash = argon2()
        .hash_password(password.as_bytes())
        .map_err(|err| PasswordError::Hashing(err.to_string()))?;
    Ok(hash.to_string())
}

fn verify_blocking(password: &str, hash: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(hash).map_err(|err| PasswordError::Hashing(err.to_string()))?;
    Ok(argon2()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// A fixed hash of an unguessable string, used only to burn the same CPU time as a real
/// verification. Computed once, since it is a constant.
fn dummy_hash() -> String {
    use std::sync::LazyLock;
    static DUMMY: LazyLock<String> = LazyLock::new(|| {
        hash_blocking("rustle-timing-equalisation-placeholder")
            .expect("hashing a fixed string cannot fail")
    });
    DUMMY.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_password_verifies_against_its_own_hash() {
        let hasher = Hasher::default();
        let hash = hasher
            .hash("correct horse battery".to_owned())
            .await
            .unwrap();

        assert!(
            hasher
                .verify("correct horse battery".to_owned(), hash.clone())
                .await
                .unwrap()
        );
        assert!(
            !hasher
                .verify("Correct horse battery".to_owned(), hash)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn hashes_are_salted_so_the_same_password_hashes_differently() {
        let hasher = Hasher::default();
        let first = hasher
            .hash("correct horse battery".to_owned())
            .await
            .unwrap();
        let second = hasher
            .hash("correct horse battery".to_owned())
            .await
            .unwrap();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn the_stored_hash_records_argon2id_and_our_parameters() {
        let hash = Hasher::default()
            .hash("correct horse battery".to_owned())
            .await
            .unwrap();
        assert!(hash.starts_with("$argon2id$v=19$"), "{hash}");
        assert!(hash.contains(&format!("m={MEMORY_KIB}")), "{hash}");
        assert!(hash.contains(&format!("t={ITERATIONS}")), "{hash}");
        assert!(hash.contains(&format!("p={PARALLELISM}")), "{hash}");
    }

    #[tokio::test]
    async fn short_passwords_are_refused_before_any_hashing() {
        let err = Hasher::default()
            .hash("short".to_owned())
            .await
            .expect_err("should be refused");
        assert!(matches!(err, PasswordError::TooShort));
    }

    #[tokio::test]
    async fn length_is_counted_in_characters_not_bytes() {
        // Twelve characters, well over twelve bytes.
        let hasher = Hasher::default();
        assert!(
            hasher
                .hash("日本語日本語日本語日本語".to_owned())
                .await
                .is_ok()
        );
        assert!(
            hasher
                .hash("日本語日本語日本語日本".to_owned())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_corrupt_stored_hash_is_an_error_not_a_silent_rejection() {
        let err = Hasher::default()
            .verify("correct horse battery".to_owned(), "not-a-hash".to_owned())
            .await
            .expect_err("a malformed hash should not look like a wrong password");
        assert!(matches!(err, PasswordError::Hashing(_)));
    }

    #[tokio::test]
    async fn the_dummy_verification_completes_without_panicking() {
        Hasher::default().verify_dummy("anything".to_owned()).await;
    }
}
