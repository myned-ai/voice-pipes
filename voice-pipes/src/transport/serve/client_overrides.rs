//! What a connecting client may change about a realtime session.
//!
//! Merging `Configure.options` over the worker's `[realtime_provider]`
//! settings unchecked would let any client replace the operator's system
//! prompt (`instructions`) and drive the operator-billed provider session
//! with its own persona, VAD knobs and provider passthrough keys.
//!
//! The rule is an allowlist, `[realtime_provider] client_overrides`:
//!
//! - Keys on the list are honoured with client precedence.
//! - Keys not on the list are dropped before they reach the provider, logged
//!   once per session, and reported back in `session_ready.negotiated.
//!   ignored_options` so a client can see its request was not applied.
//! - `instructions`, when allowed, is **appended** to the operator's prompt
//!   under a labelled delimiter — never a replacement. The operator's text
//!   always comes first.
//! - Keys that voice-pipes consumes itself rather than forwarding
//!   (`client_tools`, the `output_chunking_*` transport knobs) are not
//!   provider options and are never filtered here; see [`ENGINE_KEYS`].
//!
//! The default list is the documented VAD tuning knobs. The wizard adds
//! `instructions` when the operator says connected clients may append to the
//! system prompt.

use std::collections::HashMap;

use serde_json::Value;

/// Option keys read by voice-pipes itself, never forwarded to a provider.
/// They bypass the allowlist so an operator cannot accidentally break the
/// transport by tightening `client_overrides`.
pub const ENGINE_KEYS: &[&str] = &[
    "client_tools",
    "output_chunking_enabled",
    "output_chunk_frame_ms",
];

/// Delimiter placed between the operator's instructions and an allowed
/// client addition. Named so a provider transcript makes the provenance of
/// each part obvious.
pub const CLIENT_INSTRUCTIONS_DELIMITER: &str =
    "\n\n[Additional instructions from the connected client]\n";

/// Default `client_overrides`: the VAD tuning knobs, which only change how
/// that client's own turns are endpointed.
pub fn default_allowlist() -> Vec<String> {
    [
        "vad_threshold",
        "vad_silence_ms",
        "vad_prefix_padding_ms",
        "vad_eagerness",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Result of applying the allowlist to a client's `options`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Filtered {
    /// Client options the provider path may read (allowlisted + engine keys).
    pub allowed: HashMap<String, Value>,
    /// Keys the client sent that were dropped, sorted for stable output.
    pub ignored: Vec<String>,
}

/// Split `client_opts` into what the provider path may see and what was
/// refused. Engine-consumed keys are always kept.
#[expect(
    clippy::implicit_hasher,
    reason = "callers pass serde_json-owned maps; no custom hasher exists"
)]
pub fn filter(client_opts: &HashMap<String, Value>, allowlist: &[String]) -> Filtered {
    let mut out = Filtered::default();
    for (k, v) in client_opts {
        let engine = ENGINE_KEYS.contains(&k.as_str());
        let listed = allowlist.iter().any(|a| a == k);
        if engine || listed {
            out.allowed.insert(k.clone(), v.clone());
        } else {
            out.ignored.push(k.clone());
        }
    }
    out.ignored.sort();
    out
}

/// Combine the operator's instructions with an (already allowlisted) client
/// addition. The operator's text is always first; an empty or whitespace
/// client string contributes nothing. Returns an empty string when neither
/// side has anything.
pub fn merge_instructions(operator: Option<&str>, client: Option<&str>) -> String {
    let operator = operator.map(str::trim).filter(|s| !s.is_empty());
    let client = client.map(str::trim).filter(|s| !s.is_empty());
    match (operator, client) {
        (Some(op), Some(cl)) => format!("{op}{CLIENT_INSTRUCTIONS_DELIMITER}{cl}"),
        (Some(op), None) => op.to_string(),
        (None, Some(cl)) => format!("{}{cl}", CLIENT_INSTRUCTIONS_DELIMITER.trim_start()),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
#[path = "tests/client_overrides.rs"]
mod tests;
