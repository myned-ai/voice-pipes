//! Tenant identity and scoping primitives.
//!
//! Multi-tenancy in Voice Pipes namespaces sessions, quotas, and audit captures
//! by a validated [`TenantId`].

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Maximum allowed byte length for a tenant identifier.
pub const MAX_TENANT_ID_LEN: usize = 64;

/// Default retention window for tenant captures (7 days).
pub const DEFAULT_TENANT_RETENTION_DAYS: u32 = 7;

/// Default capture budget per session (512 MiB).
pub const DEFAULT_TENANT_MAX_SESSION_BYTES: u64 = 512 * 1024 * 1024;

/// Error returned when validating or parsing a [`TenantId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TenantIdError {
    /// The tenant identifier was empty or contained only whitespace.
    #[error("tenant id cannot be empty")]
    Empty,
    /// The tenant identifier exceeds the maximum allowed length.
    #[error("tenant id exceeds maximum length of {max} bytes (was {actual})")]
    TooLong {
        /// Maximum allowed bytes.
        max: usize,
        /// Actual length provided.
        actual: usize,
    },
    /// The tenant identifier contains characters outside `^[A-Za-z0-9][A-Za-z0-9_-]*$`.
    #[error("tenant id '{0}' contains invalid characters (must match ^[A-Za-z0-9][A-Za-z0-9_-]*$)")]
    InvalidCharacters(String),
}

/// A validated, path-safe tenant identifier.
///
/// Guaranteed to be safe for use directly in filesystem paths (denies traversal
/// sequences such as `..`, `/`, `\`, and null bytes) and HTTP headers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    /// Create a new `TenantId` after validating its format and length.
    pub fn new(id: impl Into<String>) -> Result<Self, TenantIdError> {
        let s = id.into();
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(TenantIdError::Empty);
        }
        if trimmed.len() > MAX_TENANT_ID_LEN {
            return Err(TenantIdError::TooLong {
                max: MAX_TENANT_ID_LEN,
                actual: trimmed.len(),
            });
        }
        let mut chars = trimmed.chars();
        let Some(first) = chars.next() else {
            return Err(TenantIdError::Empty);
        };
        if !first.is_ascii_alphanumeric() {
            return Err(TenantIdError::InvalidCharacters(trimmed.to_string()));
        }
        for c in chars {
            if !c.is_ascii_alphanumeric() && c != '-' && c != '_' {
                return Err(TenantIdError::InvalidCharacters(trimmed.to_string()));
            }
        }
        Ok(Self(trimmed.to_string()))
    }

    /// Access the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for TenantId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for TenantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for TenantId {
    type Err = TenantIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for TenantId {
    type Error = TenantIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<TenantId> for String {
    fn from(id: TenantId) -> Self {
        id.0
    }
}

fn default_retention_days() -> u32 {
    DEFAULT_TENANT_RETENTION_DAYS
}

fn default_max_session_bytes() -> u64 {
    DEFAULT_TENANT_MAX_SESSION_BYTES
}

/// Retention policy and privacy controls for a tenant's captures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantRetentionPolicy {
    /// Retention window in days. `0` disables time-based eviction.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    /// Maximum disk budget in bytes for one session's captures.
    #[serde(default = "default_max_session_bytes")]
    pub max_session_bytes: u64,
    /// Zero Data Retention (ZDR) mode. When `true`, raw microphone and synthesized
    /// audio fragments are dropped immediately after inference and never written to disk.
    #[serde(default)]
    pub zero_data_retention: bool,
}

impl Default for TenantRetentionPolicy {
    fn default() -> Self {
        Self {
            retention_days: DEFAULT_TENANT_RETENTION_DAYS,
            max_session_bytes: DEFAULT_TENANT_MAX_SESSION_BYTES,
            zero_data_retention: false,
        }
    }
}

#[cfg(test)]
#[path = "tests/tenant.rs"]
mod tests;
