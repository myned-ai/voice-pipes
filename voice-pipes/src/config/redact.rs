//! Secret redaction for configs that cross the wire.
//!
//! Provider configs legitimately hold API keys locally, but nothing
//! outside the process needs the values: the worker registration
//! payload, `GET /debug/config`, and every orchestrator endpoint that
//! returns a pipeline config serve *shape*, not credentials. This
//! module blanks secret-like values in the serialized JSON at those
//! choke points, using the same key test and walker as the provider
//! frame log and the parameter schema
//! ([`voice_pipes_sdk::secrets`]).

use serde_json::Value;

pub use voice_pipes_sdk::secrets::is_secret_key;

/// Placeholder written over redacted values; the SDK's, so a `Secret`
/// and a redacted config read the same.
pub use voice_pipes_sdk::secrets::REDACTED;

/// Recursively blank secret-like values in `value`: any value under a
/// credential-shaped key, and credentials embedded in URL strings.
/// Empty strings and nulls stay as they are so a consumer can tell
/// "not set" from "set".
pub fn redact_secrets(value: &mut Value) {
    voice_pipes_sdk::secrets::redact_in_place(value, REDACTED);
}

/// Convenience: serialize `cfg` to JSON with secrets blanked.
pub fn to_redacted_json<T: serde::Serialize>(cfg: &T) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(cfg)?;
    redact_secrets(&mut value);
    Ok(value)
}

#[cfg(test)]
#[path = "tests/redact.rs"]
mod tests;
