use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    config::{AuthConfig, Secret},
    error::AppError,
};

enum CredentialVerifier {
    Plaintext(Secret),
    Argon2Hash(String),
}

impl CredentialVerifier {
    fn from_config(auth: &AuthConfig) -> Option<Self> {
        let has_password = auth
            .password
            .as_ref()
            .is_some_and(|secret| !secret.expose().trim().is_empty());
        let has_hash = auth
            .password_hash
            .as_deref()
            .is_some_and(|hash| !hash.trim().is_empty());

        match (has_password, has_hash) {
            (true, false) => auth.password.clone().map(Self::Plaintext),
            (false, true) => auth.password_hash.clone().map(Self::Argon2Hash),
            _ => None,
        }
    }

    fn matches(&self, candidate: &[u8]) -> bool {
        match self {
            Self::Plaintext(secret) => constant_time_eq(secret.expose().as_bytes(), candidate),
            Self::Argon2Hash(encoded) => PasswordHash::new(encoded).is_ok_and(|parsed| {
                Argon2::default()
                    .verify_password(candidate, &parsed)
                    .is_ok()
            }),
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

pub struct Authenticator {
    username: String,
    verifier: CredentialVerifier,
}

impl Authenticator {
    pub fn new(auth: &AuthConfig) -> Result<Self, AppError> {
        let username = auth.username.trim().to_owned();
        if username.is_empty() {
            return Err(AppError::Config(
                "auth.username must not be empty".to_owned(),
            ));
        }
        let verifier = CredentialVerifier::from_config(auth).ok_or_else(|| {
            AppError::Config("auth requires exactly one of password or password_hash".to_owned())
        })?;
        Ok(Self { username, verifier })
    }

    pub fn verify(&self, username: Option<&str>, password: Option<&str>) -> bool {
        let (Some(candidate_username), Some(candidate_password)) = (username, password) else {
            return false;
        };
        constant_time_eq(candidate_username.as_bytes(), self.username.as_bytes())
            && self.verifier.matches(candidate_password.as_bytes())
    }
}

pub struct SessionStore {
    sessions: RwLock<HashMap<String, Instant>>,
    timeout: Duration,
}

impl SessionStore {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    pub async fn create(&self) -> String {
        let session_id = Uuid::new_v4().to_string();
        self.sessions
            .write()
            .await
            .insert(session_id.clone(), Instant::now());
        session_id
    }

    pub async fn verify(&self, session_id: Option<&str>) -> bool {
        let Some(session_id) = session_id else {
            return false;
        };
        let mut sessions = self.sessions.write().await;
        match sessions.get_mut(session_id) {
            Some(last_activity) => {
                if last_activity.elapsed() >= self.timeout {
                    sessions.remove(session_id);
                    false
                } else {
                    *last_activity = Instant::now();
                    true
                }
            }
            None => false,
        }
    }

    pub async fn revoke(&self, session_id: Option<&str>) {
        if let Some(session_id) = session_id {
            self.sessions.write().await.remove(session_id);
        }
    }

    pub async fn purge_expired(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let before = sessions.len();
        sessions.retain(|_, last_activity| last_activity.elapsed() < self.timeout);
        before - sessions.len()
    }

    pub async fn active_sessions(&self) -> usize {
        self.sessions.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};

    #[test]
    fn plaintext_credentials_are_verified_in_constant_time_path() {
        let config = AuthConfig {
            username: "admin".to_owned(),
            password: Some(Secret::from("s3cret!".to_owned())),
            password_hash: None,
            session_timeout_secs: 60,
        };
        let authenticator = Authenticator::new(&config).unwrap();

        assert!(authenticator.verify(Some("admin"), Some("s3cret!")));
        assert!(!authenticator.verify(Some("admin"), Some("wrong")));
        assert!(!authenticator.verify(Some("root"), Some("s3cret!")));
        assert!(!authenticator.verify(None, Some("s3cret!")));
        assert!(!authenticator.verify(Some("admin"), None));
    }

    #[test]
    fn argon2_hash_is_verified_without_plaintext_secret() {
        let salt = SaltString::generate(&mut OsRng);
        let encoded = Argon2::default()
            .hash_password(b"s3cret!", &salt)
            .unwrap()
            .to_string();

        let config = AuthConfig {
            username: "admin".to_owned(),
            password: None,
            password_hash: Some(encoded),
            session_timeout_secs: 60,
        };
        let authenticator = Authenticator::new(&config).unwrap();

        assert!(authenticator.verify(Some("admin"), Some("s3cret!")));
        assert!(!authenticator.verify(Some("admin"), Some("wrong")));
    }

    #[test]
    fn missing_or_ambiguous_credentials_are_rejected() {
        let neither = AuthConfig {
            username: "admin".to_owned(),
            password: None,
            password_hash: None,
            session_timeout_secs: 60,
        };
        assert!(Authenticator::new(&neither).is_err());

        let both = AuthConfig {
            username: "admin".to_owned(),
            password: Some(Secret::from("a".to_owned())),
            password_hash: Some("$argon2id$invalid".to_owned()),
            session_timeout_secs: 60,
        };
        assert!(Authenticator::new(&both).is_err());

        let empty_user = AuthConfig {
            username: "  ".to_owned(),
            password: Some(Secret::from("a".to_owned())),
            password_hash: None,
            session_timeout_secs: 60,
        };
        assert!(Authenticator::new(&empty_user).is_err());
    }

    #[tokio::test]
    async fn sessions_roundtrip_revoke_and_reject_unknown() {
        let store = SessionStore::new(60);
        let session_id = store.create().await;

        assert!(store.verify(Some(&session_id)).await);
        assert!(!store.verify(Some("unknown")).await);
        assert!(!store.verify(None).await);

        store.revoke(Some(&session_id)).await;
        assert!(!store.verify(Some(&session_id)).await);
    }

    #[tokio::test]
    async fn zero_timeout_sessions_expire_immediately() {
        let store = SessionStore::new(0);
        store.create().await;

        assert_eq!(store.purge_expired().await, 1);
        assert_eq!(store.active_sessions().await, 0);
    }
}
