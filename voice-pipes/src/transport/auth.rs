//! Authentication traits and built-in authenticators.
//!
//! Provides a pluggable authentication gate that runs during the
//! transport handshake, **before** any pipeline resources are allocated.
//! Ships with [`NoopAuthenticator`] for development and
//! [`TokenAuthenticator`] for simple API-key validation.

use std::collections::HashMap;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::error::TransportError;

// ── Types ───────────────────────────────────────────────────

/// The identity of an authenticated user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthIdentity {
    /// Unique user identifier.
    pub user_id: String,
    /// Roles or scopes granted.
    pub roles: Vec<String>,
    /// Arbitrary metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// When this identity expires (if applicable).
    #[serde(skip)]
    pub expires_at: Option<Instant>,
}

/// Credentials extracted from the transport handshake.
#[derive(Debug, Clone)]
pub enum AuthCredentials {
    /// Bearer token (JWT, API key).
    Token(String),
    /// No credentials (anonymous / dev mode).
    Anonymous,
}

// ── Trait ────────────────────────────────────────────────────

/// Pluggable authenticator, checked during the transport handshake.
///
/// Implementations validate credentials and return an [`AuthIdentity`]
/// on success.  The identity is attached to the session metadata for
/// the duration of the connection.
#[async_trait]
pub trait Authenticator: Send + Sync + std::fmt::Debug {
    /// Validate credentials and return an identity.
    async fn authenticate(
        &self,
        credentials: &AuthCredentials,
    ) -> Result<AuthIdentity, TransportError>;
}

// ── No-op authenticator (dev mode) ──────────────────────────

/// Authenticator that accepts all connections.
///
/// Used in development or when authentication is disabled.
#[derive(Debug, Clone)]
pub struct NoopAuthenticator;

#[async_trait]
impl Authenticator for NoopAuthenticator {
    async fn authenticate(
        &self,
        _credentials: &AuthCredentials,
    ) -> Result<AuthIdentity, TransportError> {
        Ok(AuthIdentity {
            user_id: "anonymous".into(),
            roles: vec!["user".into()],
            metadata: HashMap::new(),
            expires_at: None,
        })
    }
}

// ── Token authenticator (simple API keys) ───────────────────

/// Authenticator that checks against a configured set of API keys.
///
/// Each key maps to a user ID.  Useful for server-to-server or
/// early-stage client authentication.
#[derive(Debug, Clone)]
pub struct TokenAuthenticator {
    /// Map from API key -> `user_id`.
    tokens: HashMap<String, String>,
}

impl TokenAuthenticator {
    /// Create a new token authenticator from a map of key -> `user_id`.
    pub fn new(tokens: HashMap<String, String>) -> Self {
        Self { tokens }
    }
}

#[async_trait]
impl Authenticator for TokenAuthenticator {
    async fn authenticate(
        &self,
        credentials: &AuthCredentials,
    ) -> Result<AuthIdentity, TransportError> {
        match credentials {
            AuthCredentials::Token(token) => {
                if let Some(user_id) = self.tokens.get(token) {
                    Ok(AuthIdentity {
                        user_id: user_id.clone(),
                        roles: vec!["user".into()],
                        metadata: HashMap::new(),
                        expires_at: None,
                    })
                } else {
                    Err(TransportError::AuthFailed("invalid token".into()))
                }
            }
            AuthCredentials::Anonymous => Err(TransportError::AuthFailed("token required".into())),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/auth.rs"]
mod tests;
