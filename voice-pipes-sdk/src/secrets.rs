//! Credential detection and redaction shared by everything that lets a
//! config, a provider frame or an option map leave the process: the
//! worker registration payload, the debug and orchestrator config
//! endpoints, the trace-level provider frame log, and the parameter
//! schema the admin console renders.
//!
//! One key test decides what is a secret, so the surfaces cannot drift
//! apart; one walker applies it, so nesting, arrays and URLs are handled
//! the same way everywhere. A credential a provider holds is a [`Secret`],
//! which cannot be printed or serialised by accident.

use std::fmt;

use serde_json::Value;
use zeroize::Zeroizing;

/// Placeholder written wherever a credential would otherwise appear.
pub const REDACTED: &str = "***REDACTED***";

/// A credential: an API key, a token, a password.
///
/// `Debug` and `Serialize` write [`REDACTED`] (an empty secret serialises
/// as `""`, so "not set" stays distinguishable), there is deliberately no
/// `Display`, and the value is wiped from memory when the last copy is
/// dropped. The one way to read it is [`Secret::expose`], at the call that
/// sends it:
///
/// ```
/// use voice_pipes_sdk::secrets::Secret;
///
/// let key = Secret::from("sk-live-123");
/// assert_eq!(format!("{key:?}"), "Secret(***REDACTED***)");
/// assert_eq!(key.expose(), "sk-live-123");
/// ```
#[derive(Clone, Default)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wrap a credential.
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// The credential itself. Call it only where the value leaves for the
    /// service that needs it (a header, a connection string).
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the credential is the empty string.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({REDACTED})")
    }
}

/// Two secrets are equal when their values are, so a type holding one can
/// still derive `PartialEq`.
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

impl Eq for Secret {}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

impl serde::Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(if self.is_empty() { "" } else { REDACTED })
    }
}

/// The credential to send: `key` when configured and not blank, else
/// `env_var`'s value, looked up through `env`, when set and not blank.
/// `None` when neither. For the host, which resolves every key before a
/// provider sees its config; `env` is the process environment there and a
/// map in a test.
pub fn resolve_key(
    key: Option<&Secret>,
    env_var: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<Secret> {
    key.filter(|k| !k.expose().trim().is_empty())
        .cloned()
        .or_else(|| {
            env(env_var?)
                .filter(|v| !v.trim().is_empty())
                .map(Secret::from)
        })
}

/// The configured credential when it is not blank: what a provider sends,
/// once the host has resolved it.
pub fn configured_key(key: Option<&Secret>) -> Option<&Secret> {
    key.filter(|k| !k.expose().trim().is_empty())
}

/// `options` as `Debug` shows them: secret-named values replaced by
/// [`REDACTED`]. For the hand-written `Debug` of config types whose option
/// maps can carry a credential.
pub fn debug_options<S: std::hash::BuildHasher>(
    options: &std::collections::HashMap<String, Value, S>,
) -> Value {
    redacted(
        &Value::Object(
            options
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        REDACTED,
    )
}

/// Substrings of a (lower-cased, `-` folded to `_`) key that mark it as
/// a credential wherever they appear.
const CONTAINS_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "secret",
    "password",
    "passwd",
    "authorization",
    "bearer",
    "credential",
    "private_key",
];

/// True when a key looks like it holds a credential.
///
/// Matching is case-insensitive and treats `-` like `_`, so `x-api-key`,
/// `apiKey`, `Authorization` and `client_secret` all hit. `token`
/// matches alone or as a `_token` suffix, so `max_tokens` and
/// `tokens_per_sec` stay visible; `key` matches only exactly (a provider
/// query parameter), so `keyterms` stays visible. Keys ending in `_env`
/// name an environment variable rather than holding a value and are
/// never secrets.
pub fn is_secret_key(key: &str) -> bool {
    let k = key.trim().to_ascii_lowercase().replace('-', "_");
    if k.ends_with("_env") {
        return false;
    }
    if k == "key" || k == "token" || k.ends_with("_token") {
        return true;
    }
    CONTAINS_MARKERS.iter().any(|m| k.contains(m))
}

/// Replace every value under a secret key with `placeholder`, whatever
/// its type, and blank credentials embedded in URL-shaped strings
/// (userinfo, secret-named query parameters). Empty strings and nulls
/// are left as they are so a consumer can still tell "not set" from
/// "set".
pub fn redact_in_place(value: &mut Value, placeholder: &str) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if is_secret_key(key) {
                    let unset =
                        matches!(v, Value::Null) || matches!(v, Value::String(s) if s.is_empty());
                    if !unset {
                        *v = Value::String(placeholder.to_string());
                    }
                } else {
                    redact_in_place(v, placeholder);
                }
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                redact_in_place(v, placeholder);
            }
        }
        Value::String(s) => {
            if let Some(clean) = redact_url(s, placeholder) {
                *s = clean;
            }
        }
        _ => {}
    }
}

/// Copy of `value` with secrets replaced by `placeholder`.
pub fn redacted(value: &Value, placeholder: &str) -> Value {
    let mut copy = value.clone();
    redact_in_place(&mut copy, placeholder);
    copy
}

/// A JSON parse error described by its kind and position only.
///
/// `serde_json`'s own message quotes the value it choked on (`invalid type:
/// string "...", expected u32`), and in a vendor's message that value can be
/// a transcript, the text being spoken, or kilobytes of base64 audio. This
/// keeps the part that locates the fault and drops the part that repeats
/// the payload, for logs and for errors that reach the health report.
pub fn json_error(e: &serde_json::Error) -> String {
    let kind = match e.classify() {
        serde_json::error::Category::Io => "read failed",
        serde_json::error::Category::Syntax => "not valid JSON",
        serde_json::error::Category::Data => "unexpected shape",
        serde_json::error::Category::Eof => "ended early",
    };
    format!("JSON {kind} at line {} column {}", e.line(), e.column())
}

/// Blank the userinfo and secret-named query parameters of a URL.
/// Returns `None` when `s` is not URL-shaped or carries nothing to
/// blank. No parser dependency: the shape needed here is
/// `scheme://[userinfo@]authority[/path][?query][#fragment]`.
pub fn redact_url(s: &str, placeholder: &str) -> Option<String> {
    let (scheme, rest) = s.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let mut changed = false;

    let authority = match authority.rsplit_once('@') {
        Some((_userinfo, host)) => {
            changed = true;
            format!("{placeholder}@{host}")
        }
        None => authority.to_string(),
    };

    let tail = match tail.find('?') {
        Some(q) => {
            let (path, query_and_fragment) = tail.split_at(q);
            let query_and_fragment = &query_and_fragment[1..];
            let (query, fragment) = match query_and_fragment.split_once('#') {
                Some((q, f)) => (q, Some(f)),
                None => (query_and_fragment, None),
            };
            let query = query
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((name, _)) if is_secret_key(name) => {
                        changed = true;
                        format!("{name}={placeholder}")
                    }
                    _ => pair.to_string(),
                })
                .collect::<Vec<_>>()
                .join("&");
            match fragment {
                Some(f) => format!("{path}?{query}#{f}"),
                None => format!("{path}?{query}"),
            }
        }
        None => tail.to_string(),
    };

    changed.then(|| format!("{scheme}://{authority}{tail}"))
}

#[cfg(test)]
#[path = "tests/secrets.rs"]
mod tests;
