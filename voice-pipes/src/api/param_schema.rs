//! Generic parameter schema for pipeline component nodes.
//!
//! Each component config struct implements [`ComponentParamSchema`] to declare:
//! - which parameters exist, with labels and help text
//! - when each parameter is visible (conditional on a sibling field value)
//! - whether each parameter can be changed at runtime without a worker restart
//! - value type and constraints for future editor support
//!
//! This removes all component-specific knowledge from the graph builder and
//! frontend renderer. Adding a new component type requires only implementing
//! the trait on its config struct — nothing else changes.
//!
//! # SDK re-exports
//! [`OnlyWhen`], [`ParamValueType`], [`ParamConstraints`], [`ParamDescriptor`],
//! [`ProviderSchema`], [`ParamValue`], and [`ParamError`] are canonical in
//! `voice-pipes-sdk` and re-exported here for backward compatibility.

// ── Schema types (canonical in voice-pipes-sdk) ───────────────
pub use voice_pipes_sdk::{
    OnlyWhen, ParamConstraints, ParamDescriptor, ParamError, ParamValue, ParamValueType,
    ProviderSchema,
};

// ── Framework-internal: component param schema ────────────────

/// Implemented by each component config struct.
///
/// The graph builder calls these methods generically without knowing which
/// component it is handling. Adding a new component only requires implementing
/// this trait on the new config struct.
pub trait ComponentParamSchema {
    /// Ordered list of parameter descriptors for this component.
    fn param_schema(&self) -> Vec<ParamDescriptor>;

    /// Current runtime values as a flat JSON map keyed by [`ParamDescriptor::key`].
    ///
    /// Secrets such as API keys MUST NOT appear here — use a `*_set` boolean
    /// placeholder instead.
    fn current_values(&self) -> serde_json::Map<String, serde_json::Value>;
}

#[cfg(test)]
#[path = "tests/param_schema.rs"]
mod tests;
